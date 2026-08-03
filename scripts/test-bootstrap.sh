#!/bin/bash
# test-bootstrap.sh — exercise bootstrap-buildhost.sh's storage setup without
# downloading a stage3.
#
# HADALOS_MIRROR is pointed at an unroutable host, so the script prepares the
# build root and then fails at the fetch. Everything before that point is what
# is under test — and it is the part that is hard to get right, because
# nodatacow must be applied while the directory is still empty and cannot be
# fixed afterwards.
#
# The btrfs cases run against a real loopback filesystem. Asserting that
# `chattr +C` was called is worthless; asserting that lsattr reports it on a
# genuine btrfs is not.
#
# Run as root: bash scripts/test-bootstrap.sh

set -uo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bootstrap-buildhost.sh"
# Not /tmp: it is frequently tmpfs, which the script correctly refuses, and
# is usually far under the 60 GB threshold anyway.
WORK="${HADALOS_TEST_DIR:-/var/tmp/hadalos-bootstrap-test}"
LOOP_IMG="$WORK/btrfs.img"
LOOP_MNT="$WORK/btrfs"

pass=0; fail=0
step() { printf '\n\033[1;36m══ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m  PASS\033[0m %s\n' "$*"; pass=$((pass+1)); }
bad()  { printf '\033[31m  FAIL\033[0m %s\n' "$*"; fail=$((fail+1)); }
note() { printf '       %s\n' "$*"; }

cleanup() {
    mountpoint -q "$LOOP_MNT" && umount "$LOOP_MNT" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

[[ $EUID -eq 0 ]] || { echo "must run as root"; exit 1; }
rm -rf "$WORK"; mkdir -p "$WORK"

# Unroutable by RFC 5737. Fails fast rather than hanging.
export HADALOS_MIRROR="http://192.0.2.1"

run_bootstrap() {
    "$SCRIPT" --root "$1" "${@:2}" 2>&1
}

# ─────────────────────────────────────────────────────────────────────────
step "the reported bug: parent directory does not exist"
out="$(run_bootstrap "$WORK/deep/nested/build" --dry-run)"
rc=$?
if grep -q "does not exist" <<<"$out"; then
    bad "still refuses a non-existent parent"
    note "$(grep 'does not exist' <<<"$out")"
elif [[ $rc -eq 0 ]]; then
    ok "accepts a path whose parent must be created"
    grep -q "will create" <<<"$out" \
        && ok "says which directory it will create" \
        || bad "did not mention creating the parent"
else
    bad "failed for another reason (rc=$rc)"
    note "$(tail -3 <<<"$out")"
fi

# ─────────────────────────────────────────────────────────────────────────
step "plain filesystem: creates the build root"
target="$WORK/plain/build"
run_bootstrap "$target" >/dev/null 2>&1
[[ -d $target ]] && ok "created $target" || bad "did not create the build root"
[[ -d $WORK/plain ]] && ok "created the missing parent" || bad "parent not created"

# ─────────────────────────────────────────────────────────────────────────
step "refuses a RAM-backed build root"
if mountpoint -q /dev/shm; then
    out="$(run_bootstrap "/dev/shm/hadalos-test/build" --dry-run)"
    grep -qi "RAM-backed" <<<"$out" \
        && ok "refused tmpfs with a clear reason" \
        || { bad "did not refuse a tmpfs root"; note "$(tail -3 <<<"$out")"; }
    rm -rf /dev/shm/hadalos-test
else
    note "no /dev/shm; skipped"
fi

# ─────────────────────────────────────────────────────────────────────────
if ! command -v mkfs.btrfs >/dev/null; then
    step "btrfs cases SKIPPED (btrfs-progs not installed)"
    note "apt-get install -y btrfs-progs"
else
    step "btrfs: real loopback filesystem"
    # A 512 MB image is nowhere near the 60 GB threshold, so the space check
    # has to be relaxed for these cases. The storage logic under test is
    # independent of capacity.
    export HADALOS_MIN_FREE_GB=0
    truncate -s 512M "$LOOP_IMG"
    mkfs.btrfs -q "$LOOP_IMG" || { bad "mkfs.btrfs failed"; }
    mkdir -p "$LOOP_MNT"
    if mount -o loop "$LOOP_IMG" "$LOOP_MNT" 2>/dev/null; then
        ok "mounted a real btrfs at $LOOP_MNT"

        # ── fresh root ──
        target="$LOOP_MNT/hadalos/build"
        run_bootstrap "$target" >/dev/null 2>&1

        if btrfs subvolume show "$target" >/dev/null 2>&1; then
            ok "created the build root as a subvolume"
        else
            bad "build root is not a btrfs subvolume"
        fi

        # The assertion that actually matters. chattr can silently no-op;
        # lsattr on a real btrfs cannot.
        if lsattr -d "$target" 2>/dev/null | grep -q C; then
            ok "nodatacow is genuinely active (verified with lsattr)"
        else
            bad "nodatacow NOT active — builds here would fragment"
            note "$(lsattr -d "$target" 2>&1)"
        fi

        # Inheritance is the whole point: files created later must get it too.
        touch "$target/probe"
        if lsattr "$target/probe" 2>/dev/null | grep -q C; then
            ok "new files inherit nodatacow"
        else
            bad "new files do NOT inherit nodatacow"
        fi

        # ── existing non-empty directory ──
        # Must not be silently "converted": its contents were already written
        # under CoW, so claiming success would be a lie.
        target2="$LOOP_MNT/hadalos/existing"
        mkdir -p "$target2"
        echo data > "$target2/preexisting"
        out="$(run_bootstrap "$target2" 2>&1)"
        if grep -q "not empty" <<<"$out"; then
            ok "warned rather than pretending to convert a populated directory"
        else
            bad "did not warn about a pre-existing populated build root"
            note "$(grep -iE 'warn|subvol' <<<"$out" | head -3)"
        fi
        [[ -f $target2/preexisting ]] \
            && ok "left existing contents alone" \
            || bad "DESTROYED existing data"

        umount "$LOOP_MNT"
    else
        bad "could not mount the loopback btrfs (kernel loop support?)"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────
printf '\n\033[1m══ %d passed, %d failed\033[0m\n' "$pass" "$fail"
exit $(( fail > 0 ? 1 : 0 ))
