#!/usr/bin/env bash
# Regression tests for the Limine boot layer.
#
# Covers the paths that only execute when something has already gone wrong —
# which is exactly the set that reached hardware unexercised. Six bugs were
# found on first real use; five of them failed silently, and four landed on
# last-known-good pinning. Every one of those is asserted here.
#
# Runs unprivileged against a temporary boot root and a temporary
# $HADALOS_ETC. Needs no root, no ESP, and no installed kernel.
#
#   bash scripts/test-limine-hook.sh [path-to-plugin] [path-to-update]
#
# Defaults to the installed copies so it can also be used as a post-merge
# smoke test on a real machine.
set -uo pipefail

PLUGIN="${1:-/usr/lib/kernel/install.d/90-hadalos-limine.install}"
UPDATE="${2:-/usr/bin/hadalos-limine-update}"

pass=0 fail=0
ok()   { printf 'ok    %s\n' "$*"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %s\n' "$*"; fail=$((fail+1)); }
check(){ if [[ $2 == "$3" ]]; then ok "$1 ($2)"; else bad "$1: got '$2', want '$3'"; fi; }

for f in "$PLUGIN" "$UPDATE"; do
    [[ -x $f ]] || { echo "not executable: $f" >&2; exit 2; }
done

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
export HADALOS_ETC="$ROOT/etc"
export PATH="$(dirname "$UPDATE"):$PATH"
mkdir -p "$HADALOS_ETC/limine.d"

# Fresh boot root with the named kernel versions.
stage() {
    local boot="$ROOT/boot"; rm -rf "$boot"
    local v
    for v in "$@"; do
        mkdir -p "$boot/hadalos/$v"
        echo kernel > "$boot/hadalos/$v/vmlinuz"
        echo initrd > "$boot/hadalos/$v/initrd"
    done
    export KERNEL_INSTALL_BOOT_ROOT="$boot" KERNEL_INSTALL_LAYOUT=hadalos
    printf 'root=UUID=test rw\n' > "$HADALOS_ETC/cmdline"
}
pin() { printf '%s\n' "$1" > "$HADALOS_ETC/lastgood"; }
entries() { grep -E '^/HadalOS' "$KERNEL_INSTALL_BOOT_ROOT/limine.conf" 2>/dev/null; }

# ── the refusal: the single most important line in the boot layer ────────
stage 6.1.0 6.2.0; pin 6.1.0
out="$("$PLUGIN" remove 6.1.0 2>&1)"; rc=$?
check "refusing to remove the pinned kernel exits 1" "$rc" "1"
[[ -d $KERNEL_INSTALL_BOOT_ROOT/hadalos/6.1.0 ]] \
    && ok "pinned kernel survives an attempted removal" \
    || bad "PINNED KERNEL WAS DELETED"
[[ $out == *"last known good"* ]] \
    && ok "refusal explains itself" || bad "refusal message missing: $out"

# ── removal of anything else must still work ─────────────────────────────
"$PLUGIN" remove 6.2.0 >/dev/null 2>&1; rc=$?
check "removing an unpinned kernel exits 0" "$rc" "0"
[[ ! -d $KERNEL_INSTALL_BOOT_ROOT/hadalos/6.2.0 ]] \
    && ok "unpinned kernel is actually removed" || bad "unpinned kernel survived"

# ── initrd collection: the bug that produced an unbootable entry ─────────
stage 6.1.0; pin 6.1.0
rm -f "$KERNEL_INSTALL_BOOT_ROOT/hadalos/6.1.0/initrd"
staging="$ROOT/staging"; rm -rf "$staging"; mkdir -p "$staging"
echo initrd-content > "$staging/initrd"
echo ucode-content  > "$staging/microcode.img"
# The kernel image argument is copied with install(1), so it has to exist —
# otherwise the plugin dies under set -e before reaching the initrd logic.
echo vmlinuz-content > "$ROOT/vmlinuz-src"
KERNEL_INSTALL_STAGING_AREA="$staging" \
    "$PLUGIN" add 6.1.0 "$ROOT/entry" "$ROOT/vmlinuz-src" >/dev/null 2>&1
[[ -f $KERNEL_INSTALL_BOOT_ROOT/hadalos/6.1.0/initrd ]] \
    && ok "initrd is collected from the staging area" \
    || bad "NO INITRD — the generated entry would panic on root mount"
grep -q 'module_path' "$KERNEL_INSTALL_BOOT_ROOT/limine.conf" 2>/dev/null \
    && ok "generated entry carries a module_path" || bad "entry has no module_path"

# ── the two-entry invariant ──────────────────────────────────────────────
stage 6.1.0 6.2.0; pin 6.1.0
"$UPDATE" >/dev/null 2>&1
check "both kernels are listed" "$(entries | wc -l)" "2"
entries | head -1 | grep -q '(current)' \
    && ok "newest kernel is marked current" || bad "newest is not marked current"
entries | grep -q '6.1.0.*last known good' \
    && ok "an older pin keeps its entry and its label" \
    || bad "PIN LOST ITS ENTRY — the fallback would be a guess"

# ── version ordering ─────────────────────────────────────────────────────
stage 7.2.0 7.9.0 7.10.0; pin 7.2.0
"$UPDATE" >/dev/null 2>&1
check "sort is version-aware, not lexical" \
      "$(entries | head -1 | grep -oE '7\.[0-9.]+')" "7.10.0"

# ── a pin naming an uninstalled kernel must not wedge the generator ──────
stage 6.2.0; pin 6.1.0-gone
"$UPDATE" >/dev/null 2>&1; rc=$?
check "a stale pin is ignored rather than fatal" "$rc" "0"
[[ -s $KERNEL_INSTALL_BOOT_ROOT/limine.conf ]] \
    && ok "a config is still written when the pin is stale" || bad "no config written"

# ── drop-ins must survive regeneration ───────────────────────────────────
stage 6.1.0; pin 6.1.0
printf '/Rescue\n    protocol: linux\n' > "$HADALOS_ETC/limine.d/99-rescue.conf"
"$UPDATE" >/dev/null 2>&1
grep -q '^/Rescue' "$KERNEL_INSTALL_BOOT_ROOT/limine.conf" \
    && ok "drop-in entries are appended verbatim" \
    || bad "DROP-IN LOST — hand-written fallbacks would be destroyed"

# ── refusing to write an empty config ────────────────────────────────────
stage; mkdir -p "$KERNEL_INSTALL_BOOT_ROOT/hadalos"
"$UPDATE" >/dev/null 2>&1; rc=$?
[[ $rc -ne 0 ]] \
    && ok "refuses to write a config with no kernels" \
    || bad "wrote an empty config"

printf '\n%d/%d passed\n' "$pass" "$((pass+fail))"
[[ $fail -eq 0 ]]
