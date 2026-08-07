//! Configuration, and the one place a secret is read.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:11434";
pub const DEFAULT_UPSTREAM: &str = "https://integrate.api.nvidia.com/v1";

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the broker reaches us. Loopback only, by default and by intent.
    pub listen: SocketAddr,
    /// OpenAI-compatible base URL.
    pub upstream: String,
    /// Model id passed through to the upstream.
    pub model: String,
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

        Ok(Config {
            listen,
            upstream: upstream.trim_end_matches('/').to_string(),
            model,
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
}
