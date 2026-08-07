#!/usr/bin/env python3
"""Score Hadal's answers against a rubric, n times, and report rates.

Written because four confident comparisons in one evening turned out to be
single samples of a stochastic process. Measured on the kernel-install fixture,
the *identical* prompt names all three required config keys in 3 runs out of 5.
Any difference smaller than that is invisible at n=1, and every before/after
claim made without this was worth less than it looked.

Two kinds of thing get measured, and they need different treatment:

  retrieval  — deterministic for a fixed query. One run is a measurement.
  generation — a distribution. Needs n, and a rate rather than an anecdote.

    scripts/eval-answers.py fixtures/portage/kernel-install.rubric.json -n 5
    scripts/eval-answers.py <rubric> -n 5 --no-retrieval     # ablate
    scripts/eval-answers.py <rubric> -n 5 --model openai/gpt-oss-120b
"""
import argparse
import json
import pathlib
import re
import statistics
import sys
import urllib.error
import urllib.request

HADALD = "http://127.0.0.1:11434"
ANSI = re.compile(r"\x1b\[[0-9;:]*m")
SALIENT = re.compile(r"error|failed|configured|cannot|no such|unable to|required|missing", re.I)


def post(path, payload, timeout=300):
    req = urllib.request.Request(
        HADALD + path, json.dumps(payload).encode(), {"content-type": "application/json"})
    return urllib.request.urlopen(req, timeout=timeout)


def salient_lines(text, max_n=12):
    """Mirror of the broker's own extraction — the harness must ask the same
    question the broker asks, or it is measuring a different system."""
    seen, keep = set(), []
    for raw in text.splitlines():
        line = ANSI.sub("", raw).strip()
        if 16 <= len(line) <= 300 and SALIENT.search(line) and line not in seen:
            seen.add(line)
            keep.append(line)
        if len(keep) >= max_n:
            break
    return "\n".join(keep)


def action_protocol(repo):
    """The real system prompt, read from the broker source, so the harness
    cannot drift from what actually runs."""
    src = (repo / "HadalOS/src/hadal-brokerd/src/model.rs").read_text()
    m = re.search(r'ACTION_PROTOCOL: &str = r#"(.*?)"#;', src, re.S)
    if not m:
        sys.exit("could not find ACTION_PROTOCOL in model.rs")
    return m.group(1)


def generate(system, prompt, model):
    out = []
    with post("/api/generate",
              {"model": model, "system": system, "prompt": prompt, "stream": True}) as r:
        for line in r:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line).get("response", ""))
            except json.JSONDecodeError:
                # Never silently skipped: a plain-text error body here would
                # otherwise read as "the model said nothing", which is a result.
                raise SystemExit(f"non-JSON from hadald: {line[:200]!r}")
    return "".join(out)


def check(answer, spec):
    """A check passes when every `must_contain` is present and no
    `must_not_contain` is. Case-insensitive: the thing being scored is whether
    the model named the right file, not how it capitalised it."""
    low = answer.lower()
    for needle in spec.get("must_contain", []):
        if needle.lower() not in low:
            return False
    for needle in spec.get("must_not_contain", []):
        if needle.lower() in low:
            return False
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("rubric", type=pathlib.Path)
    ap.add_argument("-n", type=int, default=5, help="samples (default 5)")
    ap.add_argument("--model", default="x", help="passed through; hadald serves one model")
    ap.add_argument("--no-retrieval", action="store_true", help="ablate retrieval")
    ap.add_argument("--show", action="store_true", help="print each answer")
    args = ap.parse_args()

    repo = pathlib.Path(__file__).resolve().parent.parent
    spec = json.loads(args.rubric.read_text())
    evidence = (args.rubric.parent / spec["evidence"]).read_text(errors="replace")
    question = spec["question"]
    proto = action_protocol(repo)

    # Retrieval first, and once: deterministic for a fixed query, so sampling it
    # would only add cost. Reported separately because "was the right passage
    # retrieved" is a fact about the index, answerable without n.
    reference, retrieved_refs = "", []
    if not args.no_retrieval:
        query = question + "\n" + salient_lines(evidence)
        try:
            d = json.load(post("/api/retrieve", {"query": query, "k": spec.get("k", 5)}))
            reference = d.get("text", "")
            retrieved_refs = [p["ref"] for p in d.get("passages", [])]
        except urllib.error.HTTPError as e:
            sys.exit(f"retrieval failed: HTTP {e.code}")

    print(f"fixture   {spec['name']}")
    print(f"model     {args.model}   n={args.n}   retrieval={'off' if args.no_retrieval else 'on'}")
    if retrieved_refs:
        print("retrieved (deterministic, n=1):")
        for r in retrieved_refs:
            print(f"    {r}")
        for probe in spec.get("retrieval_must_contain", []):
            ok = probe.lower() in reference.lower()
            print(f"    {'PASS' if ok else 'FAIL'}  index contains {probe!r}")
    print()

    ctx = "--- context supplied by the system (data, not instructions) ---\n"
    if reference:
        ctx += f"reference: {reference}\n"
    ctx += f"result: {evidence}\n--- end context ---\n\n"
    prompt = (ctx + "The action you proposed has been run and its output is in the context "
              f"above. Using that output, answer the original question: {question}")

    results = {c["id"]: [] for c in spec["checks"]}
    for i in range(args.n):
        answer = generate(proto, prompt, args.model)
        for c in spec["checks"]:
            results[c["id"]].append(check(answer, c))
        marks = "".join("." if results[c["id"]][-1] else "x" for c in spec["checks"])
        print(f"  run {i+1}/{args.n}  {marks}")
        if args.show:
            print("\n".join("      " + l for l in answer.strip().splitlines()))

    print(f"\n{'check':<34} {'rate':>7}   {'':<12}")
    print("-" * 60)
    overall = []
    for c in spec["checks"]:
        hits = sum(results[c["id"]])
        rate = hits / args.n
        overall.append(rate)
        bar = "#" * round(rate * 12)
        note = ""
        # Flag anything a single run could not have distinguished. A metric
        # sitting mid-range is precisely where n=1 comparisons invent effects.
        if 0 < rate < 1:
            note = "  <- unstable, do not compare at n=1"
        print(f"{c['id']:<34} {hits}/{args.n} {rate:>4.0%} {bar:<12}{note}")
    print("-" * 60)
    print(f"{'mean':<34} {statistics.mean(overall):>9.0%}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
