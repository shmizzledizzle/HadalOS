# Migrating this laptop from OpenRC to systemd

Runbook for the HP/i5-1235U Gentoo box, so it can host `hadal-brokerd` as the
architecture of record specifies. Every command here needs root, and sudo on
this machine wants a password — so all of it is run by hand.

Measured 2026-08-07. Re-verify anything that looks stale before trusting it.

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

## Step 0 — build the escape hatches

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

`installkernel-68-r1` with EFI-stub support regenerates the boot entry when the
kernel is reinstalled or upgraded, taking its command line from the running
system. A `gentoo-kernel-bin` upgrade can therefore silently drop your fallback
entries or rewrite the cmdline.

**After any kernel upgrade, re-run `efibootmgr -v` and confirm the entries are
still what you expect.** This is worth a checklist entry for as long as this
machine has no bootloader — and installing one (`sys-boot/limine`, matching the
desktop architecture) would remove the whole class of problem. That is arguably
worth doing *before* the migration rather than after.

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
