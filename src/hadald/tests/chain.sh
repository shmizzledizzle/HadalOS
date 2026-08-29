#!/usr/bin/env bash
# End-to-end: the --fallback chain, and where it refuses to fail over.
#
# Stands up two fake OpenAI-compatible upstreams that misbehave on request and
# walks hadald through the four cases that matter. Needs no API key and no
# network.
#
# What this file does NOT cover, and why: per-link `Authorization` headers.
# Every upstream here is on loopback, and a loopback link carries no credential
# by design (config.rs, `Upstream::key_file`), so there is no header to assert.
# That the key is indexed per link rather than shared is covered by
# `fallbacks_keep_their_own_model_and_key` in config.rs.
set -uo pipefail

BIN="${1:-target/debug/hadald}"
[[ -x $BIN ]] || { echo "not built: $BIN" >&2; exit 2; }

T="$(mktemp -d)"; PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n $p ]] && kill "$p" 2>/dev/null; done; rm -rf "$T"; }
trap cleanup EXIT
pass=0; fail=0
ok()  { printf 'ok    %s\n' "$*"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s\n' "$*"; fail=$((fail+1)); }

APORT=18535; BPORT=18536; HPORT=18534
for port in $APORT $BPORT $HPORT; do
    if (exec 3<>/dev/tcp/127.0.0.1/$port) 2>/dev/null; then
        echo "port $port already in use — a previous run may still be alive" >&2; exit 2
    fi
done

# ── a fake upstream that fails in whichever way it was told to ────────────
cat > "$T/upstream.py" <<'PY'
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT, MODE, HITS = int(sys.argv[1]), sys.argv[2], sys.argv[3]

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
        # One line per request received, so the test can assert on which links
        # were contacted at all — the failover questions are mostly about which
        # upstreams did NOT see the prompt.
        with open(HITS, 'a') as f:
            f.write(f"{PORT} {MODE} model={body.get('model')}\n")

        if MODE == '400-context':
            # Verbatim shape of the rejection a 64k-context free tier returns
            # for a full-size build log, down to the wording hadald parses.
            payload = json.dumps({"error": {"message":
                "This model's maximum context length is 65536 tokens. However, you "
                "requested 2048 output tokens and your prompt contains at least 87931 "
                "input tokens, for a total of at least 89979 tokens."}}).encode()
            self.send_response(400)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        if MODE in ('429', '400', '500'):
            payload = json.dumps({"error": f"mock {MODE}"}).encode()
            self.send_response(int(MODE))
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Transfer-Encoding', 'chunked')
        self.end_headers()
        def chunk(s):
            b = s.encode()
            self.wfile.write(f"{len(b):X}\r\n".encode() + b + b"\r\n"); self.wfile.flush()

        if MODE == 'dead-stream':
            # 200, then die partway through. hadald has already committed to
            # this link and must NOT restart elsewhere.
            chunk('data: {"choices":[{"delta":{"content":"partial"}}]}\n\n')
            time.sleep(0.1)
            self.close_connection = True
            return

        for tok in ['hello ', 'from ', f'port{PORT}']:
            chunk('data: ' + json.dumps({"choices":[{"delta":{"content":tok}}]}) + '\n\n')
        chunk('data: [DONE]\n\n')
        self.wfile.write(b"0\r\n\r\n"); self.wfile.flush()

ThreadingHTTPServer(('127.0.0.1', PORT), H).serve_forever()
PY

wait_port() {
    for _ in $(seq 1 100); do
        (exec 3<>/dev/tcp/127.0.0.1/"$1") 2>/dev/null && return 0
        sleep 0.05
    done
    return 1
}

# Bring up A and B in the requested modes, point hadald at both, send one
# broker-shaped request. Sets $CODE and leaves the reply in $T/out.ndjson.
run_case() {
    for p in "${PIDS[@]:-}"; do [[ -n $p ]] && kill "$p" 2>/dev/null; done; PIDS=()
    : > "$T/hits.txt"
    python3 "$T/upstream.py" $APORT "$1" "$T/hits.txt" & PIDS+=($!)
    python3 "$T/upstream.py" $BPORT "$2" "$T/hits.txt" & PIDS+=($!)
    wait_port $APORT && wait_port $BPORT || { echo "mock upstreams did not start" >&2; exit 2; }

    "$BIN" --serve --listen 127.0.0.1:$HPORT \
        --model model-a --upstream http://127.0.0.1:$APORT/v1 \
        --fallback http://127.0.0.1:$BPORT/v1,model-b \
        > "$T/hadald.log" 2>&1 & PIDS+=($!)
    wait_port $HPORT || { echo "hadald did not start" >&2; cat "$T/hadald.log" >&2; exit 2; }

    CODE=$(curl -s -o "$T/out.ndjson" -w '%{http_code}' -X POST \
        "http://127.0.0.1:$HPORT/api/generate" -H 'Content-Type: application/json' \
        -d '{"model":"model-a","prompt":"why did boot fail?","system":"SYSPROMPT"}')
}

text_of() {
    python3 -c 'import json,sys
print("".join(json.loads(l)["response"] for l in open(sys.argv[1]) if l.strip()))' "$T/out.ndjson"
}
hits() { wc -l < "$T/hits.txt" | tr -d ' '; }

# ── 1. a rate limit is the case the chain exists for ──────────────────────
run_case 429 ok
[[ $CODE == 200 ]] && ok "429 at the primary still returns 200" || bad "429 gave $CODE"
[[ "$(text_of)" == "hello from port$BPORT" ]] \
    && ok "the fallback's answer is the one forwarded" || bad "wrong answer: $(text_of)"
[[ "$(hits)" == 2 ]] && ok "both links were attempted" || bad "$(hits) attempts, expected 2"
# The bug this catches: one model id reused across providers, which 404s
# everywhere but the endpoint that issued it.
[[ "$(grep -c 'model=model-a' "$T/hits.txt")" == 1 && \
   "$(grep -c 'model=model-b' "$T/hits.txt")" == 1 ]] \
    && ok "each link was asked for its own model id" \
    || bad "model ids not per-link: $(cat "$T/hits.txt")"

# ── 2. a malformed request is wrong everywhere ────────────────────────────
run_case 400 ok
[[ $CODE == 502 ]] && ok "400 at the primary fails fast" || bad "400 gave $CODE"
[[ "$(hits)" == 1 ]] \
    && ok "a 400 does not ship the prompt to the rest of the chain" \
    || bad "the chain walked past a 400 — $(hits) upstreams saw the prompt"

# ── 2b. a context window is a property of the link, not of the request ────
# The one 400 that must NOT fail fast. Free tiers cap context as well as
# throughput, so the chain is deliberately heterogeneous; if the narrowest
# window in it could end the walk, it would decide the largest log the whole
# chain can explain.
run_case 400-context ok
[[ $CODE == 200 ]] \
    && ok "a context-length 400 falls through to a link with a wider window" \
    || bad "context 400 gave $CODE — the chain stopped at the narrow link"
[[ "$(text_of)" == "hello from port$BPORT" ]] \
    && ok "the wider link's answer is the one forwarded" || bad "wrong answer: $(text_of)"

# ...and when nothing behind it is wider, the arithmetic is what comes back,
# not the generic 502 that sent a real diagnosis chasing rate limits for a day.
run_case 400-context 400-context
[[ $CODE == 413 ]] \
    && ok "a chain that is too small everywhere reports 413, not 502" \
    || bad "expected 413 once every window was too small, got $CODE"
grep -q '65536\|87931' "$T/out.ndjson" \
    && ok "the 413 carries the numbers the user needs" \
    || bad "413 lost the arithmetic: $(cat "$T/out.ndjson")"
grep -qi 'input tokens, for a total' "$T/out.ndjson" \
    && bad "the upstream body was forwarded verbatim" \
    || ok "the upstream body itself is still not forwarded"

# ── 3. nothing left to try ────────────────────────────────────────────────
run_case 429 500
[[ $CODE == 502 ]] && ok "an exhausted chain returns 502" || bad "exhausted chain gave $CODE"
grep -q "$APORT" "$T/out.ndjson" && grep -q "$BPORT" "$T/out.ndjson" \
    && ok "the error names every link that failed" \
    || bad "error does not name both links: $(cat "$T/out.ndjson")"

# ── 4. the commit boundary ────────────────────────────────────────────────
# Once a token has reached the broker, restarting on another link would splice
# two models' output into one stream — and the broker's ProposalScanner is a
# single pass, so the concatenation can form a proposal neither model made.
run_case dead-stream ok
[[ "$(hits)" == 1 ]] \
    && ok "a stream that dies after 200 does not restart on the next link" \
    || bad "hadald retried mid-stream — output can be spliced"
[[ "$(grep -c partial "$T/out.ndjson")" == 1 ]] \
    && ok "no duplicated text" || bad "text was duplicated"

# ── 5. configuration errors are refusals to start, not runtime surprises ──
for p in "${PIDS[@]:-}"; do [[ -n $p ]] && kill "$p" 2>/dev/null; done; PIDS=()

"$BIN" --serve --listen 127.0.0.1:$HPORT --model m \
    --upstream http://127.0.0.1:$APORT/v1 \
    --fallback https://b.example/v1,b-model,"$T/nonexistent.key" \
    > "$T/badkey.log" 2>&1
# A chain whose third link has an unreadable key must break at startup, not on
# the day the first two are rate-limited.
[[ $? -ne 0 ]] && ok "an unreadable fallback key refuses to start" \
    || bad "started with an unreadable fallback key"

"$BIN" --serve --listen 127.0.0.1:$HPORT --model m \
    --upstream http://127.0.0.1:$APORT/v1 \
    --fallback http://127.0.0.1:$APORT/v1,m \
    > "$T/dup.log" 2>&1
[[ $? -ne 0 ]] && ok "a chain that falls back to itself is refused" \
    || bad "accepted a duplicate link"

echo
echo "$pass/$((pass+fail)) passed"
[[ $fail -eq 0 ]]
