#!/bin/bash
# check-buildhost.sh — report whether this machine can build HadalOS, and
# flag the things that cause trouble hours into a build rather than at the
# start.
#
# Read-only. Safe to run anywhere, as a normal user (a few checks need root
# and will say so rather than failing).
#
#   bash scripts/check-buildhost.sh

set -uo pipefail

warn_count=0
BUILD_ROOT="${1:-/var/hadalos/build}"

hdr()  { printf '\n\033[1;36m══ %s\033[0m\n' "$*"; }
item() { printf '  %-26s %s\n' "$1" "$2"; }
good() { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; warn_count=$((warn_count+1)); }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; warn_count=$((warn_count+1)); }

# ── CPU and memory ──────────────────────────────────────────────────────
hdr "cpu / memory"
cpu_model="$(awk -F: '/model name/ {print $2; exit}' /proc/cpuinfo | sed 's/^ *//')"
threads="$(nproc)"
cores="$(awk -F: '/^cpu cores/ {print $2; exit}' /proc/cpuinfo | tr -d ' ')"
mem_gb=$(( $(awk '/MemTotal/ {print $2}' /proc/meminfo) / 1024 / 1024 ))
swap_gb=$(( $(awk '/SwapTotal/ {print $2}' /proc/meminfo) / 1024 / 1024 ))

item "cpu" "$cpu_model"
item "cores / threads" "${cores:-?} / $threads"
item "memory" "${mem_gb} GB"
item "swap" "${swap_gb} GB"

# Heavy C++ links (llvm, rust, gcc) want 1.5-2 GB per job, and some want
# 4-8 GB in a single process regardless of -j.
by_mem=$(( mem_gb / 2 ))
jobs=$(( threads < by_mem ? threads : by_mem ))
(( jobs < 1 )) && jobs=1
item "recommended MAKEOPTS" "-j${jobs} -l$((jobs + 4))"
if (( by_mem < threads )); then
    warn "memory-limited: ${mem_gb} GB caps you at -j${jobs}, below ${threads} threads"
fi
if (( swap_gb == 0 && mem_gb <= 16 )); then
    warn "no swap. One 8 GB zram device turns a rare OOM into a slow patch:"
    printf '      %s\n' "echo 'zram0 lz4 8G' >> /etc/systemd/zram-generator.conf"
fi

# ── firmware ────────────────────────────────────────────────────────────
hdr "firmware"
if [[ -d /sys/firmware/efi ]]; then
    good "UEFI — Limine installs to the ESP"
    [[ -d /sys/firmware/efi/efivars ]] && good "efivars mounted (efibootmgr will work)" \
        || warn "efivars not mounted; efibootmgr cannot register a boot entry"
else
    warn "booted in BIOS/CSM mode — Limine supports it, but the HadalOS"
    warn "  kernel-install layout is written and tested for UEFI"
fi

# ── /boot ───────────────────────────────────────────────────────────────
hdr "/boot"
boot_fs="$(findmnt -no FSTYPE /boot 2>/dev/null || echo "not a separate mount")"
boot_src="$(findmnt -no SOURCE /boot 2>/dev/null || echo -)"
boot_avail="$(df -BG --output=avail /boot 2>/dev/null | tail -1 | tr -d ' G')"
item "filesystem" "$boot_fs"
item "device" "$boot_src"
item "available" "${boot_avail:-?} GB"

case "$boot_fs" in
    vfat)
        good "vfat — this is the ESP; Limine's boot():/ resolves here"
        ;;
    ext2|ext4|xfs|btrfs)
        warn "/boot is $boot_fs, not vfat, so it is probably NOT the ESP."
        warn "  Limine reads limine.conf from the partition it was installed"
        warn "  to. Confirm where the ESP is mounted (likely /efi or"
        warn "  /boot/efi) and install Limine there."
        ;;
    *)
        warn "could not determine /boot filesystem"
        ;;
esac

# Each HadalOS kernel entry is a vmlinuz (~15 MB) plus a dracut initramfs
# (~60-120 MB). The last-known-good design keeps at least two.
if [[ -n ${boot_avail:-} ]] && (( boot_avail < 1 )); then
    bad "/boot has under 1 GB free — too tight for two kernels + initramfs"
elif [[ -n ${boot_avail:-} ]] && (( boot_avail < 2 )); then
    warn "/boot under 2 GB free; fine for two kernels, tight for more"
else
    good "room for several kernels plus their initramfs"
fi

# ── build storage ───────────────────────────────────────────────────────
hdr "build storage ($BUILD_ROOT)"
probe="$BUILD_ROOT"
while [[ ! -d $probe && $probe != / ]]; do probe="$(dirname "$probe")"; done
fstype="$(stat -f -c %T "$probe" 2>/dev/null || echo unknown)"
avail_gb="$(df -BG --output=avail "$probe" 2>/dev/null | tail -1 | tr -d ' G')"

item "nearest existing path" "$probe"
item "filesystem" "$fstype"
item "available" "${avail_gb:-?} GB"

case "$fstype" in
    tmpfs|ramfs)
        bad "RAM-backed. A stage3 extracted here will exhaust memory."
        ;;
    btrfs)
        warn "btrfs: set nodatacow BEFORE writing anything, or the build tree"
        warn "  fragments permanently. bootstrap-buildhost.sh does this, but"
        warn "  only on a directory it creates empty."
        if [[ -d $BUILD_ROOT ]]; then
            if lsattr -d "$BUILD_ROOT" 2>/dev/null | grep -q C; then
                good "nodatacow already active on $BUILD_ROOT"
            else
                warn "  $BUILD_ROOT exists WITHOUT nodatacow — remove it and"
                warn "  let bootstrap-buildhost.sh recreate it"
            fi
        fi
        ;;
    ext2/ext3|ext4|xfs)
        good "$fstype needs no special handling"
        ;;
esac

if [[ -n ${avail_gb:-} ]]; then
    if   (( avail_gb < 60 ));  then bad  "under 60 GB — not enough for a stage3 build"
    elif (( avail_gb < 100 )); then warn "60-100 GB: stages fine, full catalyst chain tight"
    else good "enough for the full stage1 + stage3 + livecd chain"
    fi
fi

# /tmp on tmpfs is normal and fine, as long as nothing builds there.
tmp_fs="$(stat -f -c %T /tmp 2>/dev/null)"
if [[ $tmp_fs == tmpfs ]]; then
    tmp_gb="$(df -BG --output=size /tmp | tail -1 | tr -d ' G')"
    item "/tmp" "tmpfs, ${tmp_gb} GB (RAM-backed)"
    warn "keep PORTAGE_TMPDIR off /tmp — it is RAM, and ${tmp_gb} GB will not"
    warn "  survive a chromium or llvm build"
fi

# ── toolchain ───────────────────────────────────────────────────────────
hdr "toolchain"
for t in git curl tar chroot mountpoint; do
    if command -v "$t" >/dev/null; then good "$t"; else bad "$t missing"; fi
done
command -v gpg >/dev/null && good "gpg (stage3 signature verification)" \
    || warn "gpg missing — stage3 signature cannot be verified"
command -v btrfs >/dev/null || [[ $fstype != btrfs ]] \
    || warn "btrfs-progs missing but the build root is btrfs"

# ── verdict ─────────────────────────────────────────────────────────────
hdr "verdict"
if (( warn_count == 0 )); then
    printf '  \033[1;32mready\033[0m — no issues found\n'
else
    printf '  \033[1;33m%d item(s) to look at above\033[0m\n' "$warn_count"
fi
printf '\n  next:  sudo ./scripts/bootstrap-buildhost.sh --root %s --dry-run\n\n' "$BUILD_ROOT"
