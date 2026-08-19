#!/bin/bash
# build.sh — run the HadalOS catalyst chain.
#
# The specs carry @TIMESTAMP@, @TREEISH@ and @REPO_DIR@ placeholders, the same
# convention Gentoo's releng uses. This substitutes them and runs catalyst in
# dependency order, threading one timestamp through every stage so the
# source_subpath of each actually names the output of the last. Getting that
# wrong is the classic catalyst failure: stage3 silently builds from a
# months-old stage1 that happened to still be on disk.
#
#   catalyst/build.sh                 # everything
#   catalyst/build.sh stage1 stage3   # just those
#   catalyst/build.sh --dry-run
#
# Must run on a Gentoo host with dev-util/catalyst installed.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC_DIR="$REPO_DIR/catalyst"
WORK="${HADALOS_CATALYST_WORK:-/var/tmp/catalyst}"
DRY_RUN=0

ALL_STAGES=(stage1 stage3 livecd-stage1 livecd-stage2)

log()  { printf '\033[1;36m:: %s\033[0m\n' "$*"; }
die()  { printf '\033[1;31mxx %s\033[0m\n' "$*" >&2; exit 1; }

stages=()
while (( $# )); do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,18p' "$0" | sed 's/^# \?//'; exit 0 ;;
        -*)        die "unknown option: $1" ;;
        *)         stages+=("$1"); shift ;;
    esac
done
(( ${#stages[@]} )) || stages=("${ALL_STAGES[@]}")

(( DRY_RUN )) || [[ $EUID -eq 0 ]] || die "catalyst must run as root"
(( DRY_RUN )) || command -v catalyst >/dev/null || die "catalyst not installed (dev-util/catalyst)"

# One timestamp for the whole chain. Reused on a resumed build by exporting
# HADALOS_TIMESTAMP, so a failed livecd-stage2 does not require rebuilding
# stage1.
TIMESTAMP="${HADALOS_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"

# The tree snapshot every stage is built against. Pinning it is what makes a
# build reproducible; letting each stage resolve "latest" separately means the
# stages disagree about what Gentoo is.
if [[ -n ${HADALOS_TREEISH:-} ]]; then
    TREEISH="$HADALOS_TREEISH"
elif (( DRY_RUN )); then
    TREEISH="stable"
else
    TREEISH="$(catalyst --snapshot stable 2>&1 | grep -oE '[0-9a-f]{40}' | tail -1)" \
        || die "could not take a tree snapshot"
fi

log "timestamp: $TIMESTAMP"
log "treeish:   $TREEISH"
log "repo:      $REPO_DIR"
log "stages:    ${stages[*]}"

mkdir -p "$WORK/specs"

for stage in "${stages[@]}"; do
    src="$SPEC_DIR/$stage.spec"
    [[ -f $src ]] || die "no spec: $src"

    out="$WORK/specs/${stage}-${TIMESTAMP}.spec"
    sed -e "s|@TIMESTAMP@|$TIMESTAMP|g" \
        -e "s|@TREEISH@|$TREEISH|g" \
        -e "s|@REPO_DIR@|$REPO_DIR|g" \
        "$src" > "$out"

    if grep -q '@[A-Z_]*@' "$out"; then
        die "$stage: unsubstituted placeholder: $(grep -o '@[A-Z_]*@' "$out" | sort -u | tr '\n' ' ')"
    fi

    log "── $stage ──"
    if (( DRY_RUN )); then
        printf '   would run: catalyst -f %s\n' "$out"
        grep -E '^(target|source_subpath|version_stamp|rel_type):' "$out" | sed 's/^/   /'
    else
        catalyst -f "$out" || die "$stage failed (spec kept at $out)"
    fi
done

log "done"
if ! (( DRY_RUN )); then
    cat <<EOF

The ISO is NOT produced by catalyst. livecd-stage2 leaves a squashfs; build
the bootable image with Limine:

    scripts/mkiso.sh \\
        --squashfs $WORK/builds/hadalos/livecd-stage2-amd64-hadalos-$TIMESTAMP.squashfs \\
        --output   hadalos-amd64-$TIMESTAMP.iso

See the note at the top of catalyst/livecd-stage2.spec for why.
EOF
fi
