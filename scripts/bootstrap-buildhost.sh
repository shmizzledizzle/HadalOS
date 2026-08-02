#!/bin/bash
# bootstrap-buildhost.sh — turn a running Linux box into a HadalOS build host.
#
# Creates a Gentoo stage3 chroot suitable for building HadalOS packages and,
# later, running catalyst. Does NOT touch the host's own installation: the
# chroot is self-contained under --root, so this is safe to run on the
# CachyOS desktop without disturbing it.
#
# Reference build host: Ryzen 9800X3D, 32 GB DDR5, RX 9060.
#
# Usage:
#   sudo ./bootstrap-buildhost.sh --root /var/hadalos/build
#   sudo ./bootstrap-buildhost.sh --root /var/hadalos/build --enter
#
# Run with --dry-run first. It prints every step and touches nothing.

set -euo pipefail

MIRROR="http://gentoo-mirror.flux.utah.edu"
ROOT=""
DRY_RUN=0
ENTER_ONLY=0
JOBS=""

# Stage builds, distfiles, binpkgs and a kernel tree together want real room.
# 60 GB is the point below which you will be deleting things instead of
# building them.
MIN_FREE_GB=60

log()  { printf '\033[36m::\033[0m %s\n' "$*"; }
warn() { printf '\033[33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31mxx\033[0m %s\n' "$*" >&2; exit 1; }
run()  { if (( DRY_RUN )); then printf '   would run: %s\n' "$*"; else "$@"; fi; }

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \?//'
    exit 0
}

while (( $# )); do
    case "$1" in
        --root)     ROOT="${2:?}"; shift 2 ;;
        --jobs)     JOBS="${2:?}"; shift 2 ;;
        --dry-run)  DRY_RUN=1; shift ;;
        --enter)    ENTER_ONLY=1; shift ;;
        -h|--help)  usage ;;
        *)          die "unknown argument: $1" ;;
    esac
done

[[ -n $ROOT ]] || die "--root is required"
[[ $EUID -eq 0 ]] || die "must run as root"

# ── enter an existing chroot ────────────────────────────────────────────
mount_chroot() {
    log "mounting pseudo-filesystems"
    mountpoint -q "$ROOT/proc" || run mount -t proc /proc "$ROOT/proc"
    mountpoint -q "$ROOT/sys"  || { run mount --rbind /sys "$ROOT/sys"; run mount --make-rslave "$ROOT/sys"; }
    mountpoint -q "$ROOT/dev"  || { run mount --rbind /dev "$ROOT/dev"; run mount --make-rslave "$ROOT/dev"; }
    mountpoint -q "$ROOT/run"  || { run mount --bind /run "$ROOT/run";  run mount --make-slave "$ROOT/run"; }
}

if (( ENTER_ONLY )); then
    [[ -d $ROOT/etc/portage ]] || die "$ROOT does not look like a Gentoo chroot"
    mount_chroot
    log "entering $ROOT"
    exec chroot "$ROOT" /bin/bash -l
fi

# ── preflight ───────────────────────────────────────────────────────────
log "preflight"

for tool in curl tar mountpoint chroot; do
    command -v "$tool" >/dev/null || die "missing required tool: $tool"
done

parent="$(dirname "$ROOT")"
[[ -d $parent ]] || die "parent directory $parent does not exist"

free_gb=$(( $(stat -f -c '%a * %S' "$parent") / 1024 / 1024 / 1024 ))
if (( free_gb < MIN_FREE_GB )); then
    die "only ${free_gb} GB free at $parent; need at least ${MIN_FREE_GB} GB"
fi
log "disk: ${free_gb} GB free at $parent"

nproc_count="$(nproc)"
mem_gb=$(( $(awk '/MemTotal/ {print $2}' /proc/meminfo) / 1024 / 1024 ))
log "cpu: ${nproc_count} threads, ram: ${mem_gb} GB"

# Heavy C++ translation units (llvm, webkit, rust) run 1.5-2 GB per job.
# Sizing purely on thread count is how a 32 GB box OOMs eight hours into a
# build, so take the memory-derived limit when it is lower.
if [[ -z $JOBS ]]; then
    by_mem=$(( mem_gb / 2 ))
    JOBS=$(( nproc_count < by_mem ? nproc_count : by_mem ))
    (( JOBS < 1 )) && JOBS=1
fi
LOAD=$(( JOBS + 4 ))
log "makeopts: -j${JOBS} -l${LOAD}"

# ── fetch stage3 ────────────────────────────────────────────────────────
STAGE_PATH="releases/amd64/autobuilds"
LATEST_URL="$MIRROR/$STAGE_PATH/latest-stage3-amd64-systemd.txt"

log "resolving latest systemd stage3 from $MIRROR"
if (( DRY_RUN )); then
    printf '   would fetch: %s\n' "$LATEST_URL"
    STAGE3=""
else
    STAGE3="$(curl -fsSL "$LATEST_URL" \
        | grep -v '^#' \
        | grep -oE '^[^ ]+stage3-amd64-systemd-[^ ]+\.tar\.xz' \
        | head -n1)" || die "could not resolve latest stage3"
    [[ -n $STAGE3 ]] || die "latest-stage3 file did not contain a tarball path"
    log "stage3: $STAGE3"
fi

run mkdir -p "$ROOT"

if ! (( DRY_RUN )); then
    tarball="$parent/$(basename "$STAGE3")"
    if [[ -e $tarball ]]; then
        log "reusing $tarball"
    else
        log "downloading stage3 (~300 MB)"
        curl -fL --progress-bar -o "$tarball.part" "$MIRROR/$STAGE_PATH/$STAGE3"
        mv "$tarball.part" "$tarball"
    fi

    log "verifying signature"
    if curl -fsSL -o "$tarball.asc" "$MIRROR/$STAGE_PATH/$STAGE3.asc" 2>/dev/null \
       && command -v gpg >/dev/null; then
        if gpg --verify "$tarball.asc" "$tarball" 2>/dev/null; then
            log "signature OK"
        else
            # Not fatal only because the release key may not be in the host
            # keyring yet. It is still the last checkpoint before you extract
            # 1.5 GB of someone else's userland over your disk, so it is loud.
            warn "SIGNATURE VERIFICATION FAILED OR KEY MISSING"
            warn "import the Gentoo release key:"
            warn "  gpg --keyserver hkps://keys.gentoo.org --recv-keys 13EBBDBEDE7A12775DFDB1BABB572E0E2D182910"
            read -r -p "continue anyway? [y/N] " reply
            [[ $reply == [yY] ]] || die "aborted"
        fi
    else
        warn "could not verify signature (no .asc or no gpg)"
    fi

    log "extracting to $ROOT"
    tar xpf "$tarball" --xattrs-include='*.*' --numeric-owner -C "$ROOT"
fi

# ── configure ───────────────────────────────────────────────────────────
log "writing portage configuration"

if ! (( DRY_RUN )); then
    mkdir -p "$ROOT/etc/portage/repos.conf" \
             "$ROOT/etc/portage/package.accept_keywords" \
             "$ROOT/etc/portage/package.use" \
             "$ROOT/var/cache/binpkgs"

    cat >"$ROOT/etc/portage/make.conf" <<EOF
# HadalOS build host — generated by bootstrap-buildhost.sh

COMMON_FLAGS="-O2 -pipe -march=native"
CFLAGS="\${COMMON_FLAGS}"
CXXFLAGS="\${COMMON_FLAGS}"
FCFLAGS="\${COMMON_FLAGS}"
FFLAGS="\${COMMON_FLAGS}"
RUSTFLAGS="-C target-cpu=native"

# -march=native is correct for a build host compiling for itself. It is
# WRONG for anything shipped in an ISO — catalyst specs override this,
# because a stage3 built with native flags will SIGILL on other machines.

CHOST="x86_64-pc-linux-gnu"
MAKEOPTS="-j${JOBS} -l${LOAD}"
EMERGE_DEFAULT_OPTS="--jobs=4 --load-average=${LOAD} --keep-going --with-bdeps=y"

ACCEPT_LICENSE="*"
FEATURES="buildpkg parallel-fetch candy"
PKGDIR="/var/cache/binpkgs"

USE="systemd dbus policykit elogind -gnome -kde -wayland X"

VIDEO_CARDS="amdgpu radeonsi"
INPUT_DEVICES="libinput"

GENTOO_MIRRORS="${MIRROR}"
LC_MESSAGES=C.utf8
EOF

    cat >"$ROOT/etc/portage/repos.conf/gentoo.conf" <<EOF
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /var/db/repos/gentoo
sync-type = rsync
sync-uri = rsync://rsync.gentoo.org/gentoo-portage
auto-sync = yes
EOF

    # The HadalOS overlay is bind-mounted from the git checkout rather than
    # copied, so edits on the dev laptop land here after a plain `git pull`
    # with no extra sync step.
    cat >"$ROOT/etc/portage/repos.conf/hadalos.conf" <<EOF
[hadalos]
location = /var/db/repos/hadalos
sync-type =
auto-sync = no
EOF

    # XLibre is not in ::gentoo; see ARCHITECTURE.md for why HadalOS uses it
    # rather than the effectively-unmaintained Xorg.
    cat >"$ROOT/etc/portage/repos.conf/x11libre.conf" <<EOF
[x11libre]
location = /var/db/repos/x11libre
sync-type = git
sync-uri = https://github.com/X11Libre/ports-gentoo.git
auto-sync = yes
EOF

    mkdir -p "$ROOT/var/db/repos/hadalos"
    cp --reflink=auto /etc/resolv.conf "$ROOT/etc/" 2>/dev/null || \
        cp /etc/resolv.conf "$ROOT/etc/"
fi

log "done"
cat <<EOF

Next:

  1. Bind the overlay from your git checkout:
       mount --bind /path/to/HadalOS/overlay $ROOT/var/db/repos/hadalos

  2. Enter the chroot:
       sudo $0 --root $ROOT --enter

  3. Inside:
       emerge-webrsync && emerge --sync
       emerge -avuDN @world

Do not set -march=native in anything catalyst builds. See make.conf.
EOF
