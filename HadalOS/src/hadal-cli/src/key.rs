//! `hadal key` — acquire and install the upstream API key.
//!
//! hadald will not start without a model id and, for a remote upstream, a
//! credential. Both live in `/etc/hadal`, which is `0700` and owned by `hadal`,
//! so writing them needs privilege the CLI deliberately does not have. This
//! command is the bridge: it explains where to get a key, opens the page, takes
//! the paste, and hands the write to `pkexec` — one prompt, at the end, for the
//! one operation that needs it.
//!
//! # Why this does not go through the broker
//!
//! The broker is the component that holds privilege, so a `SetUpstreamKey`
//! capability would be the consistent-looking choice. It is the wrong one. The
//! broker's whole design argument is that its action enum is closed and that no
//! model generation can reach anything outside it; "write these bytes to a
//! privileged file" is precisely the shape that argument exists to keep off the
//! list. This command is interactive, user-initiated, and reachable only from a
//! terminal — there is no path to it from a generation because it is not a
//! capability at all.
//!
//! # Why it runs before the bus connection
//!
//! `main` connects to the system bus before dispatching most subcommands. This
//! one cannot wait for that: hadal-brokerd `Requires=hadald.service`, hadald
//! will not start without the key, so the broker is guaranteed to be down at
//! the exact moment someone runs this. A `key` command that needed the broker
//! running would be unreachable in the only situation it is for.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const CONFIG_DIR: &str = "/etc/hadal";
const KEY_FILE: &str = "/etc/hadal/upstream.key";
const ENV_FILE: &str = "/etc/hadal/hadald.env";

/// Where NVIDIA hands out keys for the endpoint `config.rs` defaults to.
///
/// Named as a constant beside the default upstream it belongs to, because the
/// two have to change together: pointing hadald at a different provider makes
/// these directions wrong, and wrong directions are worse than none.
const SIGNUP_URL: &str = "https://build.nvidia.com/";
const DEFAULT_UPSTREAM: &str = "https://integrate.api.nvidia.com/v1";

/// The model asked of `DEFAULT_UPSTREAM`. Third member of the same set: an id
/// is only meaningful at the endpoint that serves it, and this one 404s
/// everywhere else.
const DEFAULT_MODEL: &str = "nvidia/nemotron-3-ultra-550b-a55b";

/// What NVIDIA's keys have looked like. Advisory only.
///
/// Checked to catch the common paste mistakes — a URL, an empty clipboard, the
/// page title — not to enforce a format. A provider that changes its prefix
/// must not make this command refuse a valid key, so a mismatch warns and
/// continues.
const EXPECTED_PREFIX: &str = "nvapi-";

pub fn run() -> Res<()> {
    if !io::stdin().is_terminal() {
        return Err("hadal key is interactive; run it from a terminal".into());
    }

    println!();
    println!("  Setting up hadald's upstream credential.");
    println!();
    println!("  hadald routes to a model endpoint. The default is NVIDIA's,");
    println!("  which needs an API key. Nothing is sent anywhere until hadald");
    println!("  runs, and the key is written to {KEY_FILE}");
    println!("  as mode 0600 owned by hadal — never to your shell history and");
    println!("  never into the service's environment, where /proc would expose");
    println!("  it.");
    println!();

    match open_browser(SIGNUP_URL) {
        Ok(()) => println!("  Opened {SIGNUP_URL} in your browser."),
        // Not fatal. A headless box, no xdg-open, or no browser installed are
        // all ordinary; the URL is printed either way and that is the part
        // that matters.
        Err(e) => println!("  Could not open a browser ({e}). Go to {SIGNUP_URL}"),
    }

    println!();
    println!("  On that page:");
    println!("    1. Sign in, or create an account.");
    println!("    2. Pick any model — the key is account-wide, not per model.");
    println!("    3. Click 'Get API Key', then 'Generate Key'.");
    println!("    4. Copy it. It is shown once.");
    println!();

    let key = prompt_secret("  Paste the key here (it will not echo): ")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("no key entered; nothing was written".into());
    }
    if key.contains(char::is_whitespace) {
        return Err("that contains whitespace — it looks like prose, not a key".into());
    }
    if !key.starts_with(EXPECTED_PREFIX) {
        // A warning, not a refusal: see EXPECTED_PREFIX.
        println!();
        println!("  Note: that does not start with {EXPECTED_PREFIX:?}, which is what");
        println!("  NVIDIA keys have looked like. Continuing anyway — if hadald");
        println!("  reports 401 afterwards, this is the first thing to re-check.");
    }
    // Masked confirmation. A pasted key that silently lost a character to a
    // terminal is the failure this catches, and echoing the whole thing to
    // catch it would defeat the point of not echoing it.
    println!();
    println!("  Got {} characters, {}.", key.len(), mask(&key));

    // The line above ends "Got 64 characters, nvapi-…f3a." — which reads like a
    // question, and on this machine it was answered like one: `yes` was taken
    // as the model id, written to /etc/hadal/hadald.env, and asked of NVIDIA on
    // every request for weeks. The daemon started, `hadal status` answered, and
    // only the generations failed, with an HTTP error that looked like every
    // other HTTP error. Nothing between the typo and the symptom mentioned the
    // model id.
    //
    // So the answer is checked here rather than at the far end of a request:
    // this is the only moment where the person who can fix it is present.
    println!();
    println!("  Next is the model id — not a yes/no question.");
    let model = loop {
        let answer = prompt_line("  Model id [nvidia/nemotron-3-ultra-550b-a55b]: ")?;
        match answer.trim() {
            "" => break DEFAULT_MODEL.to_string(),
            chosen if looks_like_a_confirmation(chosen) => {
                println!();
                println!("  {chosen:?} is an answer to a yes/no question, and this is not one.");
                println!("  Press Enter to take the default, or paste a model id such as");
                println!("  {DEFAULT_MODEL:?}.");
                println!();
            }
            chosen => break chosen.to_string(),
        }
    };

    install(&key, &model)?;

    println!();
    println!("  Written:");
    println!("    {KEY_FILE}   (0600 hadal:hadal)");
    println!("    {ENV_FILE}    (0600 hadal:hadal)");
    println!();
    println!("  Start it with:");
    println!("    sudo systemctl start hadald hadal-brokerd");
    println!();
    println!("  Then `hadal status` should answer.");
    println!();
    Ok(())
}

/// First four and last four, with the middle elided.
///
/// Short keys are elided entirely rather than shown — a key short enough that
/// eight characters would reveal most of it is a key worth not printing.
/// Whether an answer is a yes/no reply rather than a model id.
///
/// Deliberately narrow. The cost of a false positive is a re-prompt the user
/// can override by pasting the same thing again; the cost of a false negative
/// is a daemon that starts, passes `hadal status`, and fails every generation.
/// But a model id is a vendor's string and this must not become a filter on
/// what vendors are allowed to call things — so it matches exactly the handful
/// of words that mean "yes" or "no" and nothing else. No length rule, no
/// character-class rule, no "must contain a slash": `gpt-oss-120b` has no
/// slash, and next year's model may have no hyphen either.
fn looks_like_a_confirmation(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "n" | "yes" | "no" | "yeah" | "yep" | "nope" | "ok" | "okay" | "sure"
    )
}

fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 16 {
        return "too short to show safely".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("starting {head}… and ending …{tail}")
}

fn open_browser(url: &str) -> Res<()> {
    let status = Command::new("xdg-open")
        .arg(url)
        // Silenced: xdg-open is a shell script and several backends chatter on
        // stderr even when they succeed, which would land in the middle of the
        // directions below.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("xdg-open exited {status}").into());
    }
    Ok(())
}

fn prompt_line(prompt: &str) -> Res<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Read a line with terminal echo disabled.
///
/// `stty` rather than a crate. The alternative is `libc` or `rpassword` for one
/// ioctl on one line of input, and this binary's dependency list is otherwise
/// four things it genuinely needs.
///
/// Echo is restored by `EchoGuard` on the way out, including on the error paths
/// above — leaving a terminal with echo off is a far worse outcome than
/// failing to read a key, because the shell it returns to appears broken.
fn prompt_secret(prompt: &str) -> Res<String> {
    print!("{prompt}");
    io::stdout().flush()?;

    let guard = EchoGuard::off();
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line);
    drop(guard);
    // The newline the user typed was swallowed with the echo.
    println!();
    read?;
    Ok(line)
}

/// Restores terminal echo when dropped.
struct EchoGuard {
    /// False when `stty -echo` failed, in which case there is nothing to undo
    /// and the key was echoed. Not an error: the key still gets installed, and
    /// refusing to proceed because a terminal would not turn echo off helps
    /// nobody.
    disabled: bool,
}

impl EchoGuard {
    fn off() -> Self {
        EchoGuard { disabled: stty(&["-echo"]).is_ok() }
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if self.disabled {
            let _ = stty(&["echo"]);
        }
    }
}

fn stty(args: &[&str]) -> Res<()> {
    // `-F /dev/tty` rather than inheriting stdin: it works when stdin has been
    // redirected but a controlling terminal still exists.
    let status = Command::new("stty")
        .arg("-F")
        .arg("/dev/tty")
        .args(args)
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err("stty failed".into());
    }
    Ok(())
}

/// Stage both files as the user, then move them into place with one pkexec.
///
/// Staged rather than piped because `pkexec` does not reliably forward stdin,
/// and passed as file paths rather than arguments because an API key in argv is
/// readable from `/proc/<pid>/cmdline` by anyone on the machine for as long as
/// the call runs. The staging files are created 0600 in the user's own runtime
/// directory and removed on every path out, including failure.
fn install(key: &str, model: &str) -> Res<()> {
    let staging = Staging::new()?;
    // Trailing newline: `read_key` in hadald trims, but a file without one is
    // a nuisance to every other tool that reads it.
    staging.write("upstream.key", &format!("{key}\n"))?;
    staging.write(
        "hadald.env",
        &format!(
            "# Written by `hadal key`.\n\
             # HADAL_MODEL is the one setting that varies per machine.\n\
             # HADAL_UPSTREAM overrides the unit's default; remove it to use that.\n\
             HADAL_MODEL={model}\n\
             HADAL_UPSTREAM={DEFAULT_UPSTREAM}\n"
        ),
    )?;

    println!();
    println!("  Installing into {CONFIG_DIR} — this needs authentication.");

    // One invocation, so there is one prompt rather than three. `sh -c` with
    // the paths as positional arguments rather than interpolated into the
    // script: a staging path containing a quote would otherwise be a shell
    // injection into a root command.
    let status = Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(
            "install -d -m 0700 -o hadal -g hadal /etc/hadal && \
             install -m 0600 -o hadal -g hadal \"$1\" /etc/hadal/upstream.key && \
             install -m 0600 -o hadal -g hadal \"$2\" /etc/hadal/hadald.env",
        )
        .arg("sh")
        .arg(staging.path("upstream.key"))
        .arg(staging.path("hadald.env"))
        .status()
        .map_err(|e| format!("could not run pkexec: {e}"))?;

    if !status.success() {
        // 126 is pkexec's "not authorized"; 127 is "dismissed". Both are the
        // user declining, which is not a fault to report as one.
        return Err(match status.code() {
            Some(126) | Some(127) => "not authorized; nothing was written".into(),
            _ => format!("pkexec exited {status}; nothing was written"),
        }
        .into());
    }
    Ok(())
}

/// A private directory for the two files between writing and installing them.
///
/// Under `XDG_RUNTIME_DIR` when there is one — that is `0700`, user-owned, and
/// on tmpfs, so a key staged there never reaches a disk. `/tmp` is the fallback
/// and is world-readable, which is why the files are `0600` regardless.
struct Staging {
    dir: PathBuf,
}

impl Staging {
    fn new() -> Res<Self> {
        let base = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        // Unique per *instance*, not per process. The name used to be
        // `hadal-key.<pid>`, and `Drop` shreds the whole directory — so two
        // live `Staging` values in one process shared a directory and the
        // first one dropped deleted the other's key from under it. The test
        // suite reproduced this immediately, because the harness runs tests as
        // threads of a single process, and it failed only sometimes, which is
        // the worst way for a file-deletion bug to fail.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = base.join(format!(
            "hadal-key.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        // `create_dir`, not `create_dir_all`: this must fail rather than adopt
        // a directory that already exists. A pid is reused after a reboot, so
        // an interrupted run can leave one behind — and a directory this
        // process did not create is one whose mode and ownership it did not
        // choose, which is not somewhere to write an API key.
        fs::create_dir(&dir).map_err(|e| {
            format!("could not create a private staging directory at {}: {e}", dir.display())
        })?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Staging { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write(&self, name: &str, contents: &str) -> Res<()> {
        let path = self.path(name);
        fs::write(&path, contents)?;
        // After writing, not before: `fs::write` creates with 0644 and the
        // window between the two is the only moment the key is readable.
        // Narrow, and closed here rather than left open.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Best effort, but it does run on the failure paths too — a declined
        // polkit prompt must not leave the key sitting in a temp file.
        let _ = shred(&self.dir);
    }
}

/// Overwrite before unlinking.
///
/// tmpfs makes this close to theatre and it costs nothing; on the `/tmp`
/// fallback, where `/tmp` may be a real filesystem, it is the difference
/// between a deleted file and a recoverable one.
fn shred(dir: &Path) -> io::Result<()> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(meta) = fs::metadata(&path) {
                let _ = fs::write(&path, vec![0u8; meta.len() as usize]);
            }
            let _ = fs::remove_file(&path);
        }
    }
    fs::remove_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two staging areas at once must not share a directory, because `Drop`
    /// shreds the directory and not just the files it wrote. Pinned directly
    /// rather than left to the harness: as a scheduling accident this
    /// reproduced perhaps one run in three, which is exactly often enough to
    /// be dismissed as a flake.
    #[test]
    fn two_staging_areas_do_not_share_a_directory() {
        let a = Staging::new().expect("first staging area");
        let b = Staging::new().expect("second staging area");
        assert_ne!(a.dir, b.dir, "staging directories must be distinct");

        a.write("upstream.key", "key-a\n").expect("write a");
        b.write("upstream.key", "key-b\n").expect("write b");

        // Dropping one must leave the other's key intact and readable.
        drop(a);
        assert_eq!(
            fs::read_to_string(b.path("upstream.key")).expect("b's key must survive a's drop"),
            "key-b\n"
        );
    }

    /// The live misconfiguration this check exists for: `HADAL_MODEL=yes` in
    /// /etc/hadal/hadald.env, from answering the masked-key line as though it
    /// had asked a question.
    #[test]
    fn a_yes_no_answer_is_not_accepted_as_a_model_id() {
        for answer in ["y", "n", "yes", "no", "YES", " Yes ", "ok", "sure", "nope"] {
            assert!(looks_like_a_confirmation(answer), "{answer:?} should be re-prompted");
        }
    }

    /// The check must not become an opinion about vendors' naming. These are
    /// all real ids, and none of them share a shape.
    #[test]
    fn real_model_ids_are_left_alone() {
        for id in [
            "nvidia/nemotron-3-ultra-550b-a55b",
            "llama-3.3-70b-versatile",
            "qwen-3-235b-a22b-instruct-2507",
            "gpt-oss-120b",
            "hadal-reflex",
            "o3",
        ] {
            assert!(!looks_like_a_confirmation(id), "{id:?} is a real model id");
        }
    }

    #[test]
    fn a_key_is_masked_to_its_ends() {
        let masked = mask("nvapi-abcdefghijklmnop");
        assert!(masked.contains("nvap"), "{masked}");
        assert!(masked.contains("mnop"), "{masked}");
        // The point of masking is that the middle is absent.
        assert!(!masked.contains("efghij"), "{masked}");
    }

    #[test]
    fn a_short_key_is_not_shown_at_all() {
        // Eight characters of a twelve-character key is most of it.
        assert_eq!(mask("nvapi-abcdef"), "too short to show safely");
        assert_eq!(mask(""), "too short to show safely");
    }

    #[test]
    fn staging_files_are_not_readable_by_anyone_else() {
        let staging = Staging::new().expect("staging dir");
        staging.write("upstream.key", "nvapi-secret\n").expect("write");
        let mode = fs::metadata(staging.path("upstream.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "staged key is {:o}", mode & 0o777);
        let dir_mode = fs::metadata(&staging.dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700, "staging dir is {:o}", dir_mode & 0o777);
    }

    #[test]
    fn staging_is_removed_when_dropped_even_though_the_key_was_never_installed() {
        let path = {
            let staging = Staging::new().expect("staging dir");
            staging.write("upstream.key", "nvapi-secret\n").expect("write");
            let path = staging.dir.clone();
            assert!(path.exists());
            path
        };
        // This is the declined-polkit path: nothing was installed, and the key
        // must not be left behind.
        assert!(!path.exists(), "staging survived the drop");
    }
}
