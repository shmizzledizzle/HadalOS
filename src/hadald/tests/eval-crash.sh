#!/usr/bin/env bash
# Answer the open question: can a model turn a crash report into the sentence a
# person actually needs — and, harder, stay quiet about one they cannot act on?
#
# Sends fixtures/crash/*.txt through hadald to a real upstream. This egresses
# the fixtures; they are platform stack traces with no personal data, which is
# why those two were chosen as the corpus.
#
#   bash tests/eval-crash.sh <model-id> [key-file]
set -uo pipefail

MODEL="${1:?usage: eval-crash.sh <model-id> [key-file]}"
KEY="${2:-$HOME/.config/hadal/upstream.key}"
FIXTURES="$(dirname "$0")/../../../fixtures/crash"
BIN="$(dirname "$0")/../target/debug/hadald"
PORT=18436

[[ -x $BIN ]] || { echo "not built: $BIN" >&2; exit 2; }
[[ -d $FIXTURES ]] || { echo "no fixtures at $FIXTURES" >&2; exit 2; }

T="$(mktemp -d)"; PID=""
cleanup() { [[ -n $PID ]] && kill "$PID" 2>/dev/null; rm -rf "$T"; }
trap cleanup EXIT

# The question being asked of the model. Deliberately includes permission to
# say "nothing to do" — a model that cannot decline is the failure mode that
# matters, and one that never gets tested if every case is actionable.
read -r -d '' SYSTEM <<'EOF'
You are Hadal, the assistant built into this operating system. You are being
shown a crash report the user never asked to see and cannot interpret. What
they saw was an app closing unexpectedly.

Answer in at most four sentences, in this order:
1. What actually happened, in plain language.
2. What the user should do about it — or say plainly that there is nothing for
   them to do, if the fault is in the platform rather than anything they
   control.

Do not explain internal APIs. Do not speculate beyond the report. If this is a
known kind of platform defect the user cannot fix, saying so and stopping is
the correct and complete answer.
EOF

"$BIN" --serve --model "$MODEL" --listen "127.0.0.1:$PORT" \
       --key-file "$KEY" --egress-log "$T/egress.log" > "$T/hadald.log" 2>&1 &
PID=$!
for _ in $(seq 1 100); do
    (exec 3<>/dev/tcp/127.0.0.1/$PORT) 2>/dev/null && break
    sleep 0.1
done

for f in "$FIXTURES"/*.txt; do
    name="$(basename "$f" .txt)"
    printf '\n════ %s ════\n' "$name"
    python3 - "$f" "$SYSTEM" "$MODEL" "$PORT" <<'PY'
import json, sys, urllib.request
path, system, model, port = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
report = open(path).read()
body = json.dumps({
    "model": model,
    "system": system,
    "prompt": f"Crash report:\n\n{report}",
    "stream": True,
}).encode()
req = urllib.request.Request(f"http://127.0.0.1:{port}/api/generate", body,
                             {"content-type": "application/json"})
# Every non-JSON line is surfaced, never skipped. An eval that silently
# reports "empty" when the request actually failed is worse than one that
# crashes — a blank answer reads as "the model had nothing to say", which is a
# substantive finding, and must never be produced by a swallowed rate limit.
out, junk = [], []
try:
    with urllib.request.urlopen(req, timeout=180) as r:
        if r.status != 200:
            print(f"  HTTP {r.status} from hadald")
            sys.exit(1)
        for line in r:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line).get("response", ""))
            except json.JSONDecodeError:
                junk.append(line.decode(errors="replace")[:200])
except urllib.error.HTTPError as e:
    print(f"  HTTP {e.code}: {e.read().decode(errors='replace')[:300]}")
    sys.exit(1)
except Exception as e:
    print(f"  REQUEST FAILED: {e}")
    sys.exit(1)

if junk:
    print(f"  !! {len(junk)} non-JSON line(s) — NOT an empty answer:")
    for j in junk[:3]:
        print(f"     {j}")
    sys.exit(1)

text = "".join(out).strip()
if not text:
    print("  (model genuinely returned no content — stream was well-formed)")
else:
    print("\n".join("  " + l for l in text.splitlines()))
PY
done

printf '\n════ egress ════\n'
sed 's/^/  /' "$T/egress.log" 2>/dev/null || echo "  (none recorded)"
