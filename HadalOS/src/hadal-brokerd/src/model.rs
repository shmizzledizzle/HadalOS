//! Client for `hadald`, and the scanner that separates prose from proposals.
//!
//! The broker reaches hadald over plaintext HTTP on loopback. That is safe
//! precisely because both processes sit in the same network namespace with no
//! route out — see `systemd/hadald.service`.

use serde::Deserialize;
use std::time::Duration;

use crate::action::Action;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";

/// What the model is told it may ask for. Kept in this file so the schema the
/// model sees and the schema `action.rs` accepts are edited together — they
/// drift apart the moment they live in different places.
pub const ACTION_PROTOCOL: &str = r#"
You are Hadal, the assistant built into HadalOS. You run entirely on this
machine. You have no network access.

You cannot run commands. You cannot read files directly. What you can do is
PROPOSE an action, which the user then sees and confirms. Propose one only
when you actually need its result, or when the user has asked for the change.

To propose, emit a fenced block exactly like this:

```hadal-action
{"action": "read-portage-log", "path": "/var/log/portage/build.log"}
```

Available actions and their exact parameters:

  {"action":"read-journal", "unit":"<name>.service"?, "boot":"current"|"previous"?, "lines":<1-10000>?}
  {"action":"read-portage-log", "path":"<absolute path>"}
  {"action":"read-path", "path":"<absolute path>"}
  {"action":"query-package", "atom":"<category/name>"}
  {"action":"unit-status", "unit":"<name>.service"}
  {"action":"emerge-pretend", "atoms":["<category/name>", ...]}
  {"action":"restart-unit", "unit":"<name>.service"}
  {"action":"emerge-apply", "atoms":["<category/name>", ...], "mode":"install"|"oneshot"|"depclean"?}
  {"action":"write-config", "change":{"kind":"portage-use","atom":"<category/name>","flags":["flag","-flag"]}}
  {"action":"write-config", "change":{"kind":"portage-accept-keywords","atom":"<category/name>","keyword":"~amd64"}}
  {"action":"write-config", "change":{"kind":"portage-mask","atom":"<category/name>"}}

Rules:
  - Unit names must carry their suffix. `hadald` is not a unit; `hadald.service` is.
  - Package atoms are category/name, optionally with a version operator.
  - There is no action for running arbitrary commands. Do not invent one, and
    do not write shell for the user to paste as a substitute for a proposal.
  - One action per block. Explain your reasoning in prose before the block.
  - If a block is malformed it is discarded silently and you will not be told,
    so emit exactly the shapes above.
"#;

#[derive(Debug, Deserialize)]
struct GenerateChunk {
    #[serde(default)]
    response: String,
    /// The model's working-out, when the upstream is a reasoning model.
    ///
    /// Separate from `response` all the way down, and deliberately so: this
    /// text never reaches `ProposalScanner`. Reasoning is where a model tries
    /// on action blocks it has not decided to emit, and scanning it would turn
    /// "I could propose ```hadal-action …" into a proposal the model never
    /// made — one that would then be well-typed, pass `action.rs`, and reach a
    /// polkit prompt. The type system carries that boundary rather than the
    /// prompt being asked to.
    #[serde(default)]
    thinking: String,
    #[serde(default)]
    done: bool,
}

#[derive(Debug)]
pub enum ModelError {
    Unreachable(String),
    Protocol(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Unreachable(e) => write!(f, "hadald is not reachable: {e}"),
            ModelError::Protocol(e) => write!(f, "hadald returned an unexpected response: {e}"),
        }
    }
}

pub struct ModelClient {
    http: reqwest::Client,
    endpoint: String,
}

/// What the scanner hands back as it walks the stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Prose, for the `Delta` signal.
    Text(String),
    /// The model's working-out, for the `Thinking` signal.
    ///
    /// Never produced by `ProposalScanner` — it arrives beside the scanner, not
    /// through it, which is what guarantees a rehearsed action block in the
    /// model's private reasoning cannot become a real proposal.
    Thinking(String),
    /// A complete, validated proposal.
    Proposal(Action),
    /// A block that did not parse. Not shown to the user as an action; logged
    /// so the persona can be fixed when the model keeps getting it wrong.
    Malformed(String),
}

impl ModelClient {
    pub fn new(endpoint: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .expect("http client"),
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
        }
    }

    pub async fn ready(&self) -> bool {
        self.http
            .get(format!("{}/api/tags", self.endpoint))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Fetch reference passages for a question.
    ///
    /// Returns `None` when there is no index, when retrieval fails, or when
    /// nothing matched. Retrieval is an enhancement: the broker must still
    /// answer without it, and a documentation lookup that is down is not a
    /// reason to refuse a question.
    ///
    /// The broker calls this and decides where the result goes. hadald only
    /// ranks — see `retrieve.rs` there for why that split is acceptable, and
    /// for the condition under which it stops being.
    pub async fn retrieve(&self, query: &str, k: usize) -> Option<String> {
        #[derive(Deserialize)]
        struct RetrieveReply {
            #[serde(default)]
            text: String,
        }

        let resp = self
            .http
            .post(format!("{}/api/retrieve", self.endpoint))
            .timeout(Duration::from_secs(60))
            .json(&serde_json::json!({ "query": query, "k": k }))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::warn!("retrieval returned {}; answering without it", resp.status());
            return None;
        }
        let reply: RetrieveReply = resp.json().await.ok()?;
        let text = reply.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        Some(text)
    }

    /// Streams a generation, invoking `on_event` for each scanned event.
    pub async fn generate<F>(
        &self,
        model: &str,
        prompt: &str,
        mut on_event: F,
    ) -> Result<(), ModelError>
    where
        F: FnMut(Event),
    {
        use futures_util::StreamExt;

        let body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "system": ACTION_PROTOCOL,
            "stream": true,
            "keep_alive": "30m",
        });

        let resp = self
            .http
            .post(format!("{}/api/generate", self.endpoint))
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Unreachable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ModelError::Protocol(format!("HTTP {}", resp.status())));
        }

        let mut scanner = ProposalScanner::new();
        let mut stream = resp.bytes_stream();
        let mut line_buf = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ModelError::Unreachable(e.to_string()))?;
            line_buf.push_str(&String::from_utf8_lossy(&chunk));

            // Ollama emits newline-delimited JSON; a chunk may split a line.
            while let Some(nl) = line_buf.find('\n') {
                let line: String = line_buf.drain(..=nl).collect();
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let parsed: GenerateChunk = match serde_json::from_str(line) {
                    Ok(c) => c,
                    Err(e) => return Err(ModelError::Protocol(e.to_string())),
                };
                if dispatch_chunk(&mut scanner, &parsed, &mut on_event) {
                    return Ok(());
                }
            }
        }

        for ev in scanner.finish() {
            on_event(ev);
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────

const FENCE_OPEN: &str = "```hadal-action";
const FENCE_CLOSE: &str = "```";

/// Splits a token stream into prose and action blocks.
///
/// Incremental because the fence markers routinely straddle chunk boundaries —
/// a model emits "``" and "`hadal" as separate tokens often enough that a
/// naive per-chunk search would miss most proposals and leak the JSON into the
/// user-visible text.
pub struct ProposalScanner {
    buf: String,
    inside: bool,
}

impl Default for ProposalScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProposalScanner {
    pub fn new() -> Self {
        Self { buf: String::new(), inside: false }
    }

    pub fn feed(&mut self, chunk: &str) -> Vec<Event> {
        self.buf.push_str(chunk);
        self.drain(false)
    }

    /// Flush at end of stream. An unterminated block is malformed, not prose:
    /// emitting a half-written action as text would show the user raw JSON.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = self.drain(true);
        if self.inside && !self.buf.is_empty() {
            events.push(Event::Malformed(std::mem::take(&mut self.buf)));
            self.inside = false;
        } else if !self.buf.is_empty() {
            events.push(Event::Text(std::mem::take(&mut self.buf)));
        }
        events
    }

    fn drain(&mut self, flushing: bool) -> Vec<Event> {
        let mut events = Vec::new();

        loop {
            if self.inside {
                let Some(end) = self.buf.find(FENCE_CLOSE) else { break };
                let block: String = self.buf.drain(..end).collect();
                self.buf.drain(..FENCE_CLOSE.len().min(self.buf.len()));
                self.inside = false;
                events.push(parse_block(&block));
            } else {
                match self.buf.find(FENCE_OPEN) {
                    Some(start) => {
                        let text: String = self.buf.drain(..start).collect();
                        if !text.is_empty() {
                            events.push(Event::Text(text));
                        }
                        self.buf.drain(..FENCE_OPEN.len());
                        if self.buf.starts_with('\n') {
                            self.buf.drain(..1);
                        }
                        self.inside = true;
                    }
                    None => {
                        // Hold back enough to recognise a fence marker split
                        // across chunks; release the rest as prose.
                        let keep = if flushing { 0 } else { FENCE_OPEN.len() - 1 };
                        if self.buf.len() > keep {
                            let cut = self.buf.len() - keep;
                            let cut = (0..=cut)
                                .rev()
                                .find(|i| self.buf.is_char_boundary(*i))
                                .unwrap_or(0);
                            let text: String = self.buf.drain(..cut).collect();
                            if !text.is_empty() {
                                events.push(Event::Text(text));
                            }
                        }
                        break;
                    }
                }
            }
        }

        events
    }
}

/// Turns one decoded NDJSON frame into events. Returns true when the stream is
/// finished and the caller should stop reading.
///
/// This is a free function rather than an inline block so the routing decision
/// it encodes — reasoning text goes *around* `ProposalScanner`, never through
/// it — can be tested without standing up an HTTP server. A reasoning trace is
/// the model talking to itself: it quotes protocol syntax, drafts actions it
/// then rejects, and generally contains exactly the fences the scanner is
/// looking for. Feeding it in would let a discarded draft become a real
/// proposal the user is asked to approve.
fn dispatch_chunk<F>(scanner: &mut ProposalScanner, parsed: &GenerateChunk, on_event: &mut F) -> bool
where
    F: FnMut(Event),
{
    if !parsed.thinking.is_empty() {
        on_event(Event::Thinking(parsed.thinking.clone()));
    }
    for ev in scanner.feed(&parsed.response) {
        on_event(ev);
    }
    if parsed.done {
        for ev in scanner.finish() {
            on_event(ev);
        }
        return true;
    }
    false
}

fn parse_block(raw: &str) -> Event {
    let trimmed = raw.trim();
    match serde_json::from_str::<Action>(trimmed) {
        Ok(action) => match action.check_limits() {
            Ok(()) => Event::Proposal(action),
            Err(e) => Event::Malformed(format!("{trimmed}  [limits: {e}]")),
        },
        Err(e) => Event::Malformed(format!("{trimmed}  [parse: {e}]")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(chunks: &[&str]) -> Vec<Event> {
        let mut s = ProposalScanner::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.feed(c));
        }
        out.extend(s.finish());
        out
    }

    /// Runs NDJSON frames through the same dispatch `generate` uses, so what is
    /// under test is the routing decision and not a re-implementation of it.
    fn dispatch_all(lines: &[&str]) -> Vec<Event> {
        let mut scanner = ProposalScanner::new();
        let mut out = Vec::new();
        for line in lines {
            let parsed: GenerateChunk =
                serde_json::from_str(line).expect("test frames must be valid NDJSON");
            if dispatch_chunk(&mut scanner, &parsed, &mut |e| out.push(e)) {
                break;
            }
        }
        out
    }

    /// The safety property behind `GenerateChunk::thinking`.
    ///
    /// A reasoning model drafting an action inside its working-out must not
    /// produce a proposal. The draft here is well-formed on purpose: correctly
    /// fenced, valid JSON, an action `action.rs` would accept. If it ever
    /// reached `ProposalScanner` it would parse cleanly and go on to a polkit
    /// prompt for a deletion the model only considered and then rejected.
    #[test]
    fn a_fenced_action_inside_reasoning_never_becomes_a_proposal() {
        let events = dispatch_all(&[
            r#"{"thinking":"I could remove the stale file:\n```hadal-action\n{\"action\": \"read-portage-log\", \"path\": \"/var/log/portage/build.log\"}\n```\nNo — reading it first is safer."}"#,
            r#"{"response":"The build failed while linking."}"#,
            r#"{"response":"","done":true}"#,
        ]);

        assert!(
            !events.iter().any(|e| matches!(e, Event::Proposal(_))),
            "reasoning must never yield a proposal: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::Malformed(_))),
            "nor should it be reported as a failed one; the scanner never saw it"
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Thinking(t) if t.contains("hadal-action"))),
            "the trace itself still reaches the user verbatim"
        );
        assert!(text_of(&events).contains("failed while linking"));
        assert!(
            !text_of(&events).contains("hadal-action"),
            "and it must not be mistaken for answer text"
        );
    }

    /// The converse: routing reasoning around the scanner must not leave the
    /// scanner unable to see a real proposal that follows it in the same
    /// stream. Nemotron interleaves the two, so this is the common case.
    #[test]
    fn a_real_proposal_after_reasoning_still_parses() {
        let events = dispatch_all(&[
            r#"{"thinking":"Weighing whether to read the log."}"#,
            r#"{"response":"Let me look at the log.\n```hadal-action\n"}"#,
            r#"{"response":"{\"action\": \"read-portage-log\", \"path\": \"/var/log/portage/build.log\"}\n```\n"}"#,
            r#"{"response":"","done":true}"#,
        ]);

        assert!(
            events.iter().any(|e| matches!(e, Event::Proposal(_))),
            "a proposal in `response` must still parse across frames: {events:?}"
        );
    }

    /// Verbatim output from nvidia/llama-3.3-nemotron-super-49b-v1.5, shown a
    /// real failed Portage log with ACTION_PROTOCOL as its system prompt
    /// (2026-08-07). The model reasoned correctly and proposed a sensible
    /// action — and the proposal is lost, because it put the JSON on the fence
    /// line instead of the line after, and appended a stray quote.
    ///
    /// ACTION_PROTOCOL promises malformed blocks are "discarded silently and
    /// you will not be told", so the user sees prose and no proposal, with no
    /// indication one was attempted. This test exists to pin that behaviour
    /// while the format is made more forgiving.
    #[test]
    fn real_model_output_with_json_on_the_fence_line() {
        let observed = concat!(
            "The log tail shows a successful kernel build.\n\n",
            "```hadal-action {\"action\": \"read-portage-log\", \"path\": \"/var/log/portage/build.log\"}\" \n",
            "```\n"
        );
        let events = scan_all(&[observed]);
        let proposals: Vec<_> =
            events.iter().filter(|e| matches!(e, Event::Proposal(_))).collect();
        let malformed: Vec<_> =
            events.iter().filter(|e| matches!(e, Event::Malformed(_))).collect();

        assert!(
            proposals.is_empty(),
            "documenting current behaviour: this shape yields no usable proposal"
        );
        assert!(
            !malformed.is_empty(),
            "it should at least be reported as malformed rather than vanishing entirely"
        );
        // The prose must still reach the user.
        assert!(text_of(&events).contains("successful kernel build"));
    }

    /// The same proposal, formatted as ACTION_PROTOCOL actually specifies.
    /// Confirms the content was always valid and only the framing failed.
    #[test]
    fn the_same_proposal_parses_when_correctly_fenced() {
        let events = scan_all(&[concat!(
            "```hadal-action\n",
            "{\"action\": \"read-portage-log\", \"path\": \"/var/log/portage/build.log\"}\n",
            "```\n"
        )]);
        assert!(
            events.iter().any(|e| matches!(e, Event::Proposal(_))),
            "correctly fenced, the identical action parses"
        );
    }

    fn text_of(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn extracts_a_proposal_and_keeps_surrounding_prose() {
        let events = scan_all(&[
            "Let me look at the build log.\n",
            "```hadal-action\n",
            r#"{"action":"read-portage-log","path":"/var/log/portage/x.log"}"#,
            "\n```",
            "\nThat should show the error.",
        ]);
        assert!(matches!(
            events.iter().find(|e| matches!(e, Event::Proposal(_))),
            Some(Event::Proposal(_))
        ));
        let prose = text_of(&events);
        assert!(prose.contains("Let me look at the build log."));
        assert!(prose.contains("That should show the error."));
        assert!(!prose.contains("read-portage-log"), "JSON must not leak into prose");
    }

    /// The reason the scanner is incremental at all.
    #[test]
    fn handles_fences_split_across_chunks() {
        let events = scan_all(&[
            "checking\n``", "`hadal", "-action\n", r#"{"action":"unit-status","#,
            r#""unit":"hadald.service"}"#, "\n`", "``",
        ]);
        let proposals: Vec<_> =
            events.iter().filter(|e| matches!(e, Event::Proposal(_))).collect();
        assert_eq!(proposals.len(), 1, "got: {events:?}");
        assert!(!text_of(&events).contains("hadal-action"));
    }

    #[test]
    fn injection_shaped_proposals_are_malformed_not_executed() {
        let events = scan_all(&[
            "```hadal-action\n",
            r#"{"action":"emerge-apply","atoms":["sys-boot/limine; rm -rf /"]}"#,
            "\n```",
        ]);
        assert!(events.iter().any(|e| matches!(e, Event::Malformed(_))));
        assert!(!events.iter().any(|e| matches!(e, Event::Proposal(_))));
    }

    #[test]
    fn invented_actions_are_rejected() {
        let events =
            scan_all(&["```hadal-action\n", r#"{"action":"exec","cmd":"sh -c id"}"#, "\n```"]);
        assert!(!events.iter().any(|e| matches!(e, Event::Proposal(_))));
    }

    #[test]
    fn unterminated_block_does_not_leak_as_prose() {
        let events = scan_all(&["here goes\n```hadal-action\n", r#"{"action":"read-jour"#]);
        assert!(events.iter().any(|e| matches!(e, Event::Malformed(_))));
        assert!(!text_of(&events).contains("read-jour"));
    }

    #[test]
    fn plain_prose_passes_through_unchanged() {
        let events = scan_all(&["No action needed — ", "the unit is already running."]);
        assert_eq!(text_of(&events), "No action needed — the unit is already running.");
        assert!(!events.iter().any(|e| matches!(e, Event::Proposal(_))));
    }

    #[test]
    fn multiple_proposals_in_one_reply() {
        let events = scan_all(&[
            "First:\n```hadal-action\n",
            r#"{"action":"unit-status","unit":"hadald.service"}"#,
            "\n```\nThen:\n```hadal-action\n",
            r#"{"action":"read-journal","unit":"hadald.service","lines":50}"#,
            "\n```\n",
        ]);
        let n = events.iter().filter(|e| matches!(e, Event::Proposal(_))).count();
        assert_eq!(n, 2, "got: {events:?}");
    }
}
