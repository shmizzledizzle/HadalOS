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
                for ev in scanner.feed(&parsed.response) {
                    on_event(ev);
                }
                if parsed.done {
                    for ev in scanner.finish() {
                        on_event(ev);
                    }
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
