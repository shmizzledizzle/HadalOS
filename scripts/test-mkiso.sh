#!/bin/bash
# test-mkiso.sh — build a real ISO from a synthetic rootfs and inspect it.
#
# mkiso.sh is the one part of the boot path that can be exercised without
# Gentoo, catalyst, or a machine to reboot: it only needs a squashfs, Limine's
# artefacts, and xorriso. So it gets tested properly rather than being written
# and hoped over.
#
# What this cannot prove is that the image boots — that needs firmware. It does
# prove the ISO is structurally what a bootloader expects: both El Torito boot
# records present, the EFI image where firmware looks, and the volume id
# matching the CDLABEL= the kernel command line searches for. Those are the
# things that are wrong when a hand-rolled ISO fails to boot.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
step() { printf '\n\033[1;36m══ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m  PASS\033[0m %s\n' "$*"; pass=$((pass+1)); }
bad()  { printf '\033[31m  FAIL\033[0m %s\n' "$*"; fail=$((fail+1)); }
note() { printf '       %s\n' "$*"; }

step "dependencies"
missing=()
for t in xorriso mksquashfs unsquashfs; do
    command -v "$t" >/dev/null || missing+=("$t")
done
if (( ${#missing[@]} )); then
    echo "installing: ${missing[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get install -y -qq xorriso squashfs-tools >/dev/null 2>&1
fi
for t in xorriso mksquashfs unsquashfs; do
    command -v "$t" >/dev/null && ok "$t available" || { bad "$t missing"; exit 1; }
done

# ── Limine artefacts ──────────────────────────────────────────────────
# Debian may not package Limine; fall back to an upstream binary release,
# which is what a build host without sys-boot/limine would do too.
step "limine artefacts"
LIMINE="$WORK/limine"
mkdir -p "$LIMINE"
if [[ -d /usr/share/limine ]]; then
    cp /usr/share/limine/* "$LIMINE/" 2>/dev/null
    ok "using system Limine"
else
    # The source tag contains no prebuilt artefacts -- Limine publishes those
    # as a `limine-binary` release asset. (There are also v<N>.x-binary
    # branches, but they lag: the newest at time of writing is v11.x while
    # releases are on 12.x, so the asset is the reliable source.)
    note "downloading the limine-binary release asset"
    if curl -sSfL -o "$WORK/limine-binary.tar.xz" \
        https://github.com/limine-bootloader/limine/releases/latest/download/limine-binary.tar.xz
    then
        tar -xf "$WORK/limine-binary.tar.xz" -C "$WORK"
        # The tarball has a single top-level directory whose name carries the
        # version, so find the artefacts rather than assuming the path.
        while IFS= read -r f; do
            cp "$f" "$LIMINE/"
        done < <(find "$WORK" -maxdepth 3 -type f \
                     \( -name 'limine-bios.sys' -o -name 'limine-bios-cd.bin' \
                        -o -name 'limine-uefi-cd.bin' -o -name 'BOOTX64.EFI' \))

        # The host tool that stamps the BIOS boot record ships as source
        # (limine.c) -- only a Windows build is prebuilt. It is a single
        # translation unit, so building it here costs a second and means this
        # test exercises bios-install rather than skipping the one step whose
        # absence makes an image boot under UEFI and hang under BIOS.
        src="$(find "$WORK" -maxdepth 3 -type f -name 'limine.c' | head -1)"
        if [[ -n $src ]]; then
            mkdir -p "$WORK/bin"
            if cc -O2 -o "$WORK/bin/limine" "$src" 2>/dev/null; then
                export PATH="$WORK/bin:$PATH"
            else
                note "could not build limine.c"
            fi
        fi
    fi
fi

command -v limine >/dev/null \
    && ok "limine executable available for bios-install" \
    || note "no limine executable; bios-install will be skipped"

got=0
for f in limine-bios.sys limine-bios-cd.bin limine-uefi-cd.bin BOOTX64.EFI; do
    [[ -r $LIMINE/$f ]] && got=$((got+1)) || note "missing $f"
done
if (( got == 4 )); then
    ok "all four Limine artefacts present"
else
    bad "only $got/4 Limine artefacts -- cannot build an ISO"
    exit 1
fi

# ── synthetic rootfs ──────────────────────────────────────────────────
step "synthetic rootfs"
ROOTFS="$WORK/rootfs"
mkdir -p "$ROOTFS/boot" "$ROOTFS/usr/bin" "$ROOTFS/etc"
# Not real kernels; mkiso.sh only locates and copies them.
head -c 2000000 /dev/urandom > "$ROOTFS/boot/vmlinuz-7.1.5-hadalos"
head -c 1000000 /dev/urandom > "$ROOTFS/boot/initramfs-7.1.5-hadalos.img"
echo "HadalOS" > "$ROOTFS/etc/hostname"
mksquashfs "$ROOTFS" "$WORK/root.squashfs" -quiet -noappend -comp zstd 2>/dev/null \
    && ok "built a squashfs ($(du -h "$WORK/root.squashfs" | cut -f1))" \
    || { bad "mksquashfs failed"; exit 1; }

# ── the thing under test ──────────────────────────────────────────────
step "mkiso.sh"
out="$(bash "$REPO/scripts/mkiso.sh" \
        --squashfs "$WORK/root.squashfs" \
        --output "$WORK/hadalos.iso" \
        --limine "$LIMINE" \
        --volid HADALOSTEST 2>&1)"
rc=$?
if (( rc == 0 )) && [[ -f $WORK/hadalos.iso ]]; then
    ok "built an ISO ($(du -h "$WORK/hadalos.iso" | cut -f1))"
else
    bad "mkiso.sh failed (rc=$rc)"
    note "$out"
    exit 1
fi
grep -q "extracting kernel and initrd" <<<"$out" \
    && ok "extracted the kernel from the squashfs rather than needing one passed in" \
    || bad "did not extract the kernel"

if command -v limine >/dev/null; then
    grep -q "installing the Limine BIOS stage" <<<"$out" \
        && ok "ran limine bios-install" \
        || bad "limine was available but bios-install did not run"
    grep -qi "UEFI only" <<<"$out" \
        && bad "warned about UEFI-only despite limine being present" \
        || ok "no UEFI-only warning"
else
    grep -qi "UEFI only" <<<"$out" \
        && ok "warned clearly that the image is UEFI-only without limine" \
        || bad "silently produced a UEFI-only image"
fi

# ── structure ─────────────────────────────────────────────────────────
step "ISO structure"
toc="$(xorriso -indev "$WORK/hadalos.iso" -toc 2>&1)"
listing="$(xorriso -indev "$WORK/hadalos.iso" -find / 2>/dev/null)"

grep -qi "HADALOSTEST" <<<"$toc" \
    && ok "volume id is HADALOSTEST (must match the CDLABEL= on the cmdline)" \
    || { bad "wrong volume id"; note "$(grep -i volume <<<"$toc" | head -2)"; }

for path in /boot/vmlinuz /boot/initrd /boot/limine.conf /LiveOS/squashfs.img \
            /EFI/BOOT/BOOTX64.EFI /boot/limine/limine-bios-cd.bin; do
    grep -qx "'$path'" <<<"$listing" \
        && ok "contains $path" \
        || bad "missing $path"
done

# ── boot records ──────────────────────────────────────────────────────
step "boot records"
report="$(xorriso -indev "$WORK/hadalos.iso" -report_el_torito plain 2>&1)"

grep -q "El Torito" <<<"$report" \
    && ok "has an El Torito catalogue" || { bad "no El Torito catalogue"; note "$report"; }
# BIOS: no-emulation boot image.
grep -qiE "boot_image.*(bios|no_emul)|El Torito boot img.*BIOS" <<<"$report" \
    && ok "BIOS boot record present" \
    || { bad "no BIOS boot record"; note "$(grep -i 'boot' <<<"$report" | head -4)"; }
# UEFI: the EFI System Partition image.
grep -qiE "UEFI|EFI" <<<"$report" \
    && ok "UEFI boot record present" \
    || { bad "no UEFI boot record"; note "$(grep -i 'boot' <<<"$report" | head -4)"; }

# ── generated menu ────────────────────────────────────────────────────
step "generated limine.conf"
conf="$(xorriso -osirrox on -indev "$WORK/hadalos.iso" \
        -extract /boot/limine.conf "$WORK/limine.conf" 2>/dev/null && cat "$WORK/limine.conf")"

grep -q "protocol: linux" <<<"$conf" && ok "uses the linux boot protocol" || bad "wrong protocol"
grep -q "CDLABEL=HADALOSTEST" <<<"$conf" \
    && ok "cmdline CDLABEL matches the ISO volume id" \
    || { bad "CDLABEL does not match the volume id -- the live root would not be found"; note "$conf"; }
grep -q "path: boot():/boot/vmlinuz" <<<"$conf" && ok "kernel path uses the boot() URI" || bad "bad kernel path"
grep -q "module_path: boot():/boot/initrd" <<<"$conf" && ok "initrd supplied as a module" || bad "no initrd"
[[ $(grep -c '^/HadalOS' <<<"$conf") -eq 3 ]] \
    && ok "three menu entries (default, verbose, nomodeset)" \
    || bad "unexpected entry count: $(grep -c '^/HadalOS' <<<"$conf")"

printf '\n\033[1m══ %d passed, %d failed\033[0m\n' "$pass" "$fail"
exit $(( fail > 0 ? 1 : 0 ))
