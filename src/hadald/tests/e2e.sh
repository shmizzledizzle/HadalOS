#!/usr/bin/env bash
# End-to-end: broker-shaped request in, broker-shaped stream out.
#
# Stands up a fake OpenAI-compatible upstream, points hadald at it, and speaks
# to hadald exactly as hadal-brokerd's ModelClient does. Needs no API key and
# no network — the thing being tested is the translation, not the model.
set -uo pipefail

BIN="${1:-target/debug/hadald}"
[[ -x $BIN ]] || { echo "not built: $BIN" >&2; exit 2; }

# Track PIDs explicitly. Job specs (%1, %2) do not work under `set -u` in a
# non-interactive shell, so a failed run used to leave the fake upstream alive
# holding its port — and the next run's requests were then served by a stale
# process writing into a deleted temp directory. The symptom was a confusing
# "malformed NDJSON" that had nothing to do with the code under test.
T="$(mktemp -d)"; PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [[ -n $p ]] && kill "$p" 2>/dev/null; done; rm -rf "$T"; }
trap cleanup EXIT
pass=0; fail=0
ok()  { printf 'ok    %s\n' "$*"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s\n' "$*"; fail=$((fail+1)); }

# ── a fake upstream that streams an action fence one character at a time ──
cat > "$T/upstream.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

REPLY = 'Checking the log.\n```hadal-action\n{"action":"read-journal","lines":50}\n```\n'

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        # Echo what we were sent so the test can assert on it.
        with open(sys.argv[2], 'w') as f:
            json.dump({"auth": self.headers.get("Authorization",""), "body": body}, f)
        self.send_response(200)
        self.send_header('Content-Type','text/event-stream')
        self.end_headers()
        for ch in REPLY:                      # worst-case tokenisation
            frame = {"choices":[{"delta":{"content":ch}}]}
            self.wfile.write(f"data: {json.dumps(frame)}\n\n".encode())
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

HTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PY

UPORT=18435; HPORT=18434
# Fail loudly rather than silently reusing someone else's listener.
for port in $UPORT $HPORT; do
    if (exec 3<>/dev/tcp/127.0.0.1/$port) 2>/dev/null; then
        echo "port $port already in use — a previous run may still be alive" >&2; exit 2
    fi
done
python3 "$T/upstream.py" "$UPORT" "$T/seen.json" & PIDS+=($!)
printf 'nvapi-fake-key-for-tests' > "$T/key"; chmod 600 "$T/key"

"$BIN" --serve --model test-model \
       --listen "127.0.0.1:$HPORT" \
       --upstream "http://127.0.0.1:$UPORT/v1" \
       --key-file "$T/key" --egress-log "$T/egress.log" > "$T/hadald.log" 2>&1 & PIDS+=($!)

# Wait for BOTH listeners. Waiting only on hadald is a race: it binds in
# milliseconds while python's HTTPServer still has imports to do, so the first
# request would arrive before the upstream existed and fail as "unreachable".
wait_port() {
    for _ in $(seq 1 100); do
        (exec 3<>/dev/tcp/127.0.0.1/"$1") 2>/dev/null && return 0
        sleep 0.1
    done
    echo "timed out waiting for port $1" >&2; return 1
}
wait_port "$UPORT" || exit 2
wait_port "$HPORT" || exit 2

# ── readiness, as ModelClient::ready() probes it ──────────────────────────
code=$(curl -s -o "$T/tags.json" -w '%{http_code}' "http://127.0.0.1:$HPORT/api/tags")
[[ $code == 200 ]] && ok "GET /api/tags returns 200" || bad "/api/tags returned $code"
grep -q 'test-model' "$T/tags.json" && ok "tags names the configured model" || bad "model missing from tags"

# ── generation, as ModelClient::generate() calls it ───────────────────────
curl -s -N -X POST "http://127.0.0.1:$HPORT/api/generate" \
     -H 'content-type: application/json' \
     -d '{"model":"test-model","prompt":"why did boot fail?","system":"SYSPROMPT","stream":true}' \
     > "$T/out.ndjson"

# Every line must be a standalone JSON object with the two fields the broker reads.
lines=$(wc -l < "$T/out.ndjson")
if python3 - "$T/out.ndjson" <<'PY'
import json,sys
bad=0
for i,l in enumerate(open(sys.argv[1])):
    l=l.strip()
    if not l: continue
    try:
        o=json.loads(l)
        assert "response" in o and "done" in o
    except Exception as e:
        print(f"  line {i}: {e}"); bad=1
sys.exit(bad)
PY
then ok "every output line is valid Ollama NDJSON ($lines lines)"
else bad "malformed NDJSON"; fi

# The reassembled text must be byte-identical to what the upstream emitted.
python3 - "$T/out.ndjson" > "$T/joined.txt" <<'PY'
import json,sys
print("".join(json.loads(l)["response"] for l in open(sys.argv[1]) if l.strip()), end="")
PY
grep -q '```hadal-action' "$T/joined.txt" \
    && ok "the action fence survives reassembly" \
    || bad "ACTION FENCE LOST — the broker would see prose only"
grep -q '"action":"read-journal"' "$T/joined.txt" \
    && ok "proposal JSON is intact" || bad "proposal corrupted"

tail -1 "$T/out.ndjson" | grep -q '"done":true' \
    && ok "stream terminates with done:true" \
    || bad "no terminating done — the broker's scanner never flushes"

# ── what we sent upstream ─────────────────────────────────────────────────
python3 - "$T/seen.json" <<'PY' && ok "system prompt forwarded verbatim; auth header set; stream requested" || bad "upstream request wrong"
import json,sys
d=json.load(open(sys.argv[1])); b=d["body"]
assert d["auth"] == "Bearer nvapi-fake-key-for-tests", d["auth"]
assert b["messages"][0]["role"]=="system" and b["messages"][0]["content"]=="SYSPROMPT"
assert b["messages"][1]["content"]=="why did boot fail?"
assert b["stream"] is True
PY

# ── the egress record ─────────────────────────────────────────────────────
[[ -s $T/egress.log ]] && ok "egress log records the outbound request" || bad "egress log empty"
grep -q 'prompt_bytes=' "$T/egress.log" && ok "egress log notes payload size" || bad "no size recorded"
grep -q 'why did boot fail' "$T/egress.log" \
    && bad "prompt body logged without --log-bodies" \
    || ok "prompt body NOT logged unless asked"

if (( fail )); then
    echo; echo "--- hadald log ---"; sed 's/^/    /' "$T/hadald.log"
    echo "--- first bytes of /api/generate ---"; head -c 300 "$T/out.ndjson" | sed 's/^/    /'; echo
fi

printf '\n%d/%d passed\n' "$pass" "$((pass+fail))"
[[ $fail -eq 0 ]]
