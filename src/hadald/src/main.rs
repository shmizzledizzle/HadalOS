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
mod upstream;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use config::Config;
use upstream::{Delta, SseDecoder};

struct Ctx {
    cfg: Config,
    key: String,
    http: reqwest::Client,
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
                 [--upstream URL] [--egress-log PATH] [--log-bodies]"
            );
            return std::process::ExitCode::from(2);
        }
    };

    let key = match config::read_key(&cfg.key_file) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("hadald: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let ctx = Arc::new(Ctx {
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .expect("http client"),
        cfg: cfg.clone(),
        key,
    });

    let app = Router::new()
        .route("/api/tags", get(tags))
        .route("/api/generate", post(generate))
        .route("/api/embed", post(embed))
        .with_state(ctx);

    let listener = match tokio::net::TcpListener::bind(cfg.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hadald: cannot bind {}: {e}", cfg.listen);
            return std::process::ExitCode::FAILURE;
        }
    };

    tracing::info!(
        "hadald listening on {} — model {} via {}",
        cfg.listen,
        cfg.model,
        cfg.upstream
    );
    if cfg.egress_log.is_none() {
        tracing::warn!(
            "no --egress-log: outbound prompts will not be recorded anywhere. \
             This daemon sends system logs to a third party."
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
async fn tags(State(ctx): State<Arc<Ctx>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "models": [{
            "name": ctx.cfg.model,
            "model": ctx.cfg.model,
            "details": { "family": "hadald-upstream", "upstream": ctx.cfg.upstream },
        }]
    }))
}

/// Record one outbound request. Appends; never truncates.
fn note_egress(cfg: &Config, prompt: &str, system: &str) {
    let Some(path) = &cfg.egress_log else { return };
    use std::io::Write;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut line = format!(
        "{stamp} model={} upstream={} prompt_bytes={} system_bytes={}\n",
        cfg.model,
        cfg.upstream,
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
async fn embed(
    State(ctx): State<Arc<Ctx>>,
    Json(req): Json<EmbedRequest>,
) -> Result<Response, Response> {
    if req.input.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty input").into_response());
    }

    let (body, kind) = upstream::embed_body(&ctx.cfg.embed_model, &req.input);
    note_egress_embed(&ctx.cfg, &req.input, kind.as_str());

    let resp = ctx
        .http
        .post(format!("{}/embeddings", ctx.cfg.upstream))
        .bearer_auth(&ctx.key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("upstream unreachable: {e}");
            (StatusCode::BAD_GATEWAY, format!("upstream unreachable: {e}")).into_response()
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        tracing::error!("embeddings upstream returned {status}");
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

fn note_egress_embed(cfg: &Config, inputs: &[String], kind: &str) {
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
        cfg.upstream,
        inputs.len()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

async fn generate(
    State(ctx): State<Arc<Ctx>>,
    Json(req): Json<GenerateRequest>,
) -> Result<Response, Response> {
    if req.prompt.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty prompt").into_response());
    }

    // The broker names a model; hadald serves exactly one. Say so rather than
    // silently answering as something else.
    if !req.model.is_empty() && req.model != ctx.cfg.model {
        tracing::warn!(
            "broker asked for model '{}', serving '{}'",
            req.model,
            ctx.cfg.model
        );
    }

    note_egress(&ctx.cfg, &req.prompt, &req.system);

    let body = upstream::chat_body(&ctx.cfg.model, &req.system, &req.prompt);
    let resp = ctx
        .http
        .post(format!("{}/chat/completions", ctx.cfg.upstream))
        .bearer_auth(&ctx.key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("upstream unreachable: {e}");
            (StatusCode::BAD_GATEWAY, format!("upstream unreachable: {e}")).into_response()
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        tracing::error!("upstream returned {status}: {detail}");
        // Do not forward the upstream body verbatim — it can echo the request,
        // and the request is the thing we are being careful about.
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("upstream returned {status}"),
        )
            .into_response());
    }

    // Translate SSE to the newline-delimited JSON the broker parses.
    let stream = async_stream::stream(resp);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .body(Body::from_stream(stream))
        .expect("response"))
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
}
