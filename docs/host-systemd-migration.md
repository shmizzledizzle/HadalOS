# Migrating this laptop from OpenRC to systemd

Runbook for the HP/i5-1235U Gentoo box, so it can host `hadal-brokerd` as the
architecture of record specifies. Every command here needs root, and sudo on
this machine wants a password — so all of it is run by hand.

Measured 2026-08-07. Re-verify anything that looks stale before trusting it.

---

## Do this in three phases, not one

Limine belongs in this plan twice, and the two halves land on opposite sides
of the migration:

| Phase | What | Needs systemd? |
|---|---|---|
| **1** | `sys-boot/limine`, hand-written `limine.conf` | no |
| **2** | OpenRC → systemd | — |
| **3** | `hadalos-limine-hook`, `layout=hadalos`, lastgood pinning | **yes** |

Phase 1 is pure risk reduction and must come first: it replaces "one EFI stub
entry, no menu, rescue USB if it fails" with a boot menu you can pick from.
Everything after it is recoverable in one keypress.

Phase 3 cannot come first, because `hadalos-limine-hook`'s RDEPEND is
`sys-boot/limine`, `sys-kernel/installkernel[systemd]`, `app-shells/bash`,
`sys-apps/systemd`, and `hadalos-mark-boot-good.service` is a systemd unit
leaning on `ProtectSystem=strict`, `PrivateNetwork=yes` and
`SystemCallFilter=@system-service` — the exact directives ARCHITECTURE.md §0
cites as the reason systemd was chosen.

**Phase 3 is also the point of the whole exercise.** The desktop README marks
both `Limine kernel-install integration` and `Last-known-good pinning` as
*written, untested on hardware*, and says outright that the boot layer *"has
been functionally tested against synthetic kernel trees but has never booted a
machine."* This laptop would be that machine — proving the boot layer on
hardware whose loss is survivable, before the 9800X3D exists to depend on it.

---

## Read this first: there is no way back from a failed boot

This machine has **no bootloader**. The kernel is loaded directly by UEFI as an
EFI stub, the kernel command line lives inside a UEFI variable rather than any
config file, and:

```
BootOrder: 01FF,0003          01FF = Gentoo, 0003 = USB
Timeout:   0 seconds          no menu is shown, ever
```

So there is exactly one OS boot entry and no menu to choose from. If systemd
fails to come up, the machine does not fall back — it needs a USB rescue image.
An init migration is precisely the change most likely to produce an unbootable
system, so **step 0 is not optional.**

Relevant layout:

| | |
|---|---|
| ESP | `/dev/nvme0n1p1` → `/efi`, 1 GB vfat |
| root | `/dev/nvme0n1p3` subvol `@`, btrfs, `compress=zstd:1` |
| home | subvol `@home` — **separate**, unaffected by a root rollback |
| snapshots | subvol `@snapshots` → `/.snapshots`, currently empty |
| kernel | `gentoo-kernel-bin-6.18.41`, dracut initramfs |
| free | 456 GB |

Current kernel command line, which every new boot entry must reproduce exactly
apart from the bits being changed:

```
root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw initrd=\EFI\Gentoo\initramfs-6.18.41-gentoo-dist-bin.img
```

---

## Phase 1 — Limine, before anything else

`sys-boot/limine-12.5.2` is in `::gentoo`, matching the architecture's
"Limine 12.x". Installed bare it needs no systemd and no hook.

Preconditions verified on this machine 2026-08-07:

| | |
|---|---|
| Secure Boot | **disabled** (`SecureBoot=0`, `SetupMode=0`) — no signing, the ebuild's hash-enrollment warning does not apply |
| ESP | `/dev/nvme0n1p1` → `/efi`, 923 MB free |
| LLVM | 22.1.8 with AArch64/ARM/X86/RISCV/LoongArch all enabled — no rebuild |
| build cost | 16 packages, **15 prebuilt binaries** from the binhost; only limine compiles (677 KiB source), ~55 MB download |
| initramfs | single image, microcode embedded by dracut — one `module_path` |

### 1.1 Keyword and USE

`sys-boot/limine` is `~amd64`. Narrow the USE flags to this machine's actual
firmware — the default enables every architecture Limine supports, which builds
BIOS, PXE, CD and four other UEFI targets for nothing, and drags in `mtools`
via `uefi-cd`.

```bash
echo '=sys-boot/limine-12.5.2 ~amd64' | sudo tee /etc/portage/package.accept_keywords/limine
echo 'sys-boot/limine -bios -bios-cd -bios-pxe -uefi-cd -uefi-ia32 -uefi-aarch64 -uefi-riscv64 -uefi-loongarch64 uefi-x86-64' | sudo tee /etc/portage/package.use/limine
sudo emerge -av sys-boot/limine
ls -l /usr/share/limine/BOOTX64.EFI      # must exist before continuing
```

### 1.2 Deploy to the ESP

```bash
sudo mkdir -p /efi/EFI/limine
sudo cp -v /usr/share/limine/BOOTX64.EFI /efi/EFI/limine/
```

### 1.3 Write `/efi/limine.conf`

`boot():` resolves to the partition Limine itself booted from — the ESP — which
is where the kernels already live.

```
timeout: 5

/Gentoo Linux 6.18.41
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw

/Gentoo Linux 6.18.41 (OpenRC, explicit)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw init=/sbin/openrc-init
```

The kernel file is named `.efi` because it is an EFI-stub kernel, but a stub
kernel is still a bzImage with a PE header on the front, so Limine's `linux`
protocol loads it normally.

The second entry is the one that matters later. Note that it is **not** how the
machine boots today: today PID 1 is `sys-apps/sysvinit`'s `/usr/bin/init`,
which then starts OpenRC. `init=/sbin/openrc-init` uses OpenRC's own init
instead. That distinction is the entire point — installing systemd displaces
sysvinit, so `/usr/bin/init` will be gone, while `/sbin/openrc-init` from
`sys-apps/openrc` survives.

**So test that entry now, while OpenRC is still the only init installed.** An
untested fallback is not a fallback, and this is the one moment where testing
it costs nothing.

### 1.4 Enroll the UEFI entry, keeping the old one

```bash
sudo efibootmgr --create --disk /dev/nvme0n1 --part 1 \
  --label "Limine" \
  --loader '\EFI\limine\BOOTX64.EFI' --unicode

sudo efibootmgr -v          # note the new entry's number, e.g. 0004
```

Put Limine first but **leave `01FF` in the order**. The existing EFI-stub entry
is a completely independent path to a booted system and costs nothing to keep:

```bash
sudo efibootmgr -o 0004,01FF,0003     # substitute the real number
sudo efibootmgr --timeout 5
```

### 1.5 Verify before moving on

Reboot. You should get a Limine menu with two entries.

1. Boot entry 1. Confirm normal desktop, `rc-status` healthy.
2. Reboot, boot entry 2 (`OpenRC, explicit`). Confirm it also reaches a
   desktop, and `ps -p 1 -o comm=` shows `openrc-init` rather than `init`.
3. If Limine itself fails, pick the old `UMC 1 Gentoo Linux 6.18.41` entry
   from the firmware menu — untouched and still working.

Only once both Limine entries boot is phase 2 safe to start.

Once this works, step 0's `efibootmgr` fallback entries become unnecessary —
the fallbacks live in `limine.conf`, where a kernel upgrade cannot silently eat
them. **Do step 0b, the snapshot, regardless**, and add its boot entry to
`limine.conf` then:

```
/Gentoo (pre-systemd snapshot)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootflags=subvol=@snapshots/pre-systemd rootfstype=btrfs rw init=/sbin/openrc-init
```

---

## Step 0 — build the escape hatches

Skip the `efibootmgr` entries here if phase 1 is done — `limine.conf` already
carries them. **Do not skip step 0b.**

Two fallback boot entries and a visible menu. Do this **before** touching
anything else, and reboot once into the OpenRC fallback to prove it works
while OpenRC is still the only thing installed.

```bash
# A menu you can actually reach
sudo efibootmgr --timeout 5

# Fallback 1: same kernel, forced back to OpenRC
sudo efibootmgr --create --disk /dev/nvme0n1 --part 1 \
  --label "Gentoo (OpenRC fallback)" \
  --loader '\EFI\Gentoo\vmlinuz-6.18.41-gentoo-dist-bin.efi' \
  --unicode 'root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw initrd=\EFI\Gentoo\initramfs-6.18.41-gentoo-dist-bin.img init=/sbin/openrc-init'

sudo efibootmgr -v      # confirm the entry exists and the cmdline is intact
```

`/sbin/openrc-init` is provided by `sys-apps/openrc` (0.63.3), **not** by
sysvinit. That matters: installing systemd will displace `sys-apps/sysvinit`,
but it does not touch OpenRC, so this fallback survives the migration.

**Now reboot into "Gentoo (OpenRC fallback)" and confirm it boots.** An escape
hatch you have not tested is not an escape hatch.

### Step 0b — the snapshot

`@snapshots` already exists and is empty, so this is cheap:

```bash
sudo mkdir -p /mnt/btrfs-top
sudo mount -o subvolid=5 /dev/nvme0n1p3 /mnt/btrfs-top
sudo btrfs subvolume snapshot /mnt/btrfs-top/@ /mnt/btrfs-top/@snapshots/pre-systemd
sudo btrfs subvolume list /mnt/btrfs-top | grep pre-systemd
```

Then a third boot entry that boots *the snapshot*, making rollback a firmware
menu choice rather than a rescue-USB session:

```bash
sudo efibootmgr --create --disk /dev/nvme0n1 --part 1 \
  --label "Gentoo (pre-systemd snapshot)" \
  --loader '\EFI\Gentoo\vmlinuz-6.18.41-gentoo-dist-bin.efi' \
  --unicode 'root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@snapshots/pre-systemd rw initrd=\EFI\Gentoo\initramfs-6.18.41-gentoo-dist-bin.img init=/sbin/openrc-init'
```

`/home` is a separate subvolume, so none of this snapshots or rolls back your
data — only the system. That is the behaviour you want, but know it: a
rollback will not undo anything written to `/home`.

---

## Step 1 — pin OpenRC so it cannot be reaped

`emerge --depclean` on the systemd profile will happily remove OpenRC, which
would delete the `/sbin/openrc-init` your fallback entry depends on. Put it in
`@world` so that cannot happen by accident:

```bash
sudo emerge --noreplace sys-apps/openrc
```

**Do not run `emerge --depclean` at any point until systemd is verified
working.** This is the single easiest way to strand yourself.

---

## Step 2 — switch the profile

```bash
eselect profile list | grep plasma
sudo eselect profile set default/linux/amd64/23.0/desktop/plasma/systemd
eselect profile show
```

Set it by name, not by number — the numbers shift between syncs.

---

## Step 3 — preview, then rebuild

```bash
sudo emerge -pvuDN @world      # preview first, always
```

Expected scope: 1214 packages are installed but only 16 carry the `elogind`
USE flag, so this is a targeted rebuild of those plus systemd and whatever
links `libsystemd` — on the order of tens of packages, not a world rebuild.
Qt/Plasma components are the slow ones. Budget an evening on this CPU, not a
weekend.

Portage will report a blocker between `sys-apps/systemd[sysv-utils]` and
`sys-apps/sysvinit`. That is expected and correct: systemd takes over
`/sbin/init`. Let sysvinit go. OpenRC stays.

```bash
sudo emerge -avuDN --keep-going @world
```

---

## Step 3b — do not rely on `/sbin/init` being swapped

`emerge -pv sys-apps/systemd` on the **OpenRC** profile shows
`USE="... -sysv-utils ..."`. With `sysv-utils` off, systemd does **not** install
`/sbin/init`, and an entry with no `init=` would keep booting OpenRC — you would
do the whole migration and reboot into exactly what you started with, which is
a confusing failure rather than an obvious one.

The systemd profile normally flips `sysv-utils` on, which displaces
`sys-apps/sysvinit` and makes `/sbin/init` systemd. **Check that in the step 3
preview.** But do not depend on it either way: now that Limine exists, name the
init explicitly and the question stops mattering.

Add a third entry to `/efi/limine.conf` *before* rebooting:

```
/Gentoo Linux 6.18.41 (systemd)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw init=/usr/lib/systemd/systemd
```

This is why phase 1 came first. The migration is now a *menu choice* rather
than a system-wide switch: OpenRC and systemd boot from the same root, from
adjacent entries, and picking the wrong one costs a reboot instead of a rescue
USB. Keep both entries until you have run on systemd for a while.

**Do not rebuild the initramfs as part of this.** A dracut initramfs mounts
root and execs whatever `init=` names; it does not care which init that is.
Rebuilding it here changes two things at once for no benefit.

---

## Step 4 — configure systemd before rebooting

```bash
sudo systemd-machine-id-setup
sudo systemctl preset-all                     # sane defaults

# The OpenRC default runlevel was:
#   NetworkManager chronyd dbus dhcpcd local netmount seatd sysklogd xdm
sudo systemctl enable NetworkManager.service
sudo systemctl enable chronyd.service          # or switch to systemd-timesyncd
sudo systemctl enable sddm.service             # 'xdm' under OpenRC
sudo systemctl enable bluetooth.service
```

Deliberately **not** re-enabled:

- `dhcpcd` — NetworkManager handles DHCP; running both fights over leases.
- `seatd` — `systemd-logind` takes over seat management. Leaving seatd enabled
  alongside it is asking for a session that half-works.
- `sysklogd` — journald replaces it. Keep the package for now if you like, but
  do not enable both; you will get every message written twice.
- `netmount`, `local` — no systemd equivalent needed.

Note `sysklogd` currently writes `/var/log/messages`, and that is what
`read-journal` would have had to read on OpenRC. After this migration
`journalctl` exists and the broker's `read-journal` capability works as the
architecture actually intends.

---

## Step 5 — first boot

Reboot and pick the normal entry (`UMC 1 Gentoo Linux 6.18.41`). systemd
becomes PID 1 via `/sbin/init`; no cmdline change is needed for the default
entry.

If it fails: firmware boot menu → **Gentoo (OpenRC fallback)**. If the
userland is broken badly enough that even that fails → **Gentoo (pre-systemd
snapshot)**.

### Verify

```bash
systemctl --version
systemctl is-system-running          # 'running', or 'degraded' + check below
systemctl --failed
journalctl -p err -b                 # errors this boot
loginctl                             # a seat and your session should be listed
```

Then the thing this was all for:

```bash
cd ~/Documents/HadalOS-Mobile/HadalOS/src/hadal-brokerd && cargo test
```

---

## Step 6 — only after everything works

```bash
sudo emerge --depclean -p           # PREVIEW. read every line.
```

If that list contains `sys-apps/openrc`, stop — you are about to delete your
fallback. Keep it pinned until you are confident enough to also delete the
extra boot entries, and do those two things together or not at all.

Clean up when you are done:

```bash
sudo efibootmgr -b <NUM> -B         # remove a fallback entry
sudo btrfs subvolume delete /mnt/btrfs-top/@snapshots/pre-systemd
```

---

## Known hazard: kernel reinstalls rewrite boot entries

`installkernel-68-r1` is built here with `USE="dracut efistub"`, and the efistub
logic regenerates the UEFI boot entry when the kernel is reinstalled or
upgraded, taking its command line from the running system. A
`gentoo-kernel-bin` upgrade can therefore silently drop the fallback entries
from step 0 or rewrite the cmdline.

**Until phase 1 is done, re-run `efibootmgr -v` after every kernel upgrade.**
Phase 1 removes the entire class of problem, because entries move from UEFI
NVRAM into a text file on the ESP.

---

## Phase 3 — `hadalos-limine-hook`, and two bugs waiting in it

Only after systemd is verified. `installkernel` will by then have been rebuilt
with `USE=systemd` by the profile switch, which is what provides
`kernel-install` — it does not exist on this box today.

```bash
sudo emerge -av sys-boot/hadalos-limine-hook   # from the ::hadalos overlay
echo 'layout=hadalos' | sudo tee -a /etc/kernel/install.conf
```

Two things will bite on this machine specifically, both consequences of the
boot layer never having run on real hardware.

### 1. `BOOT_ROOT` is `/efi` here, and the service unit hardcodes `/boot`

`90-hadalos-limine.install` correctly honours the environment:

```bash
BOOT_ROOT="${KERNEL_INSTALL_BOOT_ROOT:-/boot}"
DEST="$BOOT_ROOT/hadalos/$VERSION"
```

but `hadalos-mark-boot-good.service` does not:

```ini
ConditionPathExists=/boot/hadalos
ReadWritePaths=/etc/hadalos /boot
```

On this box the ESP is mounted at `/efi` and **`/boot` is empty**. So the
plugin writes `/efi/hadalos/<ver>/` while the unit's condition tests
`/boot/hadalos`, never matches, and the service is skipped **silently** —
`systemctl status` reports it as inactive-by-condition, not failed. Last-known-
good pinning would look installed and record nothing, which is the worst
possible failure mode for a safety net.

Fix upstream by making the unit agree with the plugin rather than patching it
here — `ConditionPathExists=` and `ReadWritePaths=` both need to follow
whatever `KERNEL_INSTALL_BOOT_ROOT` resolves to. Until then, verify by hand:

```bash
systemctl status hadalos-mark-boot-good.service   # must not say "condition failed"
cat /etc/hadalos/lastgood                          # must be non-empty after a boot
```

### 2. `installkernel` has no `limine` USE flag

Confirmed on 68-r1: `IUSE="dracut efistub grub refind systemd systemd-boot ugrd
uki ukify"`. ARCHITECTURE.md §3's justification for shipping a custom plugin at
all — *"sys-boot/limine has no installkernel integration (unlike GRUB and
systemd-boot)"* — is therefore still accurate, and the plugin is still needed.
Worth re-checking on each `installkernel` bump, since the day that flag appears
is the day this package can be retired.

### What to actually verify once it is running

The point of doing this here is to exercise the untested path, so exercise it:

```bash
sudo emerge --config sys-kernel/gentoo-kernel-bin   # populate /efi/hadalos
hadalos-limine-update
cat /efi/limine.conf                                 # two entries: newest + lastgood
```

Then the test that matters — install a second kernel, confirm `limine.conf`
still carries a last-known-good entry pointing at the *old* one, and confirm
the plugin refuses to remove it:

```bash
sudo emerge -av =sys-kernel/gentoo-kernel-bin-<older>
# 'remove' on the pinned version must exit 1 with the refusal message
```

That refusal path is the single most important line of the boot layer and has
never run on hardware.

---

## What this unlocks

With systemd in place, the desktop `hadal-brokerd` runs here as designed rather
than as a subset:

| Capability | On OpenRC | After |
|---|---|---|
| `emerge-*`, `query-package`, `write-config` | worked | works |
| `read-portage-log` | worked, but see below | works |
| `read-journal` | no journald (`/var/log/messages` only) | **works** |
| `restart-unit`, `unit-status` | no systemd units | **works** |
| `hadald` confinement | no `PrivateNetwork=`/`MemoryMax=` | **works** |

One thing the migration does *not* fix, and which matters more than any of the
above: `/var/log/portage` contains only `elog/` and **zero build logs**, because
`PORTAGE_LOGDIR` is unset. The flagship feature has nothing to read on this
machine either way. Add to `/etc/portage/make.conf`:

```
PORTAGE_LOGDIR="/var/log/portage"
```

Do that now — it costs nothing and every build from then on becomes training
and evaluation material.
