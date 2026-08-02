#!/usr/bin/env python3
"""Cross-check the capability list against every place it is repeated.

The set of capabilities is stated four times, in four languages:

  1. capability.rs   Capability::id()          -- what the broker knows
  2. action.rs       Action::id()              -- what can be proposed
  3. policy/*.policy polkit action ids         -- what can be authorized
  4. model.rs        ACTION_PROTOCOL           -- what the model is told exists

Rust cannot check across those boundaries. When they drift the failure is
quiet and bad: a capability with no polkit action is denied by default with no
explanation, and one the model is told about but the parser rejects turns into
proposals that vanish. This runs in a second and catches both.

Exit 0 if consistent, 1 otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BROKER = ROOT / "src" / "hadal-brokerd" / "src"

SOURCES = {
    "capability.rs Capability::id()": (
        BROKER / "capability.rs",
        re.compile(r'Capability::\w+\s*=>\s*"([a-z-]+)"'),
    ),
    "action.rs Action::id()": (
        BROKER / "action.rs",
        re.compile(r'Action::\w+\s*\{\s*\.\.\s*\}\s*=>\s*"([a-z-]+)"'),
    ),
}

POLICY_FILE = ROOT / "policy" / "org.hadal.broker.policy"


def polkit_actions(path: Path) -> set[str]:
    """Parse the policy as XML rather than scraping it with a regex.

    Not pedantry. A regex happily matches action ids in a file polkit cannot
    read: a `--` inside an XML comment (say, writing `emerge --pretend`) makes
    the document ill-formed, and polkit's parser stops there and silently
    drops every action below it. That failure looked exactly like a working
    system until half the capabilities were quietly unauthorizable.

    Parsing here fails loudly at authoring time instead.
    """
    import xml.etree.ElementTree as ET

    try:
        tree = ET.parse(path)
    except ET.ParseError as e:
        print(f"ERROR  {path.name} is not well-formed XML: {e}")
        print("       polkit will silently ignore every action after this point.")
        raise SystemExit(1)

    prefix = "org.hadal.broker."
    return {
        el.attrib["id"][len(prefix):]
        for el in tree.getroot().iter("action")
        if el.attrib.get("id", "").startswith(prefix)
    }

# Capabilities deliberately NOT described to the model, and why.
#
# The broker still knows them and polkit still governs them — a client may
# invoke them directly. But describing a capability the model cannot
# successfully use only teaches it to propose things that always fail, which
# trains the user to dismiss proposals. If one of these becomes usable, delete
# it from here and document it in ACTION_PROTOCOL in the same commit.
WITHHELD_FROM_MODEL = {
    # Unimplemented: brokerd shares hadald's network namespace and has no
    # route out. Needs the egress proxy unit first. See ARCHITECTURE.md.
    "network-lookup",
}


def action_protocol(text: str) -> str:
    """Just the raw string the model is given.

    Scanning all of model.rs instead would pick up the deliberately-invalid
    actions in the test module and the field names of the Ollama request body,
    and report all of them as drift.
    """
    m = re.search(r'ACTION_PROTOCOL:\s*&str\s*=\s*r#"(.*?)"#;', text, re.DOTALL)
    if not m:
        raise SystemExit("ERROR  could not locate ACTION_PROTOCOL in model.rs")
    return m.group(1)


def main() -> int:
    found: dict[str, set[str]] = {}
    missing_files: list[str] = []

    for label, (path, pattern) in SOURCES.items():
        if not path.exists():
            missing_files.append(f"{label}: {path} not found")
            continue
        text = path.read_text(encoding="utf-8")
        found[label] = set(pattern.findall(text))

    if missing_files:
        for m in missing_files:
            print(f"ERROR  {m}")
        return 1

    if not POLICY_FILE.exists():
        print(f"ERROR  polkit policy: {POLICY_FILE} not found")
        return 1
    found["polkit policy"] = polkit_actions(POLICY_FILE)

    protocol = action_protocol((BROKER / "model.rs").read_text(encoding="utf-8"))
    found["model.rs ACTION_PROTOCOL"] = set(
        re.findall(r'\{"action"\s*:\s*"([a-z-]+)"', protocol)
    ) | WITHHELD_FROM_MODEL

    reference_label = "capability.rs Capability::id()"
    reference = found[reference_label]

    print(f"{len(reference)} capabilities declared in {reference_label}:")
    for c in sorted(reference):
        print(f"  {c}")
    print()

    failed = False
    for label, ids in found.items():
        if label == reference_label:
            continue
        missing = reference - ids
        extra = ids - reference
        if missing or extra:
            failed = True
            print(f"MISMATCH  {label}")
            for c in sorted(missing):
                print(f"   missing: {c}")
            for c in sorted(extra):
                print(f"   unknown: {c}")
        else:
            print(f"OK        {label}")

    # An example that names a capability correctly but misspells a parameter
    # still produces proposals the parser silently drops, so check the
    # parameter vocabulary the model is taught against the struct fields that
    # actually exist.
    action_rs = (BROKER / "action.rs").read_text(encoding="utf-8")
    documented_params = set(re.findall(r'"(\w+)"\s*:', protocol)) - {"action", "kind"}
    # Matches field declarations in both multi-line and inline struct
    # variants (`PortageUse { atom: Atom, flags: Vec<UseFlag> }`), keyed off
    # the type position so local bindings in function bodies are not swept up.
    known_fields = set(
        re.findall(
            r"(\w+):\s*(?:Option<|Vec<|[A-Z]\w*|u8|u16|u32|u64|i32|i64|bool|f32|f64)",
            action_rs,
        )
    )
    unknown = documented_params - known_fields
    if unknown:
        failed = True
        print(f"\nMISMATCH  model.rs documents parameters unknown to action.rs: "
              f"{', '.join(sorted(unknown))}")
    else:
        print("OK        model.rs parameter vocabulary")

    print()
    print("INCONSISTENT" if failed else "CONSISTENT")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
