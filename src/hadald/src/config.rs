//! Configuration, and the one place a secret is read.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:11434";
pub const DEFAULT_UPSTREAM: &str = "https://integrate.api.nvidia.com/v1";
/// 2048 dimensions, verified against the live endpoint.
///
/// Not `nv-embedqa-e5-v5`, which was the obvious pick and is unusable here: it
/// caps inputs at 512 tokens, and 79% of the Gentoo corpus chunks exceed that.
/// Retrieval models are frequently short-context — check the limit against real
/// chunk sizes before choosing one, not after.
///
/// Changing this invalidates any existing index: vectors are not comparable
/// across models, and the dimension changes too. `manifest.json` records which
/// model built the current index.
pub const DEFAULT_EMBED_MODEL: &str = "nvidia/nemotron-3-embed-1b";

/// Whether the configured upstream is on this machine.
///
/// This exists because `llama-server` speaks the same OpenAI-compatible API as
/// the remote endpoint, so hosting a model locally is a change of *address*,
/// not of protocol. Three things then have to become conditional, and each was
/// unconditionally wrong for a local upstream before this type existed:
///
/// 1. **The API key.** A local server needs none, and `read_key` refuses to
///    start without one.
/// 2. **The egress log.** `/var/log/hadal/egress.log` answers "what left this
///    machine". Writing a line for a request to loopback makes it answer
///    something else, and quietly — the failure mode this project keeps
///    finding.
/// 3. **The startup warning.** "This daemon sends system logs to a third
///    party" is false when pointed at loopback, and a warning that cries wolf
///    is a warning people learn to skip.
///
/// This is the mechanism `docs/tier-routing.md` needs and does not have. That
/// document routes on whether data *must stay here*; it cannot route anywhere
/// until there are two places to route to, and hadald has had exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Local,
    Remote,
}

/// The host part of a URL, without pulling in a URL parser for one field.
///
/// Handles `scheme://user:pass@host:port/path` and `[::1]:port`. Returns `None`
/// on anything it does not understand, which the caller must treat as remote.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    // Userinfo may itself contain '@' in a password; the host is after the last.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if authority.is_empty() {
        return None;
    }
    if let Some(after) = authority.strip_prefix('[') {
        // Bracketed IPv6 literal: [::1]:11434
        return after.split_once(']').map(|(h, _)| h).filter(|h| !h.is_empty());
    }
    authority.split(':').next().filter(|h| !h.is_empty())
}

impl Locality {
    /// Derived from the upstream URL, never from a flag.
    ///
    /// A flag could disagree with the URL, and the URL is what decides where
    /// the bytes actually go. `sonar`'s `contends_with_display` makes the same
    /// choice for the same reason.
    ///
    /// **Unparseable, or a name that merely resolves to loopback, is treated as
    /// remote.** A hostname in `/etc/hosts` pointing at 127.0.0.1 is classified
    /// `Remote` here, which costs a needless key and a needless egress line —
    /// the harmless direction. Guessing `Local` and being wrong would suppress
    /// the record of a real egress, which is the direction that cannot be
    /// allowed to happen silently. Same shape as tier-routing.md §4: when the
    /// safe answer is unavailable, take the conservative one.
    pub fn of(upstream: &str) -> Locality {
        let Some(host) = host_of(upstream) else {
            return Locality::Remote;
        };
        let host = host.to_ascii_lowercase();
        if host == "localhost" || host.ends_with(".localhost") {
            return Locality::Local;
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(ip) if ip.is_loopback() => Locality::Local,
            _ => Locality::Remote,
        }
    }

    pub fn is_local(self) -> bool {
        self == Locality::Local
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Locality::Local => "local",
            Locality::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the broker reaches us. Loopback only, by default and by intent.
    pub listen: SocketAddr,
    /// OpenAI-compatible base URL.
    pub upstream: String,
    /// Whether `upstream` is on this machine. Derived from it, not configured.
    pub locality: Locality,
    /// Model id passed through to the upstream.
    pub model: String,
    /// Retrieval model for /api/embed. Separate from `model` because
    /// embedding and chat are different model families — nothing sensible
    /// serves both.
    pub embed_model: String,
    /// Directory holding manifest.json, vectors.f32 and chunks.jsonl.
    /// Absent means no retrieval — hadald answers from the model alone.
    pub index_dir: Option<PathBuf>,
    /// File holding the API key. Never an environment variable — see below.
    pub key_file: PathBuf,
    /// Append a line per outbound request here, so "what left this machine"
    /// has an answer that is not "trust me".
    pub egress_log: Option<PathBuf>,
    /// Also record the full prompt in the egress log. Off by default: the
    /// prompts contain the build logs and journal excerpts that are the whole
    /// privacy question, so writing them to a second file is a deliberate act.
    pub log_bodies: bool,
}

#[derive(Debug)]
pub enum ConfigError {
    Usage(String),
    Key(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Usage(m) => write!(f, "{m}"),
            ConfigError::Key(m) => write!(f, "api key: {m}"),
        }
    }
}

/// Read the API key from a file, refusing one anybody else can read.
///
/// Deliberately not an environment variable. A process environment is readable
/// via `/proc/<pid>/environ` for anyone who can already see the process, it is
/// inherited by children, and it lands in crash dumps and `systemctl show`
/// output. A file has an owner and a mode, which is exactly the property
/// wanted, and `systemd`'s `LoadCredential=` can supply it without it ever
/// touching the filesystem the daemon can see.
pub fn read_key(path: &Path) -> Result<String, ConfigError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| ConfigError::Key(format!("cannot stat {}: {e}", path.display())))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(ConfigError::Key(format!(
                "{} is mode {:04o}; refusing to read a key that is group- or world-readable \
                 (chmod 600)",
                path.display(),
                mode
            )));
        }
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Key(format!("cannot read {}: {e}", path.display())))?;
    let key = raw.trim().to_string();

    if key.is_empty() {
        return Err(ConfigError::Key(format!("{} is empty", path.display())));
    }
    // NVIDIA Build keys are `nvapi-...`. Warn rather than reject: the same
    // daemon should work against any OpenAI-compatible endpoint.
    if !key.starts_with("nvapi-") {
        tracing::warn!(
            "key in {} does not look like an NVIDIA Build key (expected nvapi-…); \
             continuing in case this is another OpenAI-compatible endpoint",
            path.display()
        );
    }
    Ok(key)
}

impl Config {
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Result<Self, ConfigError> {
        let mut listen = DEFAULT_LISTEN.to_string();
        let mut upstream = DEFAULT_UPSTREAM.to_string();
        let mut model = String::new();
        let mut embed_model = DEFAULT_EMBED_MODEL.to_string();
        let mut index_dir = None;
        let mut key_file = PathBuf::from("/etc/hadal/upstream.key");
        let mut egress_log = None;
        let mut log_bodies = false;

        let mut it = args.into_iter().skip(1).peekable();
        while let Some(arg) = it.next() {
            let mut val = |name: &str| -> Result<String, ConfigError> {
                it.next()
                    .ok_or_else(|| ConfigError::Usage(format!("{name} needs a value")))
            };
            match arg.as_str() {
                "--listen" => listen = val("--listen")?,
                "--upstream" => upstream = val("--upstream")?,
                "--model" => model = val("--model")?,
                "--embed-model" => embed_model = val("--embed-model")?,
                "--index" => index_dir = Some(PathBuf::from(val("--index")?)),
                "--key-file" => key_file = PathBuf::from(val("--key-file")?),
                "--egress-log" => egress_log = Some(PathBuf::from(val("--egress-log")?)),
                "--log-bodies" => log_bodies = true,
                "--serve" => {}
                other => return Err(ConfigError::Usage(format!("unknown argument: {other}"))),
            }
        }

        if model.is_empty() {
            return Err(ConfigError::Usage("--model is required".into()));
        }

        let listen: SocketAddr = listen
            .parse()
            .map_err(|e| ConfigError::Usage(format!("--listen is not an address: {e}")))?;

        if !listen.ip().is_loopback() {
            return Err(ConfigError::Usage(format!(
                "--listen {listen} is not loopback. hadald speaks plaintext HTTP and carries an \
                 API key; it is reachable only from inside its own network namespace by design. \
                 Binding it elsewhere would publish both."
            )));
        }

        let upstream = upstream.trim_end_matches('/').to_string();
        let locality = Locality::of(&upstream);

        Ok(Config {
            listen,
            locality,
            upstream,
            model,
            embed_model,
            index_dir,
            key_file,
            egress_log,
            log_bodies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("hadald").chain(v.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn model_is_required() {
        assert!(Config::from_args(args(&["--serve"])).is_err());
    }

    #[test]
    fn defaults_are_loopback_and_nvidia() {
        let c = Config::from_args(args(&["--model", "m"])).unwrap();
        assert_eq!(c.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(c.upstream, DEFAULT_UPSTREAM);
        assert!(c.listen.ip().is_loopback());
    }

    /// Binding off-loopback would expose both a plaintext API and, indirectly,
    /// the key behind it. The daemon refuses rather than warns.
    #[test]
    fn refuses_to_bind_off_loopback() {
        for addr in ["0.0.0.0:11434", "192.168.1.5:11434", "[::]:11434"] {
            let e = Config::from_args(args(&["--model", "m", "--listen", addr]));
            assert!(e.is_err(), "should refuse {addr}");
        }
        assert!(Config::from_args(args(&["--model", "m", "--listen", "[::1]:11434"])).is_ok());
    }

    #[test]
    fn trailing_slash_on_upstream_is_normalised() {
        let c =
            Config::from_args(args(&["--model", "m", "--upstream", "https://x.example/v1/"]))
                .unwrap();
        assert_eq!(c.upstream, "https://x.example/v1");
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(Config::from_args(args(&["--model", "m", "--yolo"])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_key_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hadald-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let loose = dir.join("loose.key");
        std::fs::write(&loose, "nvapi-secret").unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_key(&loose).is_err(), "0644 key must be refused");

        let tight = dir.join("tight.key");
        std::fs::write(&tight, "nvapi-secret\n").unwrap();
        std::fs::set_permissions(&tight, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_key(&tight).unwrap(), "nvapi-secret");

        let empty = dir.join("empty.key");
        std::fs::write(&empty, "   \n").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_key(&empty).is_err(), "empty key must be refused");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nvidia_default_is_remote() {
        let c = Config::from_args(args(&["--model", "m"])).unwrap();
        assert_eq!(c.locality, Locality::Remote);
    }

    /// The case that makes a local reflex model possible at all: llama-server
    /// on loopback speaks the same protocol as the remote endpoint.
    #[test]
    fn loopback_upstreams_are_local() {
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://localhost:8080/v1",
            "http://[::1]:8080/v1",
            "http://127.1.2.3:8080/v1",
            "https://localhost/v1",
            "http://foo.localhost:8080/v1",
        ] {
            let c = Config::from_args(args(&["--model", "m", "--upstream", url])).unwrap();
            assert_eq!(c.locality, Locality::Local, "{url} should be local");
        }
    }

    #[test]
    fn everything_else_is_remote() {
        for url in [
            "https://integrate.api.nvidia.com/v1",
            "http://192.168.1.50:8080/v1",
            "https://api.openai.com/v1",
            "http://10.0.0.1/v1",
            // Resolves to loopback in many setups, but we cannot know that
            // without resolving, and guessing local would suppress a real
            // egress record.
            "http://localhost.evil.example/v1",
        ] {
            let c = Config::from_args(args(&["--model", "m", "--upstream", url])).unwrap();
            assert_eq!(c.locality, Locality::Remote, "{url} should be remote");
        }
    }

    /// A URL this code cannot parse must not be optimistically called local.
    #[test]
    fn unparseable_upstream_is_remote() {
        for url in ["", "://", "http://", "http://@/v1"] {
            assert_eq!(Locality::of(url), Locality::Remote, "{url:?} should be remote");
        }
    }

    #[test]
    fn userinfo_does_not_hide_the_host() {
        // The host is after the *last* '@', so a password containing '@' or a
        // username shaped like a hostname cannot spoof the classification.
        assert_eq!(Locality::of("http://user:p@ss@127.0.0.1:8080/v1"), Locality::Local);
        assert_eq!(
            Locality::of("http://127.0.0.1@evil.example/v1"),
            Locality::Remote,
            "a loopback-looking username must not make a remote host local"
        );
    }

    #[test]
    fn host_extraction() {
        assert_eq!(host_of("https://a.example/v1"), Some("a.example"));
        assert_eq!(host_of("https://a.example:443/v1"), Some("a.example"));
        assert_eq!(host_of("http://[::1]:8080/v1"), Some("::1"));
        assert_eq!(host_of("http://[::1]"), Some("::1"));
        assert_eq!(host_of("a.example/v1"), Some("a.example"));
        assert_eq!(host_of("http://a.example?x=1"), Some("a.example"));
    }
}
