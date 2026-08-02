#!/usr/bin/env python3
"""Check that the kernel config backs the systemd directives HadalOS relies on.

HadalOS states its isolation guarantees as systemd unit directives. Most of
those are enforced by the kernel, and a kernel built without the backing
symbol does not refuse the directive — it ignores it.

`IPAddressDeny=any` on a kernel without CONFIG_CGROUP_BPF is the case that
matters. The unit starts, the log says nothing, and the claim in
ARCHITECTURE.md that hadald has no route out is simply false. That is a
security property disappearing without a single error message.

This walks every shipped unit, collects the directives, and verifies the
config fragment provides what each one needs.

Exit 0 if the units and the kernel agree, 1 otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FRAGMENT = ROOT / "overlay" / "sys-kernel" / "hadalos-kernel" / "files" / "hadalos.config"

# Directive -> (required symbols, fails_silently)
#
# fails_silently marks the directives that produce NO error when unsupported.
# Those are the dangerous ones: a loud failure gets fixed, a quiet one ships.
DIRECTIVE_REQUIRES: dict[str, tuple[list[str], bool]] = {
    # Network isolation
    "PrivateNetwork":           (["CONFIG_NET_NS"], False),
    "IPAddressDeny":            (["CONFIG_CGROUP_BPF", "CONFIG_BPF_SYSCALL"], True),
    "IPAddressAllow":           (["CONFIG_CGROUP_BPF", "CONFIG_BPF_SYSCALL"], True),
    "RestrictAddressFamilies":  (["CONFIG_SECCOMP_FILTER"], True),

    # Namespaces
    "PrivateUsers":             (["CONFIG_USER_NS"], False),
    "PrivateTmp":               (["CONFIG_NAMESPACES"], False),
    "PrivateDevices":           (["CONFIG_NAMESPACES", "CONFIG_DEVTMPFS"], False),
    "ProtectSystem":            (["CONFIG_NAMESPACES"], False),
    "ProtectHome":              (["CONFIG_NAMESPACES"], False),
    "ProtectKernelTunables":    (["CONFIG_NAMESPACES"], False),
    "ProtectKernelModules":     (["CONFIG_NAMESPACES"], False),
    "ProtectKernelLogs":        (["CONFIG_NAMESPACES"], False),
    "ProtectHostname":          (["CONFIG_UTS_NS"], False),
    "ProtectProc":              (["CONFIG_PROC_FS"], False),
    "ProcSubset":               (["CONFIG_PROC_FS"], False),
    "RestrictNamespaces":       (["CONFIG_SECCOMP_FILTER", "CONFIG_NAMESPACES"], True),
    "JoinsNamespaceOf":         (["CONFIG_NAMESPACES", "CONFIG_NET_NS"], False),

    # seccomp
    "SystemCallFilter":         (["CONFIG_SECCOMP", "CONFIG_SECCOMP_FILTER"], True),
    "SystemCallArchitectures":  (["CONFIG_SECCOMP_FILTER"], True),
    "SystemCallErrorNumber":    (["CONFIG_SECCOMP_FILTER"], True),
    "RestrictRealtime":         (["CONFIG_SECCOMP_FILTER"], True),
    "RestrictSUIDSGID":         (["CONFIG_SECCOMP_FILTER"], True),
    "LockPersonality":          (["CONFIG_SECCOMP_FILTER"], True),
    "MemoryDenyWriteExecute":   (["CONFIG_SECCOMP_FILTER"], True),
    "ProtectClock":             (["CONFIG_SECCOMP_FILTER"], True),

    # Resource control — the ceilings that stop inference starving the desktop
    "MemoryMax":                (["CONFIG_MEMCG"], True),
    "MemoryHigh":               (["CONFIG_MEMCG"], True),
    "CPUQuota":                 (["CONFIG_CGROUP_SCHED", "CONFIG_FAIR_GROUP_SCHED"], True),
    "CPUWeight":                (["CONFIG_CGROUP_SCHED", "CONFIG_FAIR_GROUP_SCHED"], True),
    "IOWeight":                 (["CONFIG_BLK_CGROUP"], True),
    "TasksMax":                 (["CONFIG_CGROUP_PIDS"], True),
    "ProtectControlGroups":     (["CONFIG_CGROUPS"], False),
    "DeviceAllow":              (["CONFIG_CGROUP_DEVICE"], True),
    "OOMPolicy":                (["CONFIG_MEMCG"], False),
}

# systemd will not boot without these at all.
SYSTEMD_BASELINE = [
    "CONFIG_DEVTMPFS", "CONFIG_CGROUPS", "CONFIG_INOTIFY_USER", "CONFIG_SIGNALFD",
    "CONFIG_TIMERFD", "CONFIG_EPOLL", "CONFIG_UNIX", "CONFIG_SYSFS", "CONFIG_PROC_FS",
    "CONFIG_FHANDLE", "CONFIG_NET", "CONFIG_TMPFS_XATTR", "CONFIG_TMPFS_POSIX_ACL",
    "CONFIG_CRYPTO_USER_API_HASH", "CONFIG_CRYPTO_HMAC", "CONFIG_CRYPTO_SHA256",
]

# Needed to reach a usable system at all, given the rest of the design.
STRUCTURAL = {
    "CONFIG_VFAT_FS": "the ESP is VFAT; without it /boot cannot be mounted",
    "CONFIG_NLS_CODEPAGE_437": "VFAT mount fails without it, with a misleading error",
    "CONFIG_NLS_ISO8859_1": "VFAT mount fails without it, with a misleading error",
    "CONFIG_EFI_STUB": "required for the direct-EFI fallback boot path",
    "CONFIG_BLK_DEV_INITRD": "dracut initramfs will not be loaded",
    "CONFIG_INPUT_EVDEV": "libinput, XLibre and hadalwm all read from evdev",
    "CONFIG_SQUASHFS": "the live ISO root is a squashfs",
    "CONFIG_OVERLAY_FS": "the live ISO needs a writable overlay",
}

CONFIG_LINE = re.compile(r"^(CONFIG_[A-Z0-9_]+)=(.+)$")
UNSET_LINE = re.compile(r"^# (CONFIG_[A-Z0-9_]+) is not set$")
DIRECTIVE_LINE = re.compile(r"^\s*([A-Za-z][A-Za-z0-9]*)\s*=")


def parse_fragment(path: Path) -> tuple[dict[str, str], list[str]]:
    """Return {symbol: value} plus any syntactically bad lines."""
    enabled: dict[str, str] = {}
    bad: list[str] = []
    for n, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        if m := UNSET_LINE.match(line):
            enabled[m.group(1)] = "n"
            continue
        if line.startswith("#"):
            continue
        if m := CONFIG_LINE.match(line):
            enabled[m.group(1)] = m.group(2).strip()
            continue
        bad.append(f"{path.name}:{n}: not valid kconfig syntax: {raw!r}")
    return enabled, bad


def unit_files() -> list[Path]:
    return sorted(
        list((ROOT / "systemd").glob("*.service"))
        + list(ROOT.glob("overlay/**/files/*.service"))
    )


def main() -> int:
    if not FRAGMENT.exists():
        print(f"ERROR  no config fragment at {FRAGMENT}")
        return 1

    enabled, bad_lines = parse_fragment(FRAGMENT)
    if bad_lines:
        for b in bad_lines:
            print(f"ERROR  {b}")
        return 1

    def provided(sym: str) -> bool:
        return enabled.get(sym, "n") in ("y", "m")

    failures: list[str] = []
    silent_failures: list[str] = []

    print(f"config fragment: {len(enabled)} symbols\n")

    # ── systemd baseline ──
    missing = [s for s in SYSTEMD_BASELINE if not provided(s)]
    if missing:
        failures += [f"systemd baseline missing: {s}" for s in missing]
        print(f"FAIL      systemd baseline ({len(missing)} missing)")
    else:
        print("OK        systemd baseline")

    # ── structural requirements ──
    for sym, why in STRUCTURAL.items():
        if not provided(sym):
            failures.append(f"{sym} not set -- {why}")
    if any(not provided(s) for s in STRUCTURAL):
        print("FAIL      structural requirements")
    else:
        print("OK        structural requirements")

    # ── unit directives ──
    units = unit_files()
    if not units:
        print("ERROR  no unit files found")
        return 1

    seen: dict[str, set[str]] = {}
    for unit in units:
        for raw in unit.read_text(encoding="utf-8").splitlines():
            if raw.lstrip().startswith("#"):
                continue
            if m := DIRECTIVE_LINE.match(raw):
                seen.setdefault(m.group(1), set()).add(unit.name)

    checked = 0
    for directive, sources in sorted(seen.items()):
        spec = DIRECTIVE_REQUIRES.get(directive)
        if spec is None:
            continue
        required, silent = spec
        checked += 1
        gaps = [s for s in required if not provided(s)]
        if gaps:
            where = ", ".join(sorted(sources))
            msg = f"{directive}= in {where} needs {', '.join(gaps)}"
            if silent:
                silent_failures.append(msg)
            else:
                failures.append(msg)

    print(f"OK        {checked} kernel-backed directives across {len(units)} units"
          if not (failures or silent_failures)
          else f"          {checked} kernel-backed directives across {len(units)} units")

    print()
    if silent_failures:
        # ASCII only: this also runs from the Windows authoring machine, whose
        # console encoding mangles anything else.
        print("SILENT FAILURES -- these directives would be accepted and ignored:")
        for f in silent_failures:
            print(f"  {f}")
        print()
    if failures:
        print("FAILURES:")
        for f in failures:
            print(f"  {f}")
        print()

    total = len(failures) + len(silent_failures)
    print("KERNEL CONFIG MATCHES UNITS" if total == 0 else f"{total} PROBLEM(S)")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
