//! The outward half: an OpenAI-compatible client, and the translation between
//! its SSE stream and the newline-delimited JSON `hadal-brokerd` expects.
//!
//! Everything that leaves this machine leaves from here. That is the whole
//! reason the file exists separately — "which code can talk to the internet"
//! should be answerable by reading one filename, not by auditing a crate.

use serde::Deserialize;

/// One decoded delta from the upstream stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Delta {
    /// Text to forward to the broker as the answer.
    Text(String),
    /// The model's private working-out, forwarded as a *separate* kind.
    ///
    /// Reasoning models — Nemotron Super 49B among them — emit a long stretch
    /// of internal monologue before any answer. Measured on the reference
    /// laptop: 103 SSE frames, of which 49 were reasoning and 54 were content,
    /// so roughly half a 220-second request produced nothing the user could
    /// see and the daemon looked hung.
    ///
    /// This must never be merged into `Text`. The broker feeds `Text` to
    /// `ProposalScanner`, and reasoning is exactly where a model rehearses
    /// action blocks it has not committed to — "I could propose
    /// ```hadal-action …" in the middle of thinking out loud would become a
    /// real proposal the model never actually made. Keeping the two variants
    /// distinct is what makes that structurally impossible rather than a thing
    /// the prompt has to discourage.
    Reasoning(String),
    /// The upstream said it is finished.
    Done,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    /// The field DeepSeek established and most reasoning models copied.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// NVIDIA sends both `reasoning` and `reasoning_content`, byte-identical.
    /// Read as a fallback so a provider that ships only the short name still
    /// produces a visible stream rather than silence.
    #[serde(default)]
    reasoning: Option<String>,
}

impl ChatDelta {
    /// Whichever reasoning field this provider populated.
    ///
    /// Never both: NVIDIA duplicates them, so taking each in turn would emit
    /// every thought twice.
    fn reasoning_text(&self) -> Option<&str> {
        self.reasoning_content
            .as_deref()
            .or(self.reasoning.as_deref())
            .filter(|s| !s.is_empty())
    }
}

/// Incremental SSE decoder.
///
/// Server-sent events are `data: <payload>` lines terminated by a blank line,
/// and a single TCP read can split a line anywhere — including mid-UTF-8, which
/// is why the caller feeds `&str` assembled with `from_utf8_lossy` rather than
/// this type owning the socket. Modelled on the broker's own `ProposalScanner`,
/// which has the same problem one layer up.
#[derive(Default)]
pub struct SseDecoder {
    buf: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk, get zero or more deltas.
    pub fn feed(&mut self, chunk: &str) -> Vec<Delta> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();

        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim();

            // Blank lines separate events; comments begin with ':'.
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();

            if payload == "[DONE]" {
                out.push(Delta::Done);
                continue;
            }

            // A chunk that does not parse is dropped rather than fatal. The
            // upstream is a third party and may add fields or emit keepalives;
            // failing the whole generation over one unrecognised frame would
            // turn a cosmetic upstream change into an outage.
            let Ok(parsed) = serde_json::from_str::<ChatChunk>(payload) else {
                tracing::debug!("unparsed SSE payload ({} bytes)", payload.len());
                continue;
            };

            for choice in parsed.choices {
                // Reasoning first: it precedes the answer in every stream that
                // has both, and a frame carrying both fields is the model
                // finishing a thought and starting to speak in the same breath.
                if let Some(thought) = choice.delta.reasoning_text() {
                    out.push(Delta::Reasoning(thought.to_string()));
                }
                if let Some(text) = &choice.delta.content {
                    if !text.is_empty() {
                        out.push(Delta::Text(text.clone()));
                    }
                }
                if choice.finish_reason.is_some() {
                    out.push(Delta::Done);
                }
            }
        }
        out
    }
}

/// Whether a failed status justifies trying the next link in the chain.
///
/// The distinction is whether the *endpoint* failed or the *request* is wrong.
/// A wrong request is wrong everywhere, and walking a five-link chain to
/// collect five identical 400s turns one fast error into five slow ones while
/// sending the prompt to four more third parties than necessary — which is a
/// privacy cost, not just a latency one.
///
/// - `429` is the case this whole mechanism exists for: free tiers cap by day
///   or by minute, and the cap is per provider, so the next one is unaffected.
/// - `401`/`403` mean this link's key is wrong or expired. Retryable, because
///   the chain should still answer, but logged at `warn` — it is a standing
///   misconfiguration that will not fix itself, and a chain quietly running one
///   link short is how you discover the outage only when the last link goes.
/// - `404` usually means the model id retired out from under the config, which
///   free tiers do without notice. The next link serves a different id.
/// - `413` and `5xx` are per-endpoint by nature — context windows differ, and
///   someone else's bad day is not ours.
/// - `400` is ours to fix. `chat_body` built it, so no other endpoint will like
///   it any better.
pub fn is_retryable(status: u16) -> bool {
    match status {
        400 => false,
        401 | 403 | 404 | 408 | 409 | 413 | 429 => true,
        s => s >= 500,
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    /// The reason the chain exists. A daily cap at one provider says nothing
    /// about the next.
    #[test]
    fn rate_limits_and_outages_move_to_the_next_link() {
        for s in [429, 500, 502, 503, 504, 529] {
            assert!(is_retryable(s), "{s} should fail over");
        }
    }

    /// A standing misconfiguration should still be answered by the chain — but
    /// `generate` logs these differently, because they will not clear on their
    /// own the way a 429 does.
    #[test]
    fn a_dead_key_or_retired_model_moves_on() {
        for s in [401, 403, 404] {
            assert!(is_retryable(s), "{s} should fail over");
        }
    }

    /// The one that must not walk the chain. A 400 is `chat_body`'s fault, so
    /// every link will reject it identically — and each attempt ships the
    /// prompt to another third party for nothing.
    ///
    /// The exception is handled in `generate` rather than here: a
    /// context-length 400 describes the *link*, not the request, and does
    /// advance the chain. `is_retryable` stays a pure function of the status
    /// code, because the body is what distinguishes the two cases.
    #[test]
    fn a_malformed_request_stops_at_the_first_link() {
        assert!(!is_retryable(400));
    }

    /// Verbatim from integrate.api.nvidia.com, 2026-08-25, on a 707 KB Portage
    /// log — the rejection that started all of this.
    const NVIDIA_OVERFLOW: &str = "This model's maximum context length is 131072 tokens. \
        However, you requested 2048 output tokens and your prompt contains at least 129025 \
        input tokens, for a total of at least 131073 tokens.";

    #[test]
    fn the_window_is_read_from_the_phrase_that_names_it() {
        let s = summarise_context_error(NVIDIA_OVERFLOW);
        assert!(s.contains("131073"), "the total sent: {s}");
        assert!(s.contains("limit of 131072"), "the window, not the output reserve: {s}");
        // 2048 is the reserved output, and was previously reported as the
        // limit — telling the operator to cut a log 64x when it was over by one
        // token. Any summary naming it is wrong.
        assert!(!s.contains("2048"), "the output reservation is not the limit: {s}");
    }

    /// The same message from a narrower free tier, which is the shape the
    /// heterogeneous chain will actually produce.
    #[test]
    fn a_narrower_window_is_reported_as_its_own_number() {
        let s = summarise_context_error(
            "This model's maximum context length is 65536 tokens. However, you requested \
             2048 output tokens and your prompt contains at least 87931 input tokens, for a \
             total of at least 89979 tokens.",
        );
        assert!(s.contains("limit of 65536"), "{s}");
        assert!(s.contains("89979"), "{s}");
    }

    /// No anchor phrase, so there is no way to know which figure is the window.
    /// It must degrade to the one true statement rather than guess.
    #[test]
    fn an_unrecognised_shape_does_not_invent_a_limit() {
        let s = summarise_context_error("context_length_exceeded: 40000 tokens");
        assert!(s.contains("40000") && !s.contains("limit of"), "{s}");
    }

    /// The rule the carve-out is a hole in: digits and hadald's own words only.
    #[test]
    fn the_summary_never_quotes_the_upstream_prose() {
        let s = summarise_context_error(NVIDIA_OVERFLOW);
        assert!(!s.contains("However, you requested"), "upstream prose leaked: {s}");
        assert!(!s.contains("This model's"), "upstream prose leaked: {s}");
    }

    /// Verbatim from the live NVIDIA endpoint, 2026-08-25, reproducing the
    /// failure that made `hadal explain` unusable. Pinned as a fixture so a
    /// rewording of the matcher cannot quietly stop recognising the one error
    /// this daemon is most likely to receive.
    const NVIDIA_400: &str = "{\"error\":{\"message\":\"This model's maximum context \
        length is 131072 tokens. However, you requested 2048 output tokens and your prompt \
        contains at least 129025 input tokens, for a total of at least 131073 tokens. Please \
        reduce the length of the input prompt or the number of requested output tokens. \
        (parameter=input_tokens, value=129025)\",\"type\":\"BadRequestError\",\
        \"param\":\"input_tokens\",\"code\":400}}";

    #[test]
    fn recognises_a_context_length_rejection() {
        assert!(is_context_length_error(NVIDIA_400));
        assert!(is_context_length_error("error: context_length_exceeded"));
        assert!(is_context_length_error("prompt exceeds context size"));
    }

    /// An ordinary 400 must keep the old opaque path — the carve-out is for
    /// arithmetic, and widening it would start forwarding bodies that can echo
    /// the prompt.
    #[test]
    fn an_ordinary_400_is_not_mistaken_for_one() {
        assert!(!is_context_length_error("{\"error\":\"invalid model id\"}"));
        assert!(!is_context_length_error("temperature must be <= 2"));
    }

    /// Verbatim frame shape from NVIDIA Nemotron, 2026-08-25. Both fields are
    /// populated and byte-identical, which is the case that would double every
    /// thought if they were read independently.
    #[test]
    fn reasoning_is_decoded_once_not_twice() {
        let mut d = SseDecoder::new();
        let out = d.feed(
            "data: {\"choices\":[{\"delta\":{\"content\":null,\
             \"reasoning\":\"Okay, the user\",\
             \"reasoning_content\":\"Okay, the user\"}}]}\n\n",
        );
        assert_eq!(out, vec![Delta::Reasoning("Okay, the user".into())]);
    }

    /// A provider that ships only the short field still produces a stream.
    #[test]
    fn the_short_reasoning_field_is_a_fallback() {
        let mut d = SseDecoder::new();
        let out = d.feed("data: {\"choices\":[{\"delta\":{\"reasoning\":\"hm\"}}]}\n\n");
        assert_eq!(out, vec![Delta::Reasoning("hm".into())]);
    }

    /// The safety property, stated as a test.
    ///
    /// A model rehearsing an action block inside its private reasoning must not
    /// produce anything the broker would scan. If this ever yields
    /// `Delta::Text`, a thought becomes a proposal — well-typed, accepted by
    /// `action.rs`, and carried to a polkit prompt the model never asked for.
    #[test]
    fn a_rehearsed_action_block_in_reasoning_is_never_text() {
        let mut d = SseDecoder::new();
        let out = d.feed(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\
             \"I could propose ```hadal-action\\n{\\\"action\\\":\\\"read-journal\\\"}\\n```\"}}]}\n\n",
        );
        assert_eq!(out.len(), 1);
        assert!(
            matches!(out[0], Delta::Reasoning(_)),
            "reasoning containing a fence must stay Reasoning, got {:?}",
            out[0]
        );
    }

    /// One frame can carry the end of a thought and the start of the answer.
    /// Both must come out, in that order, as different kinds.
    #[test]
    fn a_frame_carrying_both_emits_both_in_order() {
        let mut d = SseDecoder::new();
        let out = d.feed(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"...so\",\
             \"content\":\"The build failed\"}}]}\n\n",
        );
        assert_eq!(
            out,
            vec![
                Delta::Reasoning("...so".into()),
                Delta::Text("The build failed".into()),
            ]
        );
    }

    /// The summary carries numbers, not the upstream's prose — the prompt must
    /// not come back out through the error path.
    #[test]
    fn the_summary_reports_size_without_echoing_the_body() {
        let s = summarise_context_error(NVIDIA_400);
        assert!(s.contains("131073"), "should name the total: {s}");
        assert!(!s.contains("However"), "must not quote the upstream: {s}");
        assert!(!s.to_lowercase().contains("please reduce"));
    }
}

/// Whether a 400 body is the endpoint saying "your prompt is too long".
///
/// Matched on wording rather than a code because the OpenAI-compatible
/// ecosystem has no shared one: NVIDIA returns `type: BadRequestError` with
/// `param: input_tokens`, others use `code: context_length_exceeded`, llama.cpp
/// says "exceeds context size". The phrasings below cover what the shipped
/// chain actually returns; an unmatched one degrades to the ordinary 400 path,
/// which is the pre-existing behaviour and not a regression.
pub fn is_context_length_error(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("context_length_exceeded")
        || d.contains("input_tokens")
        || (d.contains("context") && (d.contains("maximum") || d.contains("exceed")))
        || d.contains("too many tokens")
}

/// Pull the numbers out of a context-length rejection, discarding prose.
///
/// Returns only digits and the words around them that the caller needs to act:
/// how big the window is and how far over the prompt went. Deliberately does
/// not return the upstream's message verbatim — that is what keeps the "never
/// forward an upstream body" rule intact while still answering the question the
/// user actually has, which is "by how much?".
pub fn summarise_context_error(detail: &str) -> String {
    let lower = detail.to_ascii_lowercase();

    let nums = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .filter(|n| *n >= 1000)
            .collect()
    };

    // The window has to be read from the phrase that names it, not inferred as
    // "the smallest number present". These messages carry four figures — the
    // limit, the reserved output, the input, and the total — and the smallest
    // is the output reservation. Guessing produced "roughly 89979 tokens were
    // sent against a limit near 2048", which is wrong in the one direction that
    // matters: it tells the operator to shorten a log by 40x when the real
    // overage is 37%.
    let limit = ["maximum context length is", "context length is", "context window is"]
        .iter()
        .find_map(|anchor| {
            let at = lower.find(anchor)? + anchor.len();
            nums(&lower[at..]).into_iter().next()
        });

    let total = nums(&lower).into_iter().max();

    match (total, limit) {
        (Some(hi), Some(lo)) if hi > lo => {
            format!("Roughly {hi} tokens were sent against a limit of {lo}. Shorten the log.")
        }
        (Some(hi), _) => format!("Roughly {hi} tokens were involved. Shorten the log."),
        _ => "Shorten the log.".to_string(),
    }
}

/// Build the upstream request body.
///
/// The system prompt arrives from the broker — `ACTION_PROTOCOL` in
/// `model.rs` — and is passed through untouched. hadald deliberately does not
/// author or edit it: the schema the model is shown and the schema the broker
/// accepts have to be the same string, and the broker owns it.
pub fn chat_body(model: &str, system: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": prompt },
        ],
        "stream": true,
        "temperature": 0.2,
        "max_tokens": 2048,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Embeddings
// ─────────────────────────────────────────────────────────────────────────

/// Retrieval models embed a question and a document differently, and mixing
/// the two costs real accuracy. Ollama's `nomic-embed-text` signals this with
/// a `search_document:` / `search_query:` text prefix; NVIDIA's embedqa models
/// use an `input_type` field instead.
///
/// Translating between them here means Hadal's `build_index.py` and
/// `search.py` keep working unmodified — they already emit the nomic prefixes,
/// and those carry exactly the intent `input_type` needs.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputType {
    Passage,
    Query,
}

impl InputType {
    pub fn as_str(self) -> &'static str {
        match self {
            InputType::Passage => "passage",
            InputType::Query => "query",
        }
    }
}

/// Strip a nomic-style prefix, returning the intent it encoded.
///
/// Defaults to `Passage`: indexing is the bulk operation, and mislabelling a
/// query as a passage degrades one search, whereas mislabelling every document
/// degrades the whole index.
pub fn split_input_type(text: &str) -> (InputType, &str) {
    for (prefix, kind) in [
        ("search_query:", InputType::Query),
        ("search_document:", InputType::Passage),
        ("query:", InputType::Query),
        ("passage:", InputType::Passage),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (kind, rest.trim_start());
        }
    }
    (InputType::Passage, text)
}

/// Build an OpenAI-compatible embeddings request.
///
/// A batch must be one `input_type`. Callers that mix intents get the majority
/// label, which is fine because real callers never mix: indexing sends
/// documents, searching sends one query.
pub fn embed_body(model: &str, inputs: &[String]) -> (serde_json::Value, InputType) {
    let mut kind = InputType::Passage;
    let stripped: Vec<String> = inputs
        .iter()
        .map(|t| {
            let (k, rest) = split_input_type(t);
            if k == InputType::Query {
                kind = InputType::Query;
            }
            rest.to_string()
        })
        .collect();

    (
        serde_json::json!({
            "model": model,
            "input": stripped,
            "input_type": kind.as_str(),
            "encoding_format": "float",
        }),
        kind,
    )
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingsResponse {
    pub data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingItem {
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub index: usize,
}

impl EmbeddingsResponse {
    /// Vectors in the caller's original order.
    ///
    /// The upstream returns an `index` per item and is not obliged to return
    /// them in order. Sorting on it costs nothing; assuming order silently
    /// mismatches every chunk with the wrong text, and an index built that way
    /// looks fine and retrieves nonsense.
    pub fn ordered(mut self) -> Vec<Vec<f32>> {
        self.data.sort_by_key(|d| d.index);
        self.data.into_iter().map(|d| d.embedding).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(content: &str) -> String {
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content":content}}]})
        )
    }

    #[test]
    fn decodes_a_simple_stream() {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        out.extend(d.feed(&frame("Hello")));
        out.extend(d.feed(&frame(" world")));
        out.extend(d.feed("data: [DONE]\n\n"));
        assert_eq!(
            out,
            vec![
                Delta::Text("Hello".into()),
                Delta::Text(" world".into()),
                Delta::Done
            ]
        );
    }

    /// The case that breaks naive decoders: a frame split across reads.
    #[test]
    fn handles_a_frame_split_across_chunks() {
        let whole = frame("split me");
        let (a, b) = whole.split_at(whole.len() / 2);
        let mut d = SseDecoder::new();
        let mut out = d.feed(a);
        assert!(out.is_empty(), "must not emit from a partial line");
        out.extend(d.feed(b));
        assert_eq!(out, vec![Delta::Text("split me".into())]);
    }

    #[test]
    fn several_frames_in_one_chunk() {
        let mut d = SseDecoder::new();
        let out = d.feed(&format!("{}{}", frame("a"), frame("b")));
        assert_eq!(out, vec![Delta::Text("a".into()), Delta::Text("b".into())]);
    }

    #[test]
    fn keepalives_and_comments_are_ignored() {
        let mut d = SseDecoder::new();
        assert!(d.feed(": ping\n\n").is_empty());
        assert!(d.feed("\n\n").is_empty());
    }

    /// An unrecognised frame must not abort the generation.
    #[test]
    fn unparsable_frames_are_skipped_not_fatal() {
        let mut d = SseDecoder::new();
        let mut out = d.feed("data: {not json at all\n\n");
        assert!(out.is_empty());
        out.extend(d.feed(&frame("still here")));
        assert_eq!(out, vec![Delta::Text("still here".into())]);
    }

    #[test]
    fn finish_reason_ends_the_stream() {
        let mut d = SseDecoder::new();
        let out = d.feed(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        assert_eq!(out, vec![Delta::Done]);
    }

    #[test]
    fn empty_content_deltas_produce_nothing() {
        let mut d = SseDecoder::new();
        assert!(d.feed(&frame("")).is_empty());
    }

    /// The action fence must survive the round trip intact — it is the only
    /// thing in the stream with meaning to the broker.
    #[test]
    fn an_action_fence_survives_token_splitting() {
        let text = "I need the log.\n```hadal-action\n{\"action\":\"read-journal\"}\n```\n";
        let mut d = SseDecoder::new();
        let mut got = String::new();
        // One character per frame, the worst case a tokeniser can produce.
        for ch in text.chars() {
            for delta in d.feed(&frame(&ch.to_string())) {
                if let Delta::Text(t) = delta {
                    got.push_str(&t);
                }
            }
        }
        assert_eq!(got, text);
    }

    #[test]
    fn nomic_prefixes_become_input_type() {
        assert_eq!(split_input_type("search_document: hello"), (InputType::Passage, "hello"));
        assert_eq!(split_input_type("search_query: hello"), (InputType::Query, "hello"));
        // No prefix: treated as a document, because indexing is the bulk case.
        assert_eq!(split_input_type("hello"), (InputType::Passage, "hello"));
    }

    #[test]
    fn embed_body_strips_prefixes_and_labels_the_batch() {
        let (b, kind) = embed_body(
            "m",
            &["search_document: alpha".into(), "search_document: beta".into()],
        );
        assert_eq!(kind, InputType::Passage);
        assert_eq!(b["input_type"], "passage");
        assert_eq!(b["input"][0], "alpha");
        assert_eq!(b["input"][1], "beta");

        let (b, kind) = embed_body("m", &["search_query: what broke?".into()]);
        assert_eq!(kind, InputType::Query);
        assert_eq!(b["input_type"], "query");
        assert_eq!(b["input"][0], "what broke?");
    }

    /// Out-of-order upstream results must not silently pair vectors with the
    /// wrong text — an index built that way looks healthy and retrieves noise.
    #[test]
    fn embeddings_are_returned_in_request_order() {
        let raw = r#"{"data":[
            {"embedding":[3.0],"index":2},
            {"embedding":[1.0],"index":0},
            {"embedding":[2.0],"index":1}]}"#;
        let parsed: EmbeddingsResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.ordered(), vec![vec![1.0], vec![2.0], vec![3.0]]);
    }

    #[test]
    fn body_passes_the_system_prompt_through_untouched() {
        let sys = "SYSTEM PROMPT VERBATIM";
        let b = chat_body("m", sys, "p");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], sys);
        assert_eq!(b["messages"][1]["content"], "p");
        assert_eq!(b["stream"], true);
    }
}
