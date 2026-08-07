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
    /// Text to forward to the broker.
    Text(String),
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
                if let Some(text) = choice.delta.content {
                    if !text.is_empty() {
                        out.push(Delta::Text(text));
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
