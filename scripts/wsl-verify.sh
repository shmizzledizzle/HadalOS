#!/bin/bash
# wsl-verify.sh — run the HadalOS checks on real Linux from the Windows
# authoring machine.
#
#   wsl -d Debian -u root -- /mnt/c/.../HadalOS/scripts/wsl-verify.sh
#
# Building directly on /mnt/c goes through 9p and is roughly an order of
# magnitude slower than the WSL filesystem, so sources are copied to
# $HADALOS_BUILD (default ~/hadalos-build) and built there. The copy
# deliberately preserves an existing target/ directory so rebuilds stay
# incremental.
#
# Everything this runs also runs on the real build host. Nothing here is
# WSL-specific except the copy.

set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${HADALOS_BUILD:-$HOME/hadalos-build}"
WORKSPACE="$BUILD/src"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=always

step() { printf '\n\033[1;36m══ %s\033[0m\n' "$*"; }
die()  { printf '\033[1;31mxx %s\033[0m\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo not found; install rustup (Debian's rustc is too old)"

step "syncing $SRC -> $BUILD"
mkdir -p "$BUILD"
# tar rather than cp -r so target/ in the destination survives, and rather
# than rsync so this has no dependency beyond coreutils and tar.
(cd "$SRC" && tar -cf - \
    --exclude='./src/target' \
    --exclude='./src/hadal-brokerd/target' \
    --exclude='./.git' \
    .) | (cd "$BUILD" && tar -xf -)
printf 'synced %s files\n' "$(find "$BUILD" -type f -not -path '*/target/*' | wc -l)"

step "shell syntax"
while IFS= read -r f; do
    printf '  %-64s ' "${f#$BUILD/}"
    bash -n "$f" && echo OK
done < <(find "$BUILD/scripts" "$BUILD/overlay" -type f \
            \( -name '*.sh' -o -name 'hadalos-*' -o -name '*.install' \) \
            -not -name '*.service')

step "capability consistency"
python3 "$BUILD/scripts/check-consistency.py" | tail -8

bash "$BUILD/scripts/test-portage-hook.sh" | tail -20

step "cargo test (native linux)"
cd "$WORKSPACE"
cargo test --workspace

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo build --release"
cargo build --workspace --release
for b in hadal-brokerd hadal; do
    ls -lh "$WORKSPACE/target/release/$b" | awk -v n="$b" '{print "  " n ": " $5}'
done

printf '\n\033[1;32m══ all checks passed\033[0m\n'
