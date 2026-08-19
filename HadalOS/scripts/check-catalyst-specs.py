#!/usr/bin/env python3
"""Validate the HadalOS catalyst specs, and the chain between them.

catalyst does not check that stage3's source_subpath actually names stage1's
output. If it does not, and something with a matching name is already on disk
from a previous run, the build succeeds against the wrong input -- producing an
image built from months-old bits with no error anywhere. That failure mode is
the reason this exists.

Checks:
  * spec syntax (key: value, tab-indented continuations)
  * required keys per target
  * rel_type / subarch / profile agree across the chain
  * each stage consumes the previous stage's actual output name
  * referenced paths exist
  * only known placeholders remain

Exit 0 if valid, 1 otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC_DIR = ROOT / "catalyst"

# In build order. Each consumes the previous one's output.
CHAIN = ["stage1", "stage3", "livecd-stage1", "livecd-stage2"]

REQUIRED = {
    "stage1": ["subarch", "target", "version_stamp", "rel_type", "profile",
               "snapshot_treeish", "source_subpath"],
    "stage3": ["subarch", "target", "version_stamp", "rel_type", "profile",
               "snapshot_treeish", "source_subpath"],
    "livecd-stage1": ["subarch", "target", "version_stamp", "rel_type", "profile",
                      "snapshot_treeish", "source_subpath", "livecd/packages"],
    "livecd-stage2": ["subarch", "target", "version_stamp", "rel_type", "profile",
                      "snapshot_treeish", "source_subpath", "livecd/fstype"],
}

KNOWN_PLACEHOLDERS = {"@TIMESTAMP@", "@TREEISH@", "@REPO_DIR@"}
PLACEHOLDER = re.compile(r"@[A-Z_]+@")
KEY_LINE = re.compile(r"^([A-Za-z][A-Za-z0-9_/]*):\s*(.*)$")

problems: list[str] = []


def parse(path: Path) -> dict[str, list[str]]:
    """catalyst spec format: `key: value`, continuations indented with a tab."""
    spec: dict[str, list[str]] = {}
    key = None
    for n, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        if raw.startswith(("\t", "    ")):
            if key is None:
                problems.append(f"{path.name}:{n}: continuation before any key")
                continue
            spec[key].append(raw.strip())
            continue
        m = KEY_LINE.match(raw)
        if not m:
            problems.append(f"{path.name}:{n}: not `key: value`: {raw!r}")
            continue
        key, value = m.group(1), m.group(2).strip()
        spec[key] = [value] if value else []
    return spec


def one(spec: dict[str, list[str]], key: str) -> str | None:
    v = spec.get(key)
    return v[0] if v else None


def output_name(spec: dict[str, list[str]]) -> str:
    """What catalyst will call this stage's result."""
    return "{}/{}-{}-{}".format(
        one(spec, "rel_type"), one(spec, "target"),
        one(spec, "subarch"), one(spec, "version_stamp"),
    )


def main() -> int:
    specs: dict[str, dict[str, list[str]]] = {}

    for name in CHAIN:
        path = SPEC_DIR / f"{name}.spec"
        if not path.exists():
            problems.append(f"missing spec: {path}")
            continue
        specs[name] = parse(path)

    if problems:
        for p in problems:
            print(f"ERROR  {p}")
        return 1

    print(f"{len(specs)} specs\n")

    for name, spec in specs.items():
        for key in REQUIRED[name]:
            if key not in spec or not spec[key]:
                problems.append(f"{name}: missing required key {key!r}")

        target = one(spec, "target")
        if target != name:
            problems.append(f"{name}: target is {target!r}, expected {name!r}")

        for key, values in spec.items():
            for v in values:
                for ph in PLACEHOLDER.findall(v):
                    if ph not in KNOWN_PLACEHOLDERS:
                        problems.append(f"{name}: unknown placeholder {ph} in {key}")

        for key in ("portage_confdir", "repos"):
            v = one(spec, key)
            if v and v.startswith("@REPO_DIR@"):
                rel = v.replace("@REPO_DIR@/", "")
                if not (ROOT / rel).exists():
                    problems.append(f"{name}: {key} points at missing path {rel}")

    # ── the check that matters ──
    print("chain:")
    for i, name in enumerate(CHAIN):
        spec = specs[name]
        produces = output_name(spec)
        consumes = one(spec, "source_subpath")
        print(f"  {name:<14} consumes {consumes}")
        print(f"  {'':<14} produces {produces}")

        if i == 0:
            continue
        expected = output_name(specs[CHAIN[i - 1]])
        if consumes != expected:
            problems.append(
                f"{name}: source_subpath is {consumes!r} but {CHAIN[i-1]} produces "
                f"{expected!r} -- this stage would build from the wrong input"
            )

    # ── consistency across the chain ──
    for key in ("rel_type", "subarch", "profile", "snapshot_treeish"):
        values = {name: one(s, key) for name, s in specs.items()}
        distinct = set(values.values())
        if len(distinct) > 1:
            problems.append(f"{key} differs across the chain: {values}")

    print()
    if problems:
        for p in problems:
            print(f"ERROR  {p}")
        print(f"\n{len(problems)} PROBLEM(S)")
        return 1

    print("SPECS VALID, CHAIN CONSISTENT")
    return 0


if __name__ == "__main__":
    sys.exit(main())
