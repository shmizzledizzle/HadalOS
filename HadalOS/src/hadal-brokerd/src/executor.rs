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
use crate::session::strip_ansi;

/// Inline actions are meant to answer a question, not to do work. Anything
/// that could legitimately exceed this belongs in a transient unit.
const INLINE_TIMEOUT: Duration = Duration::from_secs(120);

/// Build logs are large and the interesting part is the end.
///
/// # Why this is not a round number
///
/// It was `256 * 1024`, chosen as a byte budget with no relationship to the
/// only budget that binds: the model's context window. That made `hadal
/// explain` fail on exactly the failures worth explaining. Measured against
/// the live endpoint on 2026-08-25 with a real 707 KB llama-cpp build log:
///
/// ```text
/// 262,177 bytes of log tail  ->  103,194 prompt tokens   (2.54 bytes/token)
/// ```
///
/// Build-log text is the worst case a tokeniser meets — absolute paths,
/// compiler flags, hex offsets, base64 — so it packs at roughly 2.5 bytes per
/// token where English prose runs 4. The old cap therefore consumed ~103k of a
/// 131,072-token window *before* the action protocol, the retrieval passages
/// and the reserved output were added, and the total landed either side of the
/// limit depending on the log. The observed failure was:
///
/// ```text
/// 400: maximum context length is 131072 tokens. However, you requested 2048
/// output tokens and your prompt contains at least 129025 input tokens
/// ```
///
/// So the cap is now derived from a stated token budget instead of guessed in
/// bytes. `BYTES_PER_TOKEN_FLOOR` is deliberately *below* the 2.54 measured
/// here: density varies per log, and being wrong in the other direction costs a
/// 400 at the moment the user is already dealing with a broken build.
///
/// **This is not the primary's window, and deliberately so.** The chain is
/// heterogeneous and the windows differ by a factor of thirty:
///
/// ```text
/// nvidia/nemotron-3-ultra-550b-a55b   262,144   (256k native)
/// llama-3.3-70b-versatile (Groq)      131,072
/// qwen-3-235b-a22b-… (Cerebras free)   65,536
/// hadal-reflex (local llama.cpp)        8,192
/// ```
///
/// One number has to be picked before the prompt is built, because the read is
/// truncated long before hadald chooses a link — and the two obvious choices
/// are both wrong. Sizing to the primary's 262,144 would build prompts only the
/// primary can serve, so every fallback would 400 on length and the chain would
/// collapse to "the primary or nothing" for exactly the large logs it exists to
/// survive. Sizing to the smallest link would cut the read to a few KB and
/// throw away the reason for having a 550B model at all.
///
/// So it is sized to **the widest window that a fallback can actually serve** —
/// Groq's 131,072. The primary is then under-used by half, which costs nothing
/// but unused window; Cerebras and the local reflex model still refuse large
/// prompts, and that refusal is handled rather than fatal: a context-length 400
/// advances the chain instead of ending it (`hadald/src/main.rs`). Raising this
/// to 262,144 is only correct once every link that must reliably answer can
/// hold it.
///
/// Note that the previous revision of this comment asserted the number *was*
/// the primary's window. That stopped being true on 2026-08-25, when
/// `llama-3.3-nemotron-super-49b-v1.5` reached end of support and the primary
/// moved to Nemotron 3 Ultra. The value did not need to change; the reason did.
const CONTEXT_TOKENS: usize = 131_072;
/// Reserved for the model's reply. Must match hadald's `max_tokens`.
const RESERVED_OUTPUT_TOKENS: usize = 2_048;
/// Reserved for `ACTION_PROTOCOL` (~1,900 bytes) plus the context wrapper.
const RESERVED_PROTOCOL_TOKENS: usize = 1_500;
/// Reserved for retrieval passages. `retrieve.rs` caps them to match.
const RESERVED_RETRIEVAL_TOKENS: usize = 24_000;
/// Slack, because none of the above is measured exactly and the failure is
/// one-sided: a little unused window costs nothing, one token over costs the
/// whole request.
const RESERVED_HEADROOM_TOKENS: usize = 8_000;
/// Conservative floor for dense log text; see the measurement above.
const BYTES_PER_TOKEN_FLOOR: usize = 2;

const MAX_READ: usize = (CONTEXT_TOKENS
    - RESERVED_OUTPUT_TOKENS
    - RESERVED_PROTOCOL_TOKENS
    - RESERVED_RETRIEVAL_TOKENS
    - RESERVED_HEADROOM_TOKENS)
    * BYTES_PER_TOKEN_FLOOR;

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
            text = salient_excerpt(&text, MAX_READ);
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
    Ok(if text.len() > MAX_READ { salient_excerpt(&text, MAX_READ) } else { text })
}

/// Lines within this many of a matched line are kept with it.
///
/// A compiler error is rarely self-contained: the invocation that produced it,
/// the `In file included from` chain above it and the `make: *** [target]`
/// below are what turn "cannot find -lssl" into a diagnosis. Two either side is
/// enough for that and cheap enough to afford at 65 prompt-tokens/sec.
const SALIENT_CONTEXT_LINES: usize = 2;

/// Reserved from the budget for the end of the log, whatever else is kept.
///
/// Portage prints its failure summary — the ebuild phase, the working
/// directory, the call stack — as the last thing it does, and none of those
/// lines contain a word `is_salient` matches. Losing them to a budget spent on
/// earlier compiler noise was the specific failure this fraction prevents.
const TAIL_SHARE: usize = 4;

/// Whether a line is worth spending context-window on.
///
/// Shares its vocabulary with `salient_lines` in `session.rs`, deliberately
/// and not by extraction: that one summarises *command output* to a handful of
/// lines with no context, this one excerpts a *log file* with its surroundings
/// intact. Merging them would mean one set of thresholds serving two budgets
/// that differ by two orders of magnitude.
fn is_salient(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("cannot")
        || lower.contains("no such")
        || lower.contains("unable to")
        || lower.contains("undefined reference")
        || lower.contains("not found")
        || lower.contains("missing")
        || lower.contains("required")
        || lower.contains("fatal")
        || lower.contains("warning:")
}

/// Excerpt the parts of a log worth sending, within `max` bytes.
///
/// `tail` was the right answer when the budget was 187 KB and the window was
/// 131k tokens. Locally it is not: at 65 prompt-tokens/sec the budget is a few
/// KB, and the last few KB of a failed build is usually linker noise and
/// `make` unwinding through directories long after the line that explains
/// anything. Truncation picks by *position*; this picks by *content*.
///
/// The shape of the result is deliberate:
///
/// * matched lines keep `SALIENT_CONTEXT_LINES` of surroundings, and
///   overlapping windows merge rather than repeat,
/// * gaps are marked, so the model is never shown two distant lines as though
///   they were adjacent — a spliced traceback invites a confident wrong answer,
/// * earliest matches win, because the first failure in a build log causes the
///   rest, and
/// * the tail is kept regardless, up to `1/TAIL_SHARE` of the budget.
///
/// Falls back to `tail` when nothing matches at all, which is the right answer
/// for a log that is not a failure log.
fn salient_excerpt(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let stripped: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();
    let hits: Vec<usize> =
        (0..stripped.len()).filter(|&i| is_salient(stripped[i].trim())).collect();
    if hits.is_empty() {
        return tail(text, max);
    }

    // Reserve the tail first so a flood of early matches cannot crowd it out.
    let tail_budget = max / TAIL_SHARE;
    let mut keep = vec![false; stripped.len()];
    let mut used = 0usize;
    for i in (0..stripped.len()).rev() {
        let cost = stripped[i].len() + 1;
        if used + cost > tail_budget {
            break;
        }
        used += cost;
        keep[i] = true;
    }

    // Then spend what is left on matches, earliest first, with context.
    'outer: for &h in &hits {
        let lo = h.saturating_sub(SALIENT_CONTEXT_LINES);
        let hi = (h + SALIENT_CONTEXT_LINES).min(stripped.len() - 1);
        for i in lo..=hi {
            if keep[i] {
                continue;
            }
            let cost = stripped[i].len() + 1;
            if used + cost > max {
                break 'outer;
            }
            used += cost;
            keep[i] = true;
        }
    }

    let mut out = String::with_capacity(used + 64);
    let mut gap = false;
    for i in 0..stripped.len() {
        if keep[i] {
            // Mark the seam. A reader who cannot see the elision will reason
            // about adjacency that is not there, and a model asked to explain
            // a spliced traceback will do it confidently.
            if gap {
                out.push_str("[... omitted ...]\n");
                gap = false;
            }
            out.push_str(stripped[i].trim_end());
            out.push('\n');
        } else {
            gap = true;
        }
    }
    out
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

    /// A failed build in miniature: the error is early, thousands of lines of
    /// successful compilation follow it, and Portage's summary is last. `tail`
    /// keeps the wrong end of this.
    fn build_log() -> String {
        let mut s = String::new();
        s.push_str("make: Entering directory '/var/tmp/portage/dev-libs/openssl-3.2.1/work'\n");
        s.push_str("gcc -O2 -pipe -c crypto/evp/evp_enc.c -o crypto/evp/evp_enc.o\n");
        s.push_str("/usr/bin/ld: cannot find -lssl: No such file or directory\n");
        s.push_str("collect2: error: ld returned 1 exit status\n");
        for i in 0..4000 {
            s.push_str(&format!("gcc -O2 -pipe -c src/file{i}.c -o src/file{i}.o\n"));
        }
        s.push_str(" * ERROR: dev-libs/openssl-3.2.1::gentoo failed (compile phase)\n");
        s.push_str(" * Call stack: ebuild.sh, line 136: Called src_compile\n");
        s.push_str(" * Working directory: /var/tmp/portage/dev-libs/openssl-3.2.1/work\n");
        s
    }

    #[test]
    fn an_excerpt_keeps_the_first_error_that_a_tail_would_lose() {
        let log = build_log();
        let budget = 4_000;

        assert!(
            !tail(&log, budget).contains("cannot find -lssl"),
            "precondition: the tail must genuinely miss the root cause"
        );

        let excerpt = salient_excerpt(&log, budget);
        assert!(excerpt.contains("cannot find -lssl"), "the root cause must survive:\n{excerpt}");
        assert!(excerpt.contains("ERROR: dev-libs/openssl"), "so must Portage's summary");
        assert!(excerpt.contains("Working directory"), "and the tail that names the phase");
        assert!(excerpt.len() <= budget, "budget exceeded: {} > {budget}", excerpt.len());
    }

    /// The compiler invocation above an error is half the diagnosis.
    #[test]
    fn an_excerpt_keeps_the_lines_around_a_match() {
        let excerpt = salient_excerpt(&build_log(), 4_000);
        assert!(
            excerpt.contains("crypto/evp/evp_enc.c"),
            "the line above the error is context, not noise:\n{excerpt}"
        );
    }

    /// Distant lines must never be presented as adjacent.
    #[test]
    fn elisions_are_marked() {
        let excerpt = salient_excerpt(&build_log(), 4_000);
        assert!(excerpt.contains("[... omitted ...]"), "gaps must be visible:\n{excerpt}");
    }

    /// Not every read is a failure log. With nothing to match on, positional
    /// truncation is still the right answer rather than an empty result.
    #[test]
    fn a_log_with_no_errors_falls_back_to_the_tail() {
        let plain = "all quiet on this line\n".repeat(500);
        let excerpt = salient_excerpt(&plain, 200);
        assert!(excerpt.contains("all quiet"), "must not come back empty");
        assert!(excerpt.contains("truncated"), "should be the tail path: {excerpt}");
    }

    /// Colour escapes are stripped, since they cost tokens and mean nothing.
    #[test]
    fn ansi_colour_does_not_reach_the_model() {
        let log = "\u{1b}[31m * ERROR: package failed (compile phase)\u{1b}[0m\n".repeat(50);
        let excerpt = salient_excerpt(&log, 500);
        assert!(excerpt.contains("ERROR: package failed"));
        assert!(!excerpt.contains('\u{1b}'), "escape sequences leaked: {excerpt:?}");
    }

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
