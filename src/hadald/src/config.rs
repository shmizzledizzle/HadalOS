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
/// This is the mechanism `docs/tier-routing.md` needs. That document routes on
/// whether data *must stay here*; it could not route anywhere while hadald had
/// exactly one place to send things. With `--fallback` there is now a chain,
/// each link carrying its own locality, so a chain may legitimately mix a
/// loopback reflex model with remote flagships — and every per-link decision
/// below (key, egress line, warning) is made per link rather than once.
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

/// One place hadald can send a chat completion.
///
/// The three fields travel together because they are not independent: a model
/// id is meaningful only against the endpoint that serves it, and a key is
/// meaningful only against the endpoint that issued it. The bug this shape
/// prevents is a fallback chain that shares one model name across providers —
/// `nvidia/nemotron-3-ultra-550b-a55b` is a 404 at Groq, and a chain
/// whose every link 404s is a chain that has quietly become a single point of
/// failure with extra latency.
#[derive(Debug, Clone)]
pub struct Upstream {
    /// OpenAI-compatible base URL, no trailing slash.
    pub base: String,
    /// Model id passed through to this endpoint, and only this one.
    pub model: String,
    /// File holding this endpoint's API key. `None` iff local — a loopback
    /// server needs no credential, and requiring one is how a real key ends up
    /// mode 0644 in someone's notes.
    pub key_file: Option<PathBuf>,
    /// Whether `base` is on this machine. Derived from it, not configured.
    pub locality: Locality,
}

/// Parse one `--fallback URL,MODEL[,KEYFILE]`.
///
/// Comma-separated rather than three more flags because the fields must stay
/// bound to each other. The alternative — repeated `--upstream`/`--model` where
/// the latter attaches to the former — makes correctness depend on argument
/// *order*, and `hadald.service` already passes `--model` before `--upstream`.
/// A silent misgrouping there would send the primary model id to a fallback.
///
/// A comma is legal in a URL and in a path, and this will mis-split such a
/// value. That is accepted: the failure is loud (a nonsense host, refused at
/// config time or at connect) rather than silent, and no endpoint in practice
/// has one.
fn parse_fallback(spec: &str) -> Result<Upstream, ConfigError> {
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    let (base, model, key) = match parts.as_slice() {
        [b, m] => (*b, *m, None),
        [b, m, k] => (*b, *m, Some(*k)),
        _ => {
            return Err(ConfigError::Usage(format!(
                "--fallback {spec:?}: expected URL,MODEL or URL,MODEL,KEYFILE"
            )))
        }
    };
    if base.is_empty() || model.is_empty() {
        return Err(ConfigError::Usage(format!(
            "--fallback {spec:?}: URL and MODEL must both be non-empty"
        )));
    }

    let base = base.trim_end_matches('/').to_string();
    let locality = Locality::of(&base);
    let key_file = match (locality, key) {
        // Dropped rather than honoured — a loopback server needs no credential
        // — but said out loud, because the author clearly expected it to be
        // used, and a key silently going unsent is exactly the kind of thing
        // someone re-derives from a 401 an hour later.
        (Locality::Local, Some(k)) if !k.is_empty() => {
            tracing::warn!(
                "--fallback {base}: ignoring key file {k}; the endpoint is on this machine \
                 and no Authorization header will be sent"
            );
            None
        }
        (Locality::Local, _) => None,
        // Refused, not defaulted to the primary's key. Sharing one key across
        // providers cannot work — they are issued by different parties — so a
        // default here would only produce a 401 at the moment the chain is
        // being relied on, which is the moment it must not fail.
        (Locality::Remote, None) => {
            return Err(ConfigError::Usage(format!(
                "--fallback {spec:?}: {base} is remote and needs its own key file \
                 (URL,MODEL,KEYFILE)"
            )))
        }
        (Locality::Remote, Some("")) => {
            return Err(ConfigError::Usage(format!(
                "--fallback {spec:?}: empty key file path"
            )))
        }
        (Locality::Remote, Some(k)) => Some(PathBuf::from(k)),
    };

    Ok(Upstream { base, model: model.to_string(), key_file, locality })
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the broker reaches us. Loopback only, by default and by intent.
    pub listen: SocketAddr,
    /// Where chat completions may go, in the order they are tried. Never
    /// empty; `chain[0]` is the primary and is the only link `/api/embed` and
    /// `/api/retrieve` will ever use — see `Config::primary`.
    pub chain: Vec<Upstream>,
    /// Retrieval model for /api/embed. Separate from a chat model because
    /// embedding and chat are different model families — nothing sensible
    /// serves both.
    pub embed_model: String,
    /// Directory holding manifest.json, vectors.f32 and chunks.jsonl.
    /// Absent means no retrieval — hadald answers from the model alone.
    pub index_dir: Option<PathBuf>,
    /// Append a line per outbound request here, so "what left this machine"
    /// has an answer that is not "trust me".
    pub egress_log: Option<PathBuf>,
    /// Also record the full prompt in the egress log. Off by default: the
    /// prompts contain the build logs and journal excerpts that are the whole
    /// privacy question, so writing them to a second file is a deliberate act.
    pub log_bodies: bool,
}

impl Config {
    /// The first link, which is also the *only* link retrieval may use.
    ///
    /// Embeddings deliberately do not fail over, and this accessor is where
    /// that is enforced. Vectors are not comparable across models: the warning
    /// on `DEFAULT_EMBED_MODEL` about changing it invalidating the index
    /// applies just as much to changing it *for one request*. A chat request
    /// that lands on a fallback returns a slightly different answer; an embed
    /// request that lands on a fallback returns a vector from a different
    /// space, `search` ranks it against the index anyway, and retrieval becomes
    /// approximately random with no error anywhere. Degrading loudly is the
    /// whole point of the chain — so the half that cannot degrade loudly does
    /// not get one.
    pub fn primary(&self) -> &Upstream {
        &self.chain[0]
    }
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
    // There was a warning here for keys not starting with `nvapi-`, on the
    // reasoning that NVIDIA Build was the expected endpoint. A chain makes that
    // backwards: `csk-`, `gsk_`, `sk-or-v1-` and the rest are now the normal
    // case, so the check fired on correct configurations and stayed silent on
    // the only one it was aimed at. config.rs's own note on the startup banner
    // applies to itself — "a warning that cries wolf is a warning people learn
    // to skip". The shape of a key is the issuing endpoint's business; the
    // things hadald can actually check, mode and emptiness, are checked above.
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

        let mut fallbacks: Vec<Upstream> = Vec::new();

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
                "--fallback" => fallbacks.push(parse_fallback(&val("--fallback")?)?),
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

        let base = upstream.trim_end_matches('/').to_string();
        let locality = Locality::of(&base);
        let primary = Upstream {
            base,
            model,
            key_file: match locality {
                Locality::Local => None,
                Locality::Remote => Some(key_file),
            },
            locality,
        };

        let mut chain = Vec::with_capacity(1 + fallbacks.len());
        chain.push(primary);
        chain.append(&mut fallbacks);

        // A repeated endpoint is a chain that cannot fail over: the second
        // attempt hits the same rate limit that sent us there, one round trip
        // later. Refused rather than deduplicated, because which of the two the
        // author meant to change — the URL or the model — is not knowable here.
        for i in 0..chain.len() {
            if let Some(j) = (i + 1..chain.len())
                .find(|&j| chain[j].base == chain[i].base && chain[j].model == chain[i].model)
            {
                return Err(ConfigError::Usage(format!(
                    "links {} and {} of the chain are both {} serving {} — a fallback to the \
                     endpoint that just failed is not a fallback",
                    i + 1,
                    j + 1,
                    chain[i].base,
                    chain[i].model
                )));
            }
        }

        Ok(Config { listen, chain, embed_model, index_dir, egress_log, log_bodies })
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
        assert_eq!(c.primary().base, DEFAULT_UPSTREAM);
        assert!(c.listen.ip().is_loopback());
    }

    /// No `--fallback` must stay exactly the daemon that existed before the
    /// chain did — one link, no behaviour change, `hadald.service` and
    /// `wire-hadal.sh` unmodified.
    #[test]
    fn without_fallbacks_the_chain_is_one_link() {
        let c = Config::from_args(args(&[
            "--serve",
            "--model",
            "m",
            "--upstream",
            "https://a.example/v1",
            "--key-file",
            "/etc/hadal/upstream.key",
        ]))
        .unwrap();
        assert_eq!(c.chain.len(), 1);
        assert_eq!(c.primary().model, "m");
        assert_eq!(c.primary().key_file.as_deref(), Some(Path::new("/etc/hadal/upstream.key")));
    }

    /// `hadald.service` passes `--model` before `--upstream`. Any scheme that
    /// grouped flags by position would bind the primary's model to nothing —
    /// which is precisely why `--fallback` carries its own fields.
    #[test]
    fn flag_order_does_not_regroup_the_primary() {
        let a = Config::from_args(args(&["--model", "m", "--upstream", "https://x.example/v1"]))
            .unwrap();
        let b = Config::from_args(args(&["--upstream", "https://x.example/v1", "--model", "m"]))
            .unwrap();
        assert_eq!(a.chain.len(), 1);
        assert_eq!(a.primary().model, b.primary().model);
        assert_eq!(a.primary().base, b.primary().base);
    }

    #[test]
    fn fallbacks_keep_their_own_model_and_key() {
        let c = Config::from_args(args(&[
            "--model",
            "primary-model",
            "--upstream",
            "https://a.example/v1",
            "--fallback",
            "https://b.example/v1/,b-model,/etc/hadal/b.key",
            "--fallback",
            "http://127.0.0.1:8080/v1,local-model",
        ]))
        .unwrap();
        assert_eq!(c.chain.len(), 3);

        // Trailing slash normalised on fallbacks too, or `{base}/chat/completions`
        // becomes a double slash and some gateways 404 on it.
        assert_eq!(c.chain[1].base, "https://b.example/v1");
        assert_eq!(c.chain[1].model, "b-model");
        assert_eq!(c.chain[1].key_file.as_deref(), Some(Path::new("/etc/hadal/b.key")));
        assert_eq!(c.chain[1].locality, Locality::Remote);

        // A local link carries no key even though one was never offered.
        assert_eq!(c.chain[2].model, "local-model");
        assert_eq!(c.chain[2].key_file, None);
        assert_eq!(c.chain[2].locality, Locality::Local);
    }

    /// Sharing the primary's key with a fallback cannot work — different
    /// parties issue them — so the omission is refused at startup rather than
    /// producing a 401 at the moment the chain is being relied on.
    #[test]
    fn a_remote_fallback_without_a_key_is_refused() {
        assert!(Config::from_args(args(&[
            "--model",
            "m",
            "--fallback",
            "https://b.example/v1,b-model"
        ]))
        .is_err());
    }

    #[test]
    fn malformed_fallbacks_are_refused() {
        for spec in [
            "https://b.example/v1",            // no model
            "https://b.example/v1,m,k,extra",  // too many fields
            ",m,k",                            // empty url
            "https://b.example/v1,,k",         // empty model
            "https://b.example/v1,m,",         // empty key path
        ] {
            assert!(
                Config::from_args(args(&["--model", "m", "--fallback", spec])).is_err(),
                "{spec:?} should be refused"
            );
        }
    }

    /// A chain whose links are the same endpoint and model retries into the
    /// same rate limit one round trip later. That is not redundancy.
    #[test]
    fn a_duplicate_link_is_refused() {
        assert!(Config::from_args(args(&[
            "--model",
            "m",
            "--upstream",
            "https://a.example/v1",
            "--fallback",
            "https://a.example/v1,m,/etc/hadal/a.key",
        ]))
        .is_err());

        // Same endpoint, different model, is a legitimate chain: providers
        // retire free models one at a time.
        assert!(Config::from_args(args(&[
            "--model",
            "m",
            "--upstream",
            "https://a.example/v1",
            "--fallback",
            "https://a.example/v1,other,/etc/hadal/a.key",
        ]))
        .is_ok());
    }

    /// The chain may mix tiers, which is the arrangement `docs/tier-routing.md`
    /// wants: try the local reflex model first and only reach for a third party
    /// when it cannot answer.
    #[test]
    fn a_chain_may_start_local_and_end_remote() {
        let c = Config::from_args(args(&[
            "--model",
            "reflex",
            "--upstream",
            "http://127.0.0.1:8080/v1",
            "--fallback",
            "https://b.example/v1,flagship,/etc/hadal/b.key",
        ]))
        .unwrap();
        assert_eq!(c.primary().locality, Locality::Local);
        assert_eq!(c.primary().key_file, None, "a local primary must not demand a key");
        assert_eq!(c.chain[1].locality, Locality::Remote);
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
        assert_eq!(c.primary().base, "https://x.example/v1");
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
        assert_eq!(c.primary().locality, Locality::Remote);
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
            assert_eq!(c.primary().locality, Locality::Local, "{url} should be local");
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
            assert_eq!(c.primary().locality, Locality::Remote, "{url} should be remote");
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
