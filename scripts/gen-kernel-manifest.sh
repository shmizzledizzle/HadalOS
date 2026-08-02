#!/bin/bash
# gen-kernel-manifest.sh — produce the Manifest for sys-kernel/hadalos-kernel.
#
# Normally `ebuild ... manifest` does this, but that needs a working Portage.
# The overlay uses thin manifests, so the file is only DIST lines and can be
# generated anywhere the distfiles can be fetched and hashed — which means the
# ebuild is installable without a Gentoo host to author it on.
#
#   scripts/gen-kernel-manifest.sh [version]

set -euo pipefail

PV="${1:-7.1.5}"
BASE="linux-${PV%.*}"
MAJOR="${PV%%.*}"
CDN="https://cdn.kernel.org/pub/linux/kernel/v${MAJOR}.x"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKGDIR="$REPO/overlay/sys-kernel/hadalos-kernel"
CACHE="${HADALOS_DISTCACHE:-/var/cache/hadalos-distfiles}"

mkdir -p "$CACHE"

files=( "${BASE}.tar.xz" )
[[ ${PV##*.} != 0 ]] && files+=( "patch-${PV}.xz" )

manifest="$PKGDIR/Manifest"
: > "$manifest.tmp"

for f in "${files[@]}"; do
    if [[ -s $CACHE/$f ]]; then
        echo "cached  $f"
    else
        echo "fetch   $f"
        curl -fL --progress-bar -o "$CACHE/$f.part" "$CDN/$f"
        mv "$CACHE/$f.part" "$CACHE/$f"
    fi

    size=$(stat -c %s "$CACHE/$f")
    # Portage's required hashes for this overlay, per metadata/layout.conf.
    b2=$(b2sum "$CACHE/$f" | cut -d' ' -f1)
    s512=$(sha512sum "$CACHE/$f" | cut -d' ' -f1)
    printf 'DIST %s %s BLAKE2B %s SHA512 %s\n' "$f" "$size" "$b2" "$s512" >> "$manifest.tmp"
done

sort -o "$manifest.tmp" "$manifest.tmp"
mv "$manifest.tmp" "$manifest"

echo
echo "wrote $manifest"
cat "$manifest"
