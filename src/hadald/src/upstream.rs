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
    fn body_passes_the_system_prompt_through_untouched() {
        let sys = "SYSTEM PROMPT VERBATIM";
        let b = chat_body("m", sys, "p");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], sys);
        assert_eq!(b["messages"][1]["content"], "p");
        assert_eq!(b["stream"], true);
    }
}
