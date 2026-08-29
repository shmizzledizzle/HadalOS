//! `hadald` — the Hadal model host.
//!
//! Ollama-shaped inward, OpenAI-shaped outward. `hadal-brokerd` speaks the two
//! Ollama endpoints it already knows (`/api/tags`, `/api/generate`) over
//! plaintext loopback; hadald translates to an OpenAI-compatible chat
//! completion upstream and streams the result back as newline-delimited JSON.
//!
//! The broker needs no changes for this to work, which is the point. Swapping
//! a remote 70B for a local GGUF later is a change to this file and nothing
//! else.
//!
//! # What this does to the "local" claim
//!
//! HadalOS's README says the model daemon "runs in a network namespace with no
//! route out" and that local is "a kernel guarantee, not a marketing claim".
//! Backed by a remote endpoint, that sentence is false, and pretending
//! otherwise would be worse than the change itself.
//!
//! What survives intact is the *safety* property, and it survives by design
//! rather than by luck: the broker was built not to trust the model, so where
//! the model runs is irrelevant to "there is no code path from model output to
//! a command interpreter". Proposals are still typed, still validated by
//! `action.rs`, still gated by polkit.
//!
//! What does not survive is the *privacy* property. Build logs and journal
//! excerpts are exactly the payloads this sends, and they carry hostnames,
//! usernames, paths, and occasionally secrets. `--egress-log` exists so that
//! "what left this machine" has an answer that is not a promise.
//!
//! `systemd/hadald.service` already prescribes the intended confinement — a
//! `systemd-socket-proxyd` pinned to one upstream, rather than dropping
//! `PrivateNetwork=`. See `README.md` here for the deployment.

mod config;
mod retrieve;
mod upstream;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use config::{Config, Locality};
use upstream::{Delta, SseDecoder};

struct Ctx {
    cfg: Config,
    /// One entry per link of `cfg.chain`, same order. `None` where that link is
    /// local — a loopback server needs no key.
    ///
    /// Read once at startup rather than per request, so an unreadable or
    /// world-readable key file is a refusal to start rather than a 401 at the
    /// moment the chain is being leaned on.
    keys: Vec<Option<String>>,
    http: reqwest::Client,
    /// None when no index is configured, or when loading failed. Retrieval is
    /// an enhancement, so a broken index degrades to answering without it —
    /// but loudly, because silently unretrieved is exactly the failure this
    /// index exists to prevent.
    index: Option<retrieve::Index>,
}

impl Ctx {
    /// Attach link `i`'s API key if it has one. Sending `Bearer` to a loopback
    /// llama-server would be noise; omitting it against a remote endpoint would
    /// be a 401.
    ///
    /// Indexed by link, never by "the" key: the whole point of a chain is that
    /// the credential differs per hop, and one shared key would 401 on every
    /// link but the one that issued it.
    fn auth(&self, i: usize, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.keys[i] {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GenerateRequest {
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    system: String,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hadald=info".into()),
        )
        .init();

    let cfg = match Config::from_args(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hadald: {e}");
            eprintln!(
                "\nusage: hadald --serve --model <id> [--key-file PATH] [--listen ADDR]\n\
                 [--upstream URL] [--fallback URL,MODEL[,KEYFILE]]... [--egress-log PATH]\n\
                 [--log-bodies]\n\n\
                 --fallback may be given more than once. Links are tried in the order\n\
                 written, and only until one accepts the request — see README.md."
            );
            return std::process::ExitCode::from(2);
        }
    };

    // A local link needs no credential, and requiring one would make the local
    // tier impossible to run without inventing a dummy key file — which is how
    // a real key ends up mode 0644 in someone's notes.
    //
    // Every key is read here, before the socket is bound. A chain whose third
    // link has an unreadable key file is broken at configuration time, and
    // discovering that on the day the first two are rate-limited would be
    // discovering it at the worst available moment.
    let mut keys = Vec::with_capacity(cfg.chain.len());
    for up in &cfg.chain {
        match &up.key_file {
            None => {
                tracing::info!("upstream {} is local; no API key required", up.base);
                keys.push(None);
            }
            Some(path) => match config::read_key(path) {
                Ok(k) => keys.push(Some(k)),
                Err(e) => {
                    eprintln!("hadald: {} ({e})", up.base);
                    return std::process::ExitCode::FAILURE;
                }
            },
        }
    }

    let index = match &cfg.index_dir {
        None => None,
        Some(dir) => match retrieve::Index::load(dir) {
            Ok(i) => {
                tracing::info!(
                    "retrieval index: {} chunks from {} (model {})",
                    i.len(), dir.display(), i.model
                );
                if i.model != cfg.embed_model {
                    tracing::warn!(
                        "index was built with '{}' but --embed-model is '{}' — queries will be \
                         embedded by a different model than the documents, and retrieval will be \
                         close to random",
                        i.model, cfg.embed_model
                    );
                }
                Some(i)
            }
            Err(e) => {
                tracing::error!("retrieval disabled: {e}");
                None
            }
        },
    };

    let ctx = Arc::new(Ctx {
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .expect("http client"),
        cfg: cfg.clone(),
        keys,
        index,
    });

    let app = Router::new()
        .route("/api/tags", get(tags))
        .route("/api/generate", post(generate))
        .route("/api/embed", post(embed))
        .route("/api/retrieve", post(retrieve_handler))
        .with_state(ctx);

    let listener = match tokio::net::TcpListener::bind(cfg.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hadald: cannot bind {}: {e}", cfg.listen);
            return std::process::ExitCode::FAILURE;
        }
    };

    // Say which tier is serving, unprompted. docs/tier-routing.md §5: a reader
    // cannot tell from a model name whether it ran here or in someone else's
    // datacentre, and locality is the part that matters.
    //
    // Every link is named, not just the primary. The chain's failure mode is
    // that it works — a prompt lands somewhere the operator forgot was
    // configured, and nothing about the answer says so. Listing the whole chain
    // at startup is the one place that is cheap to state.
    tracing::info!("hadald listening on {} — {} upstream(s):", cfg.listen, cfg.chain.len());
    for (i, up) in cfg.chain.iter().enumerate() {
        tracing::info!(
            "  {}. model {} via {} [{}]",
            i + 1,
            up.model,
            up.base,
            up.locality.as_str()
        );
    }

    let remote: Vec<&str> =
        cfg.chain.iter().filter(|u| u.locality == Locality::Remote).map(|u| u.base.as_str()).collect();
    if !remote.is_empty() {
        if cfg.egress_log.is_none() {
            tracing::warn!(
                "no --egress-log: outbound prompts will not be recorded anywhere. \
                 This daemon sends system logs to {} third part{}: {}",
                remote.len(),
                if remote.len() == 1 { "y" } else { "ies" },
                remote.join(", ")
            );
        }
        for base in &remote {
            if base.starts_with("http://") {
                tracing::warn!(
                    "upstream {base} is remote and plaintext — the API key and every prompt \
                     cross the network unencrypted"
                );
            }
        }
    }

    // The unit ships PrivateNetwork=yes with a socket proxy "pinned to exactly
    // one upstream". A chain has more than one, so that pin has to grow a
    // destination per remote link or the chain silently collapses to whichever
    // link the proxy can still reach. Said here because the daemon is the only
    // component that knows how many there are.
    if remote.len() > 1 {
        tracing::info!(
            "{} remote links configured — under systemd's PrivateNetwork=yes each needs its \
             own socket proxy destination; see README.md",
            remote.len()
        );
    }

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    {
        eprintln!("hadald: server error: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Readiness probe. `ModelClient::ready()` only checks for a 2xx, but report
/// the configured model so a human running `curl` learns something.
///
/// Advertises the primary's name only — the broker names one model and expects
/// one back — but lists the chain under `details` so the answer to "where can
/// this thing send my journal" is available without reading the unit file.
async fn tags(State(ctx): State<Arc<Ctx>>) -> impl IntoResponse {
    let primary = ctx.cfg.primary();
    Json(serde_json::json!({
        "models": [{
            "name": primary.model,
            "model": primary.model,
            "details": {
                "family": "hadald-upstream",
                "upstream": primary.base,
                "locality": primary.locality.as_str(),
                "chain": ctx.cfg.chain.iter().map(|u| serde_json::json!({
                    "upstream": u.base,
                    "model": u.model,
                    "locality": u.locality.as_str(),
                })).collect::<Vec<_>>(),
            },
        }]
    }))
}

/// Record one outbound request. Appends; never truncates.
///
/// Called once per *attempt*, not once per request, and before the attempt is
/// made. A prompt that went to the first link and came back 429 still left this
/// machine; a log that recorded only the link which eventually answered would
/// understate the exposure by exactly the number of providers the chain walked
/// past. `attempt=` makes the walk legible after the fact.
fn note_egress(
    cfg: &Config,
    up: &config::Upstream,
    attempt: usize,
    of: usize,
    prompt: &str,
    system: &str,
) {
    // Nothing left the machine, so nothing belongs in the record of what left
    // the machine. A log that answers a different question than its name is
    // worse than no log.
    if up.locality.is_local() {
        return;
    }
    let Some(path) = &cfg.egress_log else { return };
    use std::io::Write;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut line = format!(
        "{stamp} model={} upstream={} attempt={}/{} prompt_bytes={} system_bytes={}\n",
        up.model,
        up.base,
        attempt + 1,
        of,
        prompt.len(),
        system.len()
    );
    if cfg.log_bodies {
        line.push_str("--- prompt ---\n");
        line.push_str(prompt);
        line.push_str("\n--- end ---\n");
    }

    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::error!("egress log write failed: {e}");
            }
        }
        // Deliberately loud. An unwritable audit log is not a detail.
        Err(e) => tracing::error!("cannot open egress log {}: {e}", path.display()),
    }
}

#[derive(Debug, Deserialize)]
struct EmbedRequest {
    #[serde(default)]
    input: Vec<String>,
}

/// Ollama's `/api/embed`, backed by an OpenAI-compatible `/v1/embeddings`.
///
/// Exists so Hadal's `rag/build_index.py` and `rag/search.py` work unchanged:
/// they already speak this shape. The embedding model is separate from the
/// chat model — retrieval models are their own thing — and is selected with
/// `--embed-model`.
///
/// Non-streaming, so unlike `generate` this waits for the whole reply. Batches
/// are the caller's business; `build_index.py` sends 32 at a time.
///
/// **Does not fail over.** `--fallback` is a chat mechanism only; see
/// `Config::primary` for why extending it here would corrupt retrieval instead
/// of rescuing it. An embedding request that cannot be served fails, and the
/// caller — `retrieve_handler` below, or `build_index.py` — finds out.
async fn embed(
    State(ctx): State<Arc<Ctx>>,
    Json(req): Json<EmbedRequest>,
) -> Result<Response, Response> {
    if req.input.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty input").into_response());
    }

    let up = ctx.cfg.primary();
    let (body, kind) = upstream::embed_body(&ctx.cfg.embed_model, &req.input);
    note_egress_embed(&ctx.cfg, up, &req.input, kind.as_str());

    let resp = ctx
        .auth(0, ctx.http.post(format!("{}/embeddings", up.base)))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("upstream unreachable: {e}");
            (StatusCode::BAD_GATEWAY, format!("upstream unreachable: {e}")).into_response()
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        // Log the reason, not just the code. `generate` already does this and
        // `embed` did not, which cost a diagnosis: a 400 meaning "Input length
        // 1984 exceeds maximum allowed token size 512" surfaced to the caller
        // as a bare 502 with no clue that chunk size was the problem.
        //
        // Still not forwarded to the caller — an embeddings error can echo the
        // input, and the input is the thing being careful about.
        let detail = resp.text().await.unwrap_or_default();
        tracing::error!("embeddings upstream returned {status}: {}", detail.trim());
        return Err((StatusCode::BAD_GATEWAY, format!("upstream returned {status}"))
            .into_response());
    }

    let parsed: upstream::EmbeddingsResponse = resp.json().await.map_err(|e| {
        (StatusCode::BAD_GATEWAY, format!("malformed embeddings reply: {e}")).into_response()
    })?;

    let vectors = parsed.ordered();
    if vectors.len() != req.input.len() {
        // Silently returning fewer vectors than inputs would pair chunks with
        // the wrong text for the rest of the batch.
        tracing::error!("upstream returned {} vectors for {} inputs", vectors.len(), req.input.len());
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("upstream returned {} vectors for {} inputs", vectors.len(), req.input.len()),
        )
            .into_response());
    }

    Ok(Json(serde_json::json!({ "embeddings": vectors })).into_response())
}

fn note_egress_embed(cfg: &Config, up: &config::Upstream, inputs: &[String], kind: &str) {
    if up.locality.is_local() {
        return;
    }
    let Some(path) = &cfg.egress_log else { return };
    use std::io::Write;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bytes: usize = inputs.iter().map(String::len).sum();
    let line = format!(
        "{stamp} embed model={} upstream={} input_type={kind} count={} bytes={bytes}\n",
        cfg.embed_model,
        up.base,
        inputs.len()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[derive(Debug, Deserialize)]
struct RetrieveRequest {
    #[serde(default)]
    query: String,
    #[serde(default = "default_k")]
    k: usize,
}

fn default_k() -> usize {
    5
}

/// Retrieve reference passages for a query.
///
/// The broker calls this and decides what to do with the result; hadald only
/// ranks. Returns an empty list rather than an error when no index is loaded,
/// so a caller can always ask and simply get nothing back.
async fn retrieve_handler(
    State(ctx): State<Arc<Ctx>>,
    Json(req): Json<RetrieveRequest>,
) -> Result<Response, Response> {
    let Some(index) = &ctx.index else {
        return Ok(Json(serde_json::json!({ "passages": [], "reason": "no index loaded" }))
            .into_response());
    };
    if req.query.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty query").into_response());
    }

    // Embedded as a *query*, not a passage. Retrieval models place questions
    // and documents in different regions of the space; using the document
    // encoding for a question silently degrades every result.
    //
    // Primary only, for the same reason `embed` is: the index was built by one
    // model, and a query vector from any other is not in the space `search`
    // ranks against.
    let up = ctx.cfg.primary();
    let (body, _) = upstream::embed_body(
        &ctx.cfg.embed_model,
        &[format!("search_query: {}", req.query)],
    );
    let resp = ctx
        .auth(0, ctx.http.post(format!("{}/embeddings", up.base)))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            (StatusCode::BAD_GATEWAY, format!("embedding the query failed: {e}")).into_response()
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        tracing::error!("query embedding returned {status}: {}", detail.trim());
        return Err((StatusCode::BAD_GATEWAY, format!("upstream returned {status}"))
            .into_response());
    }

    let parsed: upstream::EmbeddingsResponse = resp.json().await.map_err(|e| {
        (StatusCode::BAD_GATEWAY, format!("malformed embeddings reply: {e}")).into_response()
    })?;
    let Some(vector) = parsed.ordered().into_iter().next() else {
        return Err((StatusCode::BAD_GATEWAY, "no embedding returned").into_response());
    };

    let hits = index.search(&vector, req.k.clamp(1, 20));
    tracing::info!("retrieve: {} hits for {} chars", hits.len(), req.query.len());

    Ok(Json(serde_json::json!({
        "passages": hits.iter().map(|(s, c)| serde_json::json!({
            "ref": c.r#ref, "score": s, "text": c.text
        })).collect::<Vec<_>>(),
        "text": retrieve::format_passages(&hits),
    }))
    .into_response())
}

/// Chat completion, walking the chain until a link accepts.
///
/// # Where the failover boundary is, and why it is there
///
/// A link may be abandoned only up to the moment its response headers arrive
/// and prove successful. After that hadald has committed: the body is streamed
/// straight through to the broker, and the broker's `ProposalScanner` is a
/// single pass over a single token stream. Restarting on link two mid-answer
/// would splice a second model's output onto the first's — the scanner would
/// see one prefix, then a second unrelated prefix, and the concatenation can
/// form a *valid* proposal that neither model actually made. That is the one
/// failure this daemon must not have: `main.rs`'s header claims safety survives
/// a remote model because proposals are typed and validated, and a spliced
/// proposal is well-typed.
///
/// So an upstream that returns 200 and then dies four tokens in is a failed
/// request, not a failover. Buffering the stream's opening to widen the window
/// was considered and rejected: it would add latency to every request that
/// works in order to rescue the rare one that does not, and time-to-first-token
/// is the budget the reflex tier exists to protect.
async fn generate(
    State(ctx): State<Arc<Ctx>>,
    Json(req): Json<GenerateRequest>,
) -> Result<Response, Response> {
    if req.prompt.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty prompt").into_response());
    }

    // The broker names a model; hadald answers with whichever link takes the
    // request. Compare against the primary — that is what `/api/tags`
    // advertised, so that is what the broker had to go on.
    let primary = ctx.cfg.primary();
    if !req.model.is_empty() && req.model != primary.model {
        tracing::warn!(
            "broker asked for model '{}', serving '{}'",
            req.model,
            primary.model
        );
    }

    let n = ctx.cfg.chain.len();
    // Kept so the caller gets the reason the *chain* failed rather than only
    // the reason its last link did — with one bad key at the front, the last
    // link's error is usually the least informative one.
    let mut trail: Vec<String> = Vec::with_capacity(n);
    // Set when a link rejected the prompt on length. Held rather than
    // returned, because a later link with a wider window may still accept it.
    let mut too_large: Option<String> = None;

    for (i, up) in ctx.cfg.chain.iter().enumerate() {
        // Before the request, so the record of what left this machine is
        // written even if the process dies mid-flight.
        note_egress(&ctx.cfg, up, i, n, &req.prompt, &req.system);

        let body = upstream::chat_body(&up.model, &req.system, &req.prompt);
        let sent = ctx
            .auth(i, ctx.http.post(format!("{}/chat/completions", up.base)))
            .json(&body)
            .send()
            .await;

        let resp = match sent {
            Ok(r) => r,
            // Unreachable is always worth trying the next link for: DNS, TLS
            // and connect failures say nothing about whether the request is
            // acceptable elsewhere.
            Err(e) => {
                tracing::warn!("upstream {}/{} ({}) unreachable: {e}", i + 1, n, up.base);
                trail.push(format!("{}: unreachable ({e})", up.base));
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            if i > 0 {
                tracing::info!(
                    "upstream {}/{} ({}, model {}) served the request after {} failure(s)",
                    i + 1,
                    n,
                    up.base,
                    up.model,
                    i
                );
            }
            // Committed. Translate SSE to the newline-delimited JSON the broker
            // parses; from here a failure is a truncated answer, not a retry.
            let stream = async_stream::stream(resp);
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/x-ndjson")
                .body(Body::from_stream(stream))
                .expect("response"));
        }

        let detail = resp.text().await.unwrap_or_default();
        let detail = detail.trim();

        if !upstream::is_retryable(status.as_u16()) {
            // One 400 gets its cause forwarded: the prompt did not fit.
            //
            // The standing rule is that an upstream body is never passed on,
            // because it can echo the request and the request is the thing
            // being careful about. A context-length rejection is the exception
            // worth carving out — it is arithmetic, not content, and it is the
            // single most likely 400 this daemon will ever see, because the
            // prompt is a build log and build logs have no upper bound.
            //
            // Withholding it cost a real diagnosis: `hadal explain` on a 707 KB
            // log failed with a bare "upstream returned 400 Bad Request" for
            // long enough to be mistaken for a rate limit and an expired key in
            // turn, while the endpoint had been saying "your prompt contains at
            // least 129025 input tokens" the whole time. Same finding as the
            // one already recorded in `embed` above, on the path where the
            // payload is larger and the guess is harder.
            // It is also the one 400 that *does* advance the chain. The general
            // rule below — a 400 is hadald's own bad request, so every link will
            // reject it identically — is exactly false here, because a context
            // window is a property of the link and not of the request. The
            // chain is deliberately heterogeneous: free tiers cap context as
            // well as throughput, so a prompt that overflows a 64k link may sit
            // comfortably inside the 131k one behind it. Stopping here would
            // let the smallest window in the chain decide the largest log the
            // whole chain can explain.
            //
            // The rejection is remembered rather than returned, so that if
            // nothing downstream serves it the user still gets the arithmetic
            // instead of a generic 502.
            if upstream::is_context_length_error(detail) {
                tracing::warn!(
                    "upstream {}/{} ({}, model {}) rejected the prompt on length; {} — \
                     trying the next link, whose window may be larger",
                    i + 1,
                    n,
                    up.base,
                    up.model,
                    upstream::summarise_context_error(detail)
                );
                too_large = Some(upstream::summarise_context_error(detail));
                trail.push(format!("{}: {status} (context window)", up.base));
                continue;
            }

            // Wrong request, not a wrong endpoint. Stop rather than send the
            // same rejected prompt to everyone else in the chain.
            tracing::error!("upstream {} returned {status}: {detail}", up.base);
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("upstream returned {status}"),
            )
                .into_response());
        }

        // A key that will never work, distinguished from a cap that will clear
        // on its own. Both fail over; only one of them is the operator's to fix.
        if matches!(status.as_u16(), 401 | 403 | 404) {
            tracing::warn!(
                "upstream {}/{} ({}) returned {status} — this is a standing \
                 misconfiguration (key or model id), not a transient limit; the chain is \
                 running one link short until it is fixed",
                i + 1,
                n,
                up.base
            );
        } else {
            tracing::warn!("upstream {}/{} ({}) returned {status}", i + 1, n, up.base);
        }
        tracing::debug!("upstream {} detail: {detail}", up.base);
        trail.push(format!("{}: {status}", up.base));
    }

    // Do not forward any upstream body verbatim — it can echo the request, and
    // the request is the thing we are being careful about. The endpoint names
    // and status codes below are hadald's own words.
    tracing::error!("all {n} upstream(s) failed: {}", trail.join("; "));

    // If any link turned it away on length, that is the actionable finding and
    // it outranks the tail of statuses: the log is too long, and no amount of
    // waiting for a rate limit to clear will change that.
    if let Some(summary) = too_large {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("the prompt did not fit the model's context window. {summary}"),
        )
            .into_response());
    }

    Err((
        StatusCode::BAD_GATEWAY,
        format!("all {n} upstream(s) failed: {}", trail.join("; ")),
    )
        .into_response())
}

/// SSE in, Ollama NDJSON out.
mod async_stream {
    use super::{Delta, SseDecoder};
    use futures_util::{Stream, StreamExt};

    pub fn stream(
        resp: reqwest::Response,
    ) -> impl Stream<Item = Result<String, std::io::Error>> {
        let mut decoder = SseDecoder::new();
        let mut upstream = resp.bytes_stream();
        let mut finished = false;

        futures_util::stream::poll_fn(move |cx| {
            use std::task::Poll;
            loop {
                if finished {
                    return Poll::Ready(None);
                }
                match upstream.poll_next_unpin(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(Err(e))) => {
                        finished = true;
                        return Poll::Ready(Some(Err(std::io::Error::other(e))));
                    }
                    Poll::Ready(Some(Ok(bytes))) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let mut out = String::new();
                        for delta in decoder.feed(&text) {
                            match delta {
                                Delta::Text(t) => {
                                    out.push_str(&ndjson(&t, false));
                                }
                                // A distinct field, not `response`. Ollama's
                                // own schema grew `thinking` for this, so the
                                // name is borrowed rather than invented, and a
                                // broker that does not know it ignores it —
                                // `GenerateChunk` takes unknown fields in
                                // silence, so old and new can be mixed.
                                Delta::Reasoning(t) => {
                                    out.push_str(&thinking_ndjson(&t));
                                }
                                Delta::Done => {
                                    out.push_str(&ndjson("", true));
                                    finished = true;
                                }
                            }
                        }
                        if !out.is_empty() {
                            return Poll::Ready(Some(Ok(out)));
                        }
                        // Nothing decodable yet — keep reading rather than
                        // emitting an empty frame the broker would have to skip.
                    }
                    Poll::Ready(None) => {
                        // Upstream closed without [DONE]. The broker's scanner
                        // flushes on `done`, so send one or a trailing partial
                        // proposal would be lost.
                        finished = true;
                        return Poll::Ready(Some(Ok(ndjson("", true))));
                    }
                }
            }
        })
    }

    fn ndjson(text: &str, done: bool) -> String {
        format!(
            "{}\n",
            serde_json::json!({ "response": text, "done": done })
        )
    }

    /// A reasoning frame. Carries an empty `response` on purpose.
    ///
    /// The broker reads `response` unconditionally and feeds it to
    /// `ProposalScanner`; emitting the thought there would let a rehearsed
    /// action block become a real proposal. Empty means the scanner sees
    /// nothing, and a broker that ignores `thinking` degrades to exactly the
    /// old behaviour rather than to a wrong one.
    fn thinking_ndjson(text: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({ "response": "", "thinking": text, "done": false })
        )
    }
}

#[cfg(test)]
mod egress_tests {
    use super::*;

    fn cfg_for(upstream: &str, log: &std::path::Path) -> Config {
        Config::from_args(
            [
                "hadald",
                "--model",
                "m",
                "--upstream",
                upstream,
                "--egress-log",
                log.to_str().unwrap(),
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .expect("config")
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("hadald-egress-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// The record of what left the machine must not contain things that did
    /// not leave it. Verified live 2026-08-23 and pinned here: `note_egress`
    /// runs *before* the upstream request, so this fires even with nothing
    /// listening.
    #[test]
    fn a_local_upstream_writes_no_egress_line() {
        let log = tmp("local");
        let cfg = cfg_for("http://127.0.0.1:8080/v1", &log);
        let up = cfg.primary();
        assert_eq!(up.locality, Locality::Local);
        note_egress(&cfg, up, 0, 1, "why did my build fail", "you are hadal");
        note_egress_embed(&cfg, up, &["a chunk of the journal".to_string()], "query");
        assert!(
            !log.exists() || std::fs::read_to_string(&log).unwrap().is_empty(),
            "a local request must leave the egress log untouched"
        );
        let _ = std::fs::remove_file(&log);
    }

    /// The other half: the skip must be conditional, not a silent disabling of
    /// the log. A test that only asserted the local case would pass if
    /// `note_egress` had simply been broken.
    #[test]
    fn a_remote_upstream_still_writes_one() {
        let log = tmp("remote");
        let cfg = cfg_for("https://integrate.api.nvidia.com/v1", &log);
        let up = cfg.primary();
        assert_eq!(up.locality, Locality::Remote);
        note_egress(&cfg, up, 0, 1, "why did my build fail", "you are hadal");
        let body = std::fs::read_to_string(&log).expect("egress log must exist");
        assert!(body.contains("upstream=https://integrate.api.nvidia.com/v1"));
        assert!(body.contains("prompt_bytes=21"));
        let _ = std::fs::remove_file(&log);
    }

    /// The failure this test exists for: a chain that walks past two providers
    /// and is answered by the third has sent the journal excerpt to three
    /// companies, and the egress log is the only place that fact is recorded.
    /// One line per *attempt*, not one per request.
    #[test]
    fn every_attempt_in_the_chain_is_recorded() {
        let log = tmp("chain");
        let cfg = Config::from_args(
            [
                "hadald",
                "--model",
                "primary-model",
                "--upstream",
                "https://a.example/v1",
                "--fallback",
                "https://b.example/v1,b-model,/dev/null",
                "--fallback",
                "http://127.0.0.1:8080/v1,local-model",
                "--egress-log",
                log.to_str().unwrap(),
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .expect("config");
        assert_eq!(cfg.chain.len(), 3);

        for (i, up) in cfg.chain.iter().enumerate() {
            note_egress(&cfg, up, i, cfg.chain.len(), "why did my build fail", "sys");
        }

        let body = std::fs::read_to_string(&log).expect("egress log must exist");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "two remote attempts, and the local one must not appear");
        assert!(lines[0].contains("upstream=https://a.example/v1"));
        assert!(lines[0].contains("model=primary-model"));
        assert!(lines[0].contains("attempt=1/3"));
        assert!(lines[1].contains("upstream=https://b.example/v1"));
        // The fallback's own model id, not the primary's. Logging the primary's
        // here would misreport what the second provider was actually asked for.
        assert!(lines[1].contains("model=b-model"));
        assert!(lines[1].contains("attempt=2/3"));
        assert!(!body.contains("127.0.0.1"), "a local link must not appear in an egress log");
        let _ = std::fs::remove_file(&log);
    }
}
