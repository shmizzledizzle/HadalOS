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

### The profile switch is not sufficient on its own

`make.conf` USE overrides the profile, and this machine's `make.conf` is
catalyst stage3 boilerplate that pins the wrong side of the choice:

```
USE="dist-kernel wayland screencast -systemd elogind kde plasma pipewire server udev"
```

With `-systemd` forced and `elogind` masked off by the new profile, *neither*
flag is set, and every package carrying
`REQUIRED_USE="exactly-one-of ( elogind systemd )"` fails to resolve.
`kde-plasma/plasma-meta` is the one that surfaces it, with
`firewall? ( systemd )` failing alongside for the same reason.

`/etc/portage/package.use/dbus` has the same problem and bites one step later —
systemd soft-blocks elogind, so once elogind is unmerged `sys-apps/dbus[elogind]`
either fails to build or pulls elogind back in and deadlocks the resolve.

```bash
sudo sed -i 's/-systemd elogind/systemd/' /etc/portage/make.conf
sudo sed -i 's/\belogind\b/systemd/' /etc/portage/package.use/dbus
portageq envvar USE | tr ' ' '\n' | grep -E '^-?(systemd|elogind)$'   # expect: systemd
```

Worth grepping the whole of `/etc/portage` for `elogind` before rebuilding —
anything left pointing at it is a resolve failure waiting to happen.

### Then a three-way blocker: sysvinit / systemd / elogind

Fixing USE gets every package wanting systemd, at which point Portage reports
`sysvinit`, `systemd` and `elogind` as mutually uninstallable. Two independent
causes, plus one red herring.

**`elogind` is in `@world`.** Portage is obliged to keep anything in the world
file, and systemd soft-blocks it. Deadlock:

```bash
sudo emerge --deselect sys-auth/elogind      # world file only; unmerges nothing
```

**`openrc[sysvinit]`** is on by default (`+sysvinit` in IUSE) and pulls in
`sys-apps/sysvinit`, which `systemd[sysv-utils]` blocks.

The red herring is `sys-kernel/dracut`, which appears to demand sysvinit but
does not — its dependency is an any-of that systemd satisfies:

```
|| ( >=sys-apps/sysvinit-2.87-r3  sys-apps/openrc[sysv-utils(-)]
     sys-apps/systemd[sysv-utils(+)]  … )
```

Portage listed sysvinit only because it was already installed. Once systemd
arrives with `sysv-utils`, dracut is satisfied.

```bash
echo 'sys-apps/openrc -sysvinit'    | sudo tee /etc/portage/package.use/openrc
echo 'sys-apps/systemd sysv-utils'  | sudo tee /etc/portage/package.use/systemd
```

**Turning off `openrc[sysvinit]` does not endanger the fallback.** In the
openrc ebuild that flag governs only the *dependency*; the meson option that
installs sysv-compat binaries is driven by the separate `sysv-utils` flag
(`$(meson_use sysv-utils sysvinit)` — confusingly named). `/sbin/openrc-init`
is installed unconditionally, which `qlist sys-apps/openrc` confirms.

Leave openrc's `sysv-utils` **off**. Turning it on would block
`systemd[sysv-utils]` and recreate the same conflict from the other side.

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

## Step 3c — what the rebuild does to the *running* session

The rebuild unmerges `sys-auth/elogind` while your Plasma session is still
using it. The daemon stays resident but its files are gone, so seat and session
management, screen locking, suspend and polkit authentication can all start
misbehaving before you reboot. **That is expected, not a failure.** Finish
step 4 and reboot promptly rather than trying to keep working in that session.

This is also where the two fallbacks stop being interchangeable, which is worth
being precise about:

- `init=/sbin/openrc-init` protects against **"systemd will not start."** It
  boots the same `@` subvolume — where elogind is already gone and half the
  system has been rebuilt — so it gets you a shell, not necessarily a working
  desktop.
- The **snapshot entry** protects against **"the userland is now broken."** It
  is the only thing that returns the machine to its pre-migration state.

So the snapshot entry has to exist in `limine.conf` *before* the rebuild runs,
not before the reboot. Taking the snapshot without adding a way to boot it is
the easy mistake here.

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

## Step 4b — undo what `preset-all` over-enabled

`systemctl preset-all` applies upstream defaults, which stand up a complete
parallel networking and timekeeping stack beside the one this machine actually
uses. Check and disable before rebooting:

| Preset enables | Conflicts with | Effect if left |
|---|---|---|
| `systemd-networkd` | NetworkManager | both manage the same interfaces |
| `systemd-networkd-wait-online` | — | **stalls boot ~90 s** waiting on interfaces networkd does not manage |
| `systemd-timesyncd` | `chronyd` | two NTP clients |
| `systemd-resolved` | dhcpcd-written `resolv.conf` | see below |

```bash
sudo systemctl disable systemd-networkd.service systemd-networkd-wait-online.service \
                       systemd-networkd.socket systemd-networkd-varlink.socket \
                       systemd-networkd-resolve-hook.socket systemd-networkd-varlink-metrics.socket
sudo systemctl disable systemd-timesyncd.service
sudo systemctl disable systemd-resolved.service systemd-resolved-monitor.socket \
                       systemd-resolved-varlink.socket
```

`systemd-resolved` is the subtle one. `/etc/resolv.conf` here is a **static
file** written by dhcpcd, and NetworkManager has no explicit `dns=` setting, so
it auto-detects: with resolved running NM delegates DNS to it and stops writing
`resolv.conf`, leaving whatever stale nameserver was last written in place.
Disabling resolved keeps DNS behaving exactly as it does today. Adopt it later
as a deliberate change, with `/etc/resolv.conf` symlinked to the stub.

`NetworkManager-wait-online` is fine to keep — it is the native one and waits on
connections NM actually manages.

---

## Step 4c — you cannot reboot normally after the rebuild

Between the rebuild finishing and the first systemd boot there is a window
where the machine cannot be rebooted by any usual means.
`systemd[sysv-utils]` has already replaced the sysv commands, but systemd is
not PID 1 yet, so all of them refuse:

```
/sbin/reboot   -> /usr/bin/systemctl   [sys-apps/systemd]
/sbin/shutdown -> /usr/bin/systemctl
/sbin/halt     -> /usr/bin/systemctl
```

`systemctl` answers *"System has not been booted with systemd as init system
(PID 1). Can't operate."* — which is accurate and looks alarming. The same
message appears for the step 4b `disable` calls, but those still take effect,
because enable/disable are file operations on symlinks; only the daemon-reload
afterwards fails. Verify with `find /etc/systemd/system -name '<unit>'` rather
than trusting the exit status.

PID 1 at this point is still the *original* sysvinit, resident in memory since
before its package was unmerged. `/proc/cmdline` also still shows the boot from
before any Limine entries were added.

Save your work, then:

```bash
sudo openrc-shutdown -r now      # may refuse: it signals openrc-init, not sysvinit
```

If that refuses, use the sync-then-reboot sysrq sequence. Check the mask first
— this machine ships `kernel.sysrq = 16`, which permits *only* sync; `u` needs
32 and `b` needs 128:

```bash
sync
sudo sh -c 'echo 1 > /proc/sys/kernel/sysrq'
sudo sh -c 'echo s > /proc/sysrq-trigger'   # sync
sleep 3
sudo sh -c 'echo u > /proc/sysrq-trigger'   # remount all filesystems read-only
sleep 3
sudo sh -c 'echo b > /proc/sysrq-trigger'   # reboot
```

`s` then `u` is what makes this a clean reboot rather than a power cut — once
the filesystems are read-only nothing further can be lost. Do **not** use
`systemctl -ff reboot`, which skips both.

---

## Step 5 — first boot

Reboot and pick the normal entry (`UMC 1 Gentoo Linux 6.18.41`). systemd
becomes PID 1 via `/sbin/init`; no cmdline change is needed for the default
entry.

If it fails: Limine menu → **(OpenRC, explicit)**. If the userland is broken
badly enough that even that fails → **(pre-systemd snapshot)**.

### The snapshot entry is recovery, not rollback

Booting `rootflags=subvol=@snapshots/pre-systemd` gets you a working
pre-migration userland, but the snapshot carries the *old* `/etc/fstab`, which
names `/` as `subvol=/@`. systemd's fstab-generator builds `-.mount` from that,
disagrees with what the kernel actually mounted, and you get a degraded mount
unit. Good enough to log in and fix things; not a state to stay in.

A permanent rollback means making the snapshot *be* `@`, so fstab matches
again. From the snapshot (or any live media):

```bash
sudo mount -o subvolid=5 /dev/nvme0n1p3 /mnt/btrfs-top
sudo mv /mnt/btrfs-top/@ /mnt/btrfs-top/@broken
sudo mv /mnt/btrfs-top/@snapshots/pre-systemd /mnt/btrfs-top/@
sudo umount /mnt/btrfs-top
```

The normal Limine entry then boots the restored system with a consistent fstab,
and `@broken` is kept for post-mortem until you delete it. `/home` is a
separate subvolume and is untouched by any of this.

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

Prerequisites, all verified present after phase 2:

```
PID 1                systemd          systemctl is-system-running -> running
kernel-install       /usr/bin/kernel-install
installkernel USE    dracut efistub systemd
kernel-install inspect
  Layout             efistub          (becomes hadalos below)
  Boot Root          /efi             <- resolves correctly, no override needed
```

### 3.0 Save the fallbacks first — the generator overwrites limine.conf

`hadalos-limine-update` ends with `mv "$tmp" "$CONF"`. It rewrites
`$BOOT_ROOT/limine.conf` in full, destroying every hand-written entry — which
on this machine means the OpenRC and snapshot fallbacks that phases 1 and 2
depend on.

The mechanism to keep them is built in: `/etc/hadalos/limine.d/*.conf` are
appended verbatim to the generated file. Move them there **before** anything
regenerates:

```bash
sudo cp /efi/limine.conf /root/limine.conf.phase2-backup
sudo mkdir -p /etc/hadalos/limine.d
sudo tee /etc/hadalos/limine.d/00-fallbacks.conf >/dev/null <<'EOF'
/Gentoo Linux 6.18.41 (OpenRC, explicit)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw init=/sbin/openrc-init

/Gentoo Linux 6.18.41 (systemd, EFI-stub path)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw init=/usr/lib/systemd/systemd

/Gentoo (pre-systemd snapshot)
    protocol: linux
    kernel_path: boot():/EFI/Gentoo/vmlinuz-6.18.41-gentoo-dist-bin.efi
    module_path: boot():/EFI/Gentoo/initramfs-6.18.41-gentoo-dist-bin.img
    cmdline: root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootflags=subvol=@snapshots/pre-systemd rootfstype=btrfs rw init=/sbin/openrc-init
EOF
```

These keep pointing at `/efi/EFI/Gentoo/`, which the `hadalos` layout does not
manage. Switching layout stops *future* kernels landing there but does not
delete what is already present, so the fallbacks stay pinned to 6.18.41 —
which is exactly what a last-known-good entry should do.

The NVRAM entry `Boot01FF` remains the outer backstop throughout. Leave it.

### 3.1 Overlay, cmdline, layout

The `::hadalos` overlay is not configured on this machine, and
`/etc/portage/repos.conf` **does not exist** — Gentoo ships the default repo
config at `/usr/share/portage/config/repos.conf`, so nothing has needed to
create it. `mkdir -p` first or the write silently goes nowhere and `emerge`
reports *"there are no ebuilds to satisfy"*.

```bash
sudo mkdir -p /etc/portage/repos.conf
sudo tee /etc/portage/repos.conf/hadalos.conf >/dev/null <<'EOF'
[hadalos]
location = /home/shmizzy/Documents/HadalOS-Mobile/HadalOS/overlay
auto-sync = no
EOF

portageq get_repos /                    # must now list: gentoo hadalos
portageq get_repo_path / hadalos        # must print the overlay path

# The generator reads this; without it it scrapes /proc/cmdline instead.
echo 'root=UUID=c2463ee6-5148-476b-a3d4-b7b06dab732c rootfstype=btrfs rootflags=subvol=@ rw' \
  | sudo tee /etc/kernel/cmdline

**`layout=` alone is not enough on Gentoo.** Once `installkernel` is built with
`USE=systemd`, kernel-install drives the install and Gentoo's
`05-check-config.install` requires *three* keys, aborting the whole kernel
deployment if any is unset:

```
No initrd_generator= configured by install.conf
'/usr/lib/kernel/install.d/05-check-config.install' failed with exit status 1.
```

This is not a HadalOS bug, but the package's `pkg_postinst` step 1 only
mentions `layout=`, which is incomplete on this distribution — worth fixing in
that elog text. Write all three:

```
layout=hadalos
initrd_generator=dracut
uki_generator=none
```

`52-dracut.install` acts only on the literal value `dracut`. `uki_generator`
must be non-empty to satisfy the check but must **not** be `dracut` — that
branch makes dracut emit a UKI rather than a plain initrd, and the HadalOS
plugin expects an initrd it can concatenate.

Before this migration the check never ran: `installkernel` was
`USE="dracut efistub"` without `systemd`, so the *traditional* installkernel
did the work and none of these plugins executed. `/etc/kernel/install.conf`
did not even exist.

With `layout=hadalos` the competing boot-config plugins all stand down —
`90-loaderentry` wants `bls`, `90-uki-copy` and `95-efistub-kernel-bootcfg`
want `uki` — so the HadalOS plugin solely owns `limine.conf`, and nothing
rewrites the NVRAM entries.
echo '=sys-boot/hadalos-limine-hook-0.1.0 ~amd64' | sudo tee /etc/portage/package.accept_keywords/hadalos
sudo emerge -av sys-boot/hadalos-limine-hook
```

No `init=` in `/etc/kernel/cmdline` — `/sbin/init` is systemd now, so the
default is correct and the fallbacks carry their own explicit `init=`.

Two things will then bite, both consequences of the boot layer never having run
on real hardware.

### 0a. The generated entry has no initrd — this one is unbootable

The most serious of the four, because it produces a config that looks correct
and panics on root mount. From a successful-looking kernel install:

```
90-hadalos-limine: installed 6.18.41-gentoo-dist-bin (no initrd)
hadalos-limine-update: wrote /efi/limine.conf (1 kernel(s))

/efi/hadalos/6.18.41-gentoo-dist-bin/
  vmlinuz            <- and nothing else
```

dracut ran and built an initrd; the plugin simply never found it. The cause is
a convention change:

```bash
shift 4 || true
INITRDS=("$@")        # pre-systemd-251 only
```

Before systemd 251, kernel-install passed initrds as positional arguments.
It no longer does — the generator writes into `$KERNEL_INSTALL_STAGING_AREA`
and plugins read from there. systemd's own `90-loaderentry.install` documents
the canonical collection, and the order matters because microcode must be
concatenated ahead of the initrd:

```sh
# All files listed as arguments, and staged files starting with "initrd" are installed as initrds.
for initrd in "${KERNEL_INSTALL_STAGING_AREA}"/microcode* "${@}" "${KERNEL_INSTALL_STAGING_AREA}"/initrd*; do
```

The fix mirrors that, guarding each candidate with `[[ -f ]]` so unmatched
globs are skipped under `set -u`. Reading `"$@"` alone is not a hard failure —
there is no error, no non-zero exit, just an entry with no `module_path`.

**`default_entry: 1` points at exactly this entry**, so an unattended reboot
five seconds after the menu appears boots straight into the panic. The
`00-fallbacks.conf` entries carry their own `module_path` and remain bootable,
which is the only reason this was recoverable.

**Nothing in `scripts/` exercises this code path.**
`KERNEL_INSTALL_STAGING_AREA` is never referenced in any test, and the only
reference to the hook anywhere is `mkiso.sh`. The README's *"Limine ISO
assembly | 25/25"* covers ISO assembly, not the kernel-install plugin — so
this did not slip past the suite, it was never in scope. Worth a test that
runs the plugin with a populated staging area and asserts an `initrd` lands
next to `vmlinuz`.

### 0. The service unit is never installed at all

Found on the first real merge. `src_install` calls `systemd_dounit`, which
lives in `systemd.eclass`, but the ebuild has no `inherit`:

```
line 307: systemd_dounit: command not found
 * QA Notice: command not found: systemd_dounit
```

Portage treats this as a **QA notice, not an error** — the package merges
successfully, `emerge` exits 0, and the merge list quietly contains four files
with no `.service` among them:

```
/etc/hadalos/limine.d/.keep_sys-boot_hadalos-limine-hook-0
/usr/lib/kernel/install.d/90-hadalos-limine.install
/usr/bin/hadalos-mark-boot-good
/usr/bin/hadalos-limine-update
```

So step 5 of the package's own `pkg_postinst` —
`systemctl enable hadalos-mark-boot-good.service` — cannot succeed, because
the unit does not exist. Fix:

```bash
EAPI=8

inherit systemd        # <- systemd_dounit comes from here

DESCRIPTION="..."
```

**This stacks with the bug below**, and that is the part worth internalising.
Even once the unit installs, `ConditionPathExists=/boot/hadalos` never matches
on this machine. Two independent, individually silent failures, both landing on
the same feature: last-known-good pinning would appear installed and record
nothing. Neither is visible without checking the merge list and then the
condition result by hand.

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

**Confirmed on hardware**, not merely predicted: `kernel-install inspect`
reports `Boot Root: /efi`, so the plugin writes `/efi/hadalos/<ver>/` while the
unit tests `/boot/hadalos`, which does not and will not exist.

Fix upstream by making the unit agree with the plugin — `ConditionPathExists=`
and `ReadWritePaths=` both need to follow whatever `KERNEL_INSTALL_BOOT_ROOT`
resolves to. To test here without patching the package, use a drop-in (the
empty assignment resets the inherited list):

```bash
sudo mkdir -p /etc/systemd/system/hadalos-mark-boot-good.service.d
sudo tee /etc/systemd/system/hadalos-mark-boot-good.service.d/boot-root.conf >/dev/null <<'EOF'
[Unit]
ConditionPathExists=
ConditionPathExists=/efi/hadalos

[Service]
ReadWritePaths=
ReadWritePaths=/etc/hadalos /efi
EOF
sudo systemctl daemon-reload
sudo systemctl enable hadalos-mark-boot-good.service
```

**The condition is not the only place `/boot` is assumed.**
`hadalos-mark-boot-good` itself does:

```bash
BOOT_ROOT="${KERNEL_INSTALL_BOOT_ROOT:-/boot}"
...
if [[ ! -e $BOOT_ROOT/hadalos/$RUNNING/vmlinuz ]]; then
    log "running kernel $RUNNING is not in the HadalOS boot layout; nothing to record"
    exit 0
fi
```

`KERNEL_INSTALL_BOOT_ROOT` is exported **only by kernel-install**. When systemd
runs the script there is no such variable, so `BOOT_ROOT` falls back to `/boot`,
the check fails, and it **exits 0** — the unit reports success while recording
nothing. The same default then breaks the `hadalos-limine-update` call on the
last line.

So three places need correcting, and missing any one leaves the feature inert:

| | Consequence if missed |
|---|---|
| `ConditionPathExists=` | unit skipped before it runs |
| `ReadWritePaths=` | `ProtectSystem=strict` blocks the write |
| `Environment=KERNEL_INSTALL_BOOT_ROOT=` | script exits 0 having recorded nothing |

The upstream fix is for the script to locate the boot root the way
`kernel-install` does rather than defaulting to `/boot`, or for the unit to
carry the environment itself.

Then verify — and note that the failure mode is *silence*, so an inactive
service is not evidence of success:

```bash
systemctl status hadalos-mark-boot-good.service   # must NOT say "condition failed"
cat /etc/hadalos/lastgood                          # must be non-empty after a boot
```

### 1b. Running the generator by hand does not work here either

`BOOT_ROOT` defaults to `/boot`, which exists but is empty, so a bare
`hadalos-limine-update` dies with *"no kernels under /boot/hadalos"*. It fails
safe rather than writing a bad config, but the ebuild's `pkg_postinst` tells
you to run exactly that. On this machine:

```bash
sudo KERNEL_INSTALL_BOOT_ROOT=/efi hadalos-limine-update
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

---

## Phase 3 result: the boot layer booted a machine

First boot from `/HadalOS 6.18.41-gentoo-dist-bin (current)` — generated
`limine.conf`, generated initrd, cmdline from `/etc/kernel/cmdline`:

```
/proc/cmdline: root=UUID=c246... rootfstype=btrfs rootflags=subvol=@ rw
```

Six bugs found, all by running code that had never run.

### 6. The service can never see a settled system

`hadalos-mark-boot-good` refuses to promote unless
`systemctl is-system-running` reports `running`. It never can, because the
shipped unit was:

```ini
After=multi-user.target
ExecStartPre=/usr/bin/sleep 60
[Install]
WantedBy=multi-user.target
```

A unit wanted by `multi-user.target` is part of the boot transaction, so
`is-system-running` returns `starting` for as long as it runs. The system
cannot finish booting until the unit exits; the unit refuses to act until the
system has finished booting. From the journal, the refusal was logged **1.2 ms
before** `Startup finished`:

```
02:08:18.331764  Reached target Multi-User System.
02:09:18.450505  hadalos-mark-boot-good: system state is 'starting'; not promoting
02:09:18.451670  Startup finished in ... 1min 21.041s
```

The `sleep 60` also charged every boot a minute — userspace took 1min 3.7s —
while still guaranteeing the check would fail.

**Fix: a timer.** `hadalos-mark-boot-good.timer` with `OnBootSec=90s`,
`WantedBy=timers.target`, and the service stripped of `[Install]`,
`After=multi-user.target` and the sleep. Out of the transaction, the delay is
free, and the state check can succeed.

### The pattern across all six

| # | Bug | Fails how |
|---|---|---|
| 1 | missing `inherit systemd` | silent — QA notice, unit absent |
| 2 | `ConditionPathExists=/boot` | silent — skipped by condition |
| 3 | `layout=` alone in postinst | **loud** — aborts install |
| 4 | initrd read from `"$@"` | silent — unbootable default entry |
| 5 | script defaults `BOOT_ROOT=/boot` | silent — exits 0, records nothing |
| 6 | wanted by the target it waits on | silent — logs, exits 0 |

Five of six fail silently, and four of those land on last-known-good pinning.
That is not coincidence: it is a feature that only matters once something else
has already broken, so nothing exercises it in normal operation, and every
failure mode is quiet by construction. The one loud failure came from Gentoo's
tooling, not from HadalOS.

The generalisable lesson for the mobile port: **paths that only run in a crisis
need tests that run them on purpose.** `KERNEL_INSTALL_STAGING_AREA` appears in
no test in `scripts/`; neither does any assertion that `lastgood` becomes
non-empty after a boot.

### Phase 3 verified end to end

```
02:18:14  recorded 6.18.41-gentoo-dist-bin as last known good
02:18:14  hadalos-limine-update: wrote /efi/limine.conf (1 kernel(s), last known good: ...)
02:22:32  Starting ... Finished          timer-triggered, 72 ms, correctly a no-op
```

Everything the boot layer claims to do, it now does on hardware: kernel and
initrd land in `$BOOT_ROOT/hadalos/<ver>/`, `limine.conf` is generated with the
drop-in fallbacks appended, the machine boots from it, `lastgood` is recorded
after the system settles, and the menu relabels to `[last known good]`.

The 72 ms timer run is also incidental proof the fix took: the earlier
invocation took a full 60 s because `ExecStartPre=sleep 60` was still present.

A note on reading timer state. `NextElapseUSecRealtime=` being **empty is
correct** for an `OnBootSec=` timer that has already elapsed this boot — it
fires once per boot, so there is nothing further to schedule. The value that
actually distinguishes armed from inert is `ConditionResult`:

```bash
systemctl show hadalos-mark-boot-good.timer -p ConditionResult -p LastTriggerUSec
```

`ConditionResult=no` with `LastTriggerUSec` unset is the inert case, and it
reports as `enabled` throughout — the same shape as every other silent failure
in this list.

### The refusal path — tested, and it holds

Exercised against the shipped plugin and the real `/etc/hadalos/lastgood`,
using a sandbox boot root so nothing on the ESP was at risk:

```
remove 6.18.41 (pinned)   -> exit 1, "refusing to remove ... last known good", dir survives
remove 9.9.9-test         -> exit 0, dir removed, config regenerated
```

And the §3 invariants, with a newer kernel staged beside the pin:

```
/HadalOS 6.19.0-gentoo-dist-bin (current)
/HadalOS 6.18.41-gentoo-dist-bin [last known good]     pin keeps its entry when older

/HadalOS 7.10.0 (current)                              sort -V, not lexical
/HadalOS 7.9.0
/HadalOS 7.2.0
```

A stale pin naming an uninstalled kernel is ignored rather than fatal, and a
config is still written. **First clean result of the day** — after six bugs,
this part works exactly as designed.

### And the finding that actually mattered

None of the above was testable, because `LASTGOOD_FILE` was hardcoded to
`/etc/hadalos/lastgood`. Exercising the most important behaviour in the boot
layer required write access to `/etc`, which is why nothing exercised it.

Both scripts now honour `$HADALOS_ETC`, and `scripts/test-limine-hook.sh`
covers the whole set unprivileged — refusal, unpinned removal, staging-area
initrd collection, the two-entry invariant, version ordering, stale pins,
drop-in survival, and the refusal to write an empty config. 15/15.

Verified as a *regression* test, not just a passing one: reverting only the
initrd collection fix makes it fail with exactly the two assertions that
describe that bug.

---

## Upstream carry-list

Everything below exists **only in the local overlay snapshot**, which has no
git of its own. It must be applied to the real HadalOS repo by hand.

| File | Change |
|---|---|
| `overlay/sys-boot/hadalos-limine-hook/hadalos-limine-hook-0.1.0.ebuild` | `inherit systemd`; install the timer; postinst points at the timer and documents a non-`/boot` ESP |
| `files/hadalos-mark-boot-good.service` | drop `[Install]`, `After=multi-user.target`, `ExecStartPre=sleep 60`; add `Environment=KERNEL_INSTALL_BOOT_ROOT` |
| `files/hadalos-mark-boot-good.timer` | **new** — `OnBootSec=90s`, `WantedBy=timers.target`, carries the `ConditionPathExists` |
| `files/90-hadalos-limine.install` | collect initrds from `$KERNEL_INSTALL_STAGING_AREA`; honour `$HADALOS_ETC` |
| `files/hadalos-limine-update` | honour `$HADALOS_ETC` |
| `scripts/test-limine-hook.sh` | **new** — 15 unprivileged regression tests |
| `README.md` | status table: the boot layer has booted a machine |
| `src/hadal-brokerd/src/model.rs` | two scanner tests pinning real model output (fence-line JSON) |

Separately, in the **Hadal** repo (`~/Documents/Hadal`):

| File | Change |
|---|---|
| `.gitignore` | **add `tls.key`** — a real EC private key is currently tracked |
| `rag/build_index.py` | re-point `SOURCES` from Terraria mod sources to the Gentoo corpus |
| `hadal_mcp.py` | `resolve()` routes on a caller-supplied string; needs the policy in `docs/tier-routing.md` |

The `/boot` assumption is worth a single fix rather than five: have the scripts
locate the boot root the way `kernel-install` does — check `$BOOT_ROOT`, then a
mounted ESP at `/efi`, then `/boot` — instead of defaulting. Three of the six
bugs were that assumption wearing different hats.
