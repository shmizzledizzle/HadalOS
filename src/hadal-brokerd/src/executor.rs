//! The privileged side. Everything here runs only after polkit has agreed.
//!
//! Two rules hold throughout:
//!
//! 1. **No shell, ever.** Every subprocess is an explicit program plus an argv
//!    vector. There is no string concatenated into `sh -c`, which is why the
//!    validators in `action` are sufficient rather than merely helpful.
//! 2. **Prefer an API to a subprocess.** systemd operations go over D-Bus to
//!    `org.freedesktop.systemd1`, so restarting a unit involves no process
//!    execution at all.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use zbus::Connection;
use zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::action::{Action, ConfigChange, EmergeMode, PathPolicy};

/// Inline actions are meant to answer a question, not to do work. Anything
/// that could legitimately exceed this belongs in a transient unit.
const INLINE_TIMEOUT: Duration = Duration::from_secs(120);

/// Build logs are large and the interesting part is the end.
const MAX_READ: usize = 256 * 1024;

pub type ActionResult = HashMap<String, OwnedValue>;

#[derive(Debug)]
pub struct ExecError(pub String);

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err(msg: impl Into<String>) -> ExecError {
    ExecError(msg.into())
}

fn ok_text(kind: &str, text: String) -> Result<ActionResult, ExecError> {
    let mut m = ActionResult::new();
    m.insert("kind".into(), Value::from(kind).try_into().unwrap());
    m.insert("text".into(), Value::from(text).try_into().unwrap());
    Ok(m)
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn sub_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn load_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn description(&self) -> zbus::Result<String>;
}

pub struct Executor {
    conn: Connection,
    paths: PathPolicy,
    portage_conf: std::path::PathBuf,
}

/// Optional site override for which roots `read-path` may reach.
const READ_ROOTS_CONF: &str = "/etc/hadalos/read-roots.conf";

impl Executor {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn,
            paths: load_path_policy(),
            portage_conf: std::path::PathBuf::from("/etc/portage"),
        }
    }

    pub async fn run(&self, action: &Action) -> Result<ActionResult, ExecError> {
        match action {
            Action::ReadJournal { unit, boot, lines } => {
                let mut args: Vec<String> = vec![
                    "--no-pager".into(),
                    "--output=short-iso".into(),
                    "-b".into(),
                    boot.as_journalctl_arg().into(),
                    "-n".into(),
                    lines.to_string(),
                ];
                if let Some(u) = unit {
                    // Passed as a separate argv element after `-u`; the value
                    // has already been proven to contain no whitespace, no
                    // leading '-', and no path separator.
                    args.push("-u".into());
                    args.push(u.as_str().to_string());
                }
                let out = self.spawn("/usr/bin/journalctl", &args).await?;
                ok_text("journal", out)
            }

            Action::ReadPortageLog { path } | Action::ReadPath { path } => {
                let resolved = self.paths.resolve(path).map_err(|e| err(e.to_string()))?;
                let text = read_tail(&resolved).await?;
                ok_text("file", text)
            }

            Action::QueryPackage { atom } => {
                // Keep the underlying error rather than replacing it: equery
                // failing to run and equery running but erroring are different
                // problems, and flattening them to one message sends people
                // to install a package they already have.
                let out = self
                    .spawn("/usr/bin/equery", &["--quiet".into(), "list".into(), atom.as_str().into()])
                    .await
                    .map_err(|e| err(format!("{e} (is app-portage/gentoolkit installed?)")))?;
                ok_text("package", out)
            }

            Action::UnitStatus { unit } => {
                let manager = SystemdManagerProxy::new(&self.conn)
                    .await
                    .map_err(|e| err(format!("systemd unavailable: {e}")))?;
                let path = manager
                    .get_unit(unit.as_str())
                    .await
                    .map_err(|e| err(format!("no such unit: {e}")))?;
                let u = SystemdUnitProxy::builder(&self.conn)
                    .path(path)
                    .map_err(|e| err(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| err(e.to_string()))?;

                let text = format!(
                    "{}\n  load:   {}\n  active: {} ({})",
                    u.description().await.unwrap_or_default(),
                    u.load_state().await.unwrap_or_default(),
                    u.active_state().await.unwrap_or_default(),
                    u.sub_state().await.unwrap_or_default(),
                );
                ok_text("unit-status", text)
            }

            Action::EmergePretend { atoms } => {
                let mut args: Vec<String> =
                    vec!["--pretend".into(), "--verbose".into(), "--color=n".into()];
                // `--` terminates option parsing. Atoms cannot begin with '-'
                // by validation, so this is belt and braces — but it costs
                // nothing and removes the class of bug entirely.
                args.push("--".into());
                args.extend(atoms.iter().map(|a| a.as_str().to_string()));
                let out = self.spawn("/usr/bin/emerge", &args).await?;
                ok_text("emerge-pretend", out)
            }

            Action::RestartUnit { unit } => {
                let manager = SystemdManagerProxy::new(&self.conn)
                    .await
                    .map_err(|e| err(format!("systemd unavailable: {e}")))?;
                let job = manager
                    .restart_unit(unit.as_str(), "replace")
                    .await
                    .map_err(|e| err(format!("restart failed: {e}")))?;
                ok_text("job", format!("restarting {} (job {})", unit.as_str(), job.as_str()))
            }

            Action::EmergeApply { atoms, mode } => self.emerge_apply(atoms, *mode).await,

            Action::WriteConfig { change } => self.write_config(change).await,

            // Deliberately unimplemented rather than quietly routed around.
            // hadald and this process share a network namespace with no route
            // out; honouring this would mean either weakening that isolation
            // or pretending to. The designed answer is a socket-proxy unit
            // pinned to one upstream, which does not exist yet.
            Action::NetworkLookup { .. } => Err(err(
                "network lookup is not available: hadal-brokerd runs without network access. \
                 See ARCHITECTURE.md — the egress proxy is not yet implemented.",
            )),
        }
    }

    /// A real world update can run for hours, so it must not be an inline
    /// D-Bus call. Handing it to a transient systemd unit gives correct
    /// lifecycle, journal capture, and cgroup accounting for free — and lets
    /// the client follow progress through the same `unit-status` capability
    /// it already has, rather than inventing a second progress channel.
    async fn emerge_apply(
        &self,
        atoms: &[crate::action::Atom],
        mode: EmergeMode,
    ) -> Result<ActionResult, ExecError> {
        let unit_name = format!("hadal-emerge-{}", std::process::id());

        let mut args: Vec<String> = vec![
            format!("--unit={unit_name}"),
            "--collect".into(),
            "--description=Package operation proposed by Hadal".into(),
            "--property=Type=oneshot".into(),
            "/usr/bin/emerge".into(),
            "--color=n".into(),
        ];
        match mode {
            EmergeMode::Install => {}
            EmergeMode::Oneshot => args.push("--oneshot".into()),
            EmergeMode::Depclean => args.push("--depclean".into()),
        }
        args.push("--".into());
        args.extend(atoms.iter().map(|a| a.as_str().to_string()));

        self.spawn("/usr/bin/systemd-run", &args).await?;

        let mut m = ActionResult::new();
        m.insert("kind".into(), Value::from("job").try_into().unwrap());
        m.insert("unit".into(), Value::from(format!("{unit_name}.service")).try_into().unwrap());
        m.insert(
            "text".into(),
            Value::from(format!(
                "started {unit_name}.service — follow it with journalctl -fu {unit_name}"
            ))
            .try_into()
            .unwrap(),
        );
        Ok(m)
    }

    /// Config changes append to files HadalOS owns, never to files the user
    /// or Portage maintains. `/etc/portage/package.use/hadalos` is a
    /// package.use directory member, so removing it cleanly reverts every
    /// change Hadal has ever made — which is the property that makes granting
    /// this capability reasonable at all.
    async fn write_config(&self, change: &ConfigChange) -> Result<ActionResult, ExecError> {
        let (subdir, line) = match change {
            ConfigChange::PortageUse { atom, flags } => {
                let f: Vec<&str> = flags.iter().map(|x| x.as_str()).collect();
                ("package.use", format!("{} {}", atom.as_str(), f.join(" ")))
            }
            ConfigChange::PortageAcceptKeywords { atom, keyword } => {
                ("package.accept_keywords", format!("{} {}", atom.as_str(), keyword.as_str()))
            }
            ConfigChange::PortageMask { atom } => ("package.mask", atom.as_str().to_string()),
        };

        let dir = self.portage_conf.join(subdir);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| err(format!("cannot create {}: {e}", dir.display())))?;

        let file = dir.join("hadalos");
        let mut existing = tokio::fs::read_to_string(&file).await.unwrap_or_default();
        if existing.lines().any(|l| l.trim() == line) {
            return ok_text("config", format!("already present in {}: {line}", file.display()));
        }
        if existing.is_empty() {
            existing.push_str("# Managed by HadalOS. Delete this file to revert every change.\n");
        }
        existing.push_str(&line);
        existing.push('\n');

        tokio::fs::write(&file, existing)
            .await
            .map_err(|e| err(format!("cannot write {}: {e}", file.display())))?;

        ok_text("config", format!("wrote to {}: {line}", file.display()))
    }

    /// No shell. No inherited environment. Bounded time and output.
    async fn spawn(&self, program: &str, args: &[String]) -> Result<String, ExecError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C.UTF-8")
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| err(format!("cannot run {program}: {e}")))?;

        let mut stdout = child.stdout.take().ok_or_else(|| err("no stdout"))?;
        let mut stderr = child.stderr.take().ok_or_else(|| err("no stderr"))?;

        let collect = async {
            let mut o = Vec::new();
            let mut e = Vec::new();
            let _ = tokio::try_join!(stdout.read_to_end(&mut o), stderr.read_to_end(&mut e));
            let status = child.wait().await;
            (o, e, status)
        };

        let (o, e, status) = tokio::time::timeout(INLINE_TIMEOUT, collect)
            .await
            .map_err(|_| err(format!("{program} timed out after {INLINE_TIMEOUT:?}")))?;

        let status = status.map_err(|x| err(format!("{program} failed: {x}")))?;
        let mut text = String::from_utf8_lossy(&o).into_owned();
        if !status.success() {
            let stderr_text = String::from_utf8_lossy(&e);
            // Non-zero exit is frequently the *answer* (emerge --pretend on a
            // blocked package), so the output is returned rather than
            // discarded in favour of the exit code.
            text.push_str("\n--- stderr ---\n");
            text.push_str(&stderr_text);
        }
        if text.len() > MAX_READ {
            text = tail(&text, MAX_READ);
        }
        Ok(text)
    }
}

/// One absolute path per line; `#` comments and blanks ignored. Absent file
/// means the built-in defaults.
///
/// Widening this cannot expose secrets: the permanent denylist in
/// `action::PathPolicy::resolve` is consulted after canonicalisation and
/// independently of these roots. That is the whole reason it is safe to make
/// this configurable at all.
fn load_path_policy() -> PathPolicy {
    let Ok(text) = std::fs::read_to_string(READ_ROOTS_CONF) else {
        return PathPolicy::default();
    };

    let roots: Vec<std::path::PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| {
            if std::path::Path::new(l).is_absolute() {
                true
            } else {
                tracing::warn!("{READ_ROOTS_CONF}: ignoring non-absolute root {l:?}");
                false
            }
        })
        .map(std::path::PathBuf::from)
        .collect();

    if roots.is_empty() {
        tracing::warn!("{READ_ROOTS_CONF} has no usable entries; using defaults");
        return PathPolicy::default();
    }

    tracing::info!("read roots from {READ_ROOTS_CONF}: {} entries", roots.len());
    PathPolicy::with_roots(roots)
}

async fn read_tail(path: &Path) -> Result<String, ExecError> {
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    let text = String::from_utf8_lossy(&data).into_owned();
    Ok(if text.len() > MAX_READ { tail(&text, MAX_READ) } else { text })
}

/// Keep the end. In a build log that is where the error is; in anything else
/// truncation has to pick a side and the end is the better default.
fn tail(text: &str, max: usize) -> String {
    let start = text.len().saturating_sub(max);
    // Do not slice through a multi-byte character.
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    format!("[... {} bytes truncated ...]\n{}", start, &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_and_respects_char_boundaries() {
        let s = "α".repeat(1000); // 2 bytes each
        let t = tail(&s, 100);
        assert!(t.ends_with('α'));
        assert!(t.contains("truncated"));
        // Would have panicked on a bad slice; reaching here is the assertion.
        assert!(t.len() < s.len());
    }

    #[test]
    fn tail_is_a_noop_shaped_result_for_short_input() {
        let t = tail("short", 100);
        assert!(t.ends_with("short"));
    }
}
