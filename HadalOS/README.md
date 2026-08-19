# HadalOS

A Gentoo-derived distribution with a locally-hosted AI as an actual system
component — not an application that ships in the image.

- **Base** — Gentoo, amd64, systemd
- **Boot** — Limine, mainline `torvalds/linux`, last-known-good pinning
- **Assistant** — [Hadal](https://github.com/shmizzledizzle/Hadal) behind a
  D-Bus system service and a capability broker
- **Desktop** — custom X11 environment on XLibre: `hadalwm` (Rust/x11rb) +
  HadalOS Shell (C#/Avalonia)

Read [ARCHITECTURE.md](ARCHITECTURE.md) first. It is the decision record.

---

## The one idea

Windows put Copilot in the shell. That is a chat window with privileges.

HadalOS takes the opposite route. Hadal is reachable by *any* process on the
system over `org.hadal.Broker1`, and it has *no* privileges. It cannot run a
command, because there is no code path from model output to a command
interpreter — it proposes typed actions from a closed enum, and a separate
privileged broker containing no model code validates them, asks polkit, and
executes via direct API.

The unit for the daemon holding the model carries `PrivateNetwork=yes`, so that
*can* be a kernel guarantee rather than a marketing claim.

**It is not one today.** `hadald` is currently backed by a remote inference
endpoint, which needs egress, which needs that directive relaxed by a
deliberate drop-in. Stated precisely, because the difference matters:

| Property | Status |
|---|---|
| **Safety** — no path from model output to a command interpreter | **intact** |
| **Privacy** — system data never leaves the machine | **broken** |

Safety survives because the broker was built not to trust the model. Proposals
are typed, validated and polkit-gated wherever the model runs. Privacy does not
survive, and the payloads are the sharp end: Portage build logs and journal
excerpts carry hostnames, usernames, absolute paths and occasionally tokens.
`/var/log/hadal/egress.log` exists so that *"what left this machine"* has an
answer that is not a promise.

The route back is a resident local model for anything touching system state,
with the tier chosen from the capability table rather than by whoever calls —
designed in [docs/tier-routing.md](../docs/tier-routing.md), not yet built.

The feature that justifies the whole project: **Portage build failures.**
Dense, structured, high-volume logs are exactly what a small local model
handles well, and no distribution ships an assistant that reads them.

---

## Status

Foundation. **The boot layer boots a machine.**

| Component | State |
|---|---|
| Architecture / decision record | written |
| `org.hadal.Broker1` interface | defined |
| Capability + polkit policy | defined |
| Limine kernel-install integration | **booted real hardware** — 6 bugs found and fixed |
| Last-known-good pinning | **working on hardware**, 15/15 regression tests |
| systemd sandboxing for `hadald` / broker | written, untested — but the same directive set ran clean via `hadalos-mark-boot-good` |
| Overlay skeleton | created |
| `hadal-brokerd` — capability model, validators, scanner | 30/30 unit tests |
| `hadal-brokerd` — D-Bus surface | 31/31 against real D-Bus + polkit |
| `hadal` CLI (`ask` / `explain` / `why` / `status`) | working |
| Portage build-failure capture | 13/13 |
| End-to-end chain (model → polkit → executor) | 27/27 |
| `sys-kernel/hadalos-kernel` (7.1.5 pinned + 9999 mainline) | written, Manifest generated, **never built** |
| Kernel config fragment | 98 symbols, checked against every unit directive |
| catalyst specs (stage1 → stage3 → livecd ×2) | written, chain validated, **never run** |
| Limine ISO assembly | 25/25 — real ISO built and inspected |
| Limine hook regression suite | 15/15 — `scripts/test-limine-hook.sh`, unprivileged |
| Static consistency checks (capability / kernel / catalyst) | passing, all negative-tested |
| ~~`hadalwm`~~ (X11) | **abandoned** — replaced by cusk, ARCHITECTURE.md §0 |
| ~~HadalOS Shell~~ (C#/Avalonia) | **abandoned** — replaced by the cusk shell clients |
| `cusk` — nested compositor, floating, tiling | working, 33 milestones in `docs/cusk.md` |
| `cusk` — tty backend: DRM, libseat, libinput, VT switch | **verified from a VT on this laptop** |
| `cusk` — layer-shell, dmabuf, panel, chrome, workspaces | working; layer-shell verified against waybar |
| `cusk-dock` (dock, pinning, StatusNotifierItem tray) | working; tray needs an app to volunteer |
| `cusk-launcher`, `cusk-settings` | working |
| `cusk` as a display-manager session | **ebuild + session file written, never selected at a login screen** |
| `hadald` — remote-backed model host | working; 15 unit + 10 end-to-end |
| `hadald` — local reflex model, tier routing | designed in `docs/tier-routing.md`, **not built** |
| ebuilds for cusk / shell / broker / hadald | **written, never merged** |
| `sys-apps/hadalos-release` (os-release identity) | **written, never merged** |
| catalyst specs | written, chain validated, **never run** |

Everything marked *untested on hardware* has been syntax-checked only.

The boot layer is no longer in that set. On 2026-08-07 it booted a Gentoo
laptop: kernel and initrd installed to `$BOOT_ROOT/hadalos/<ver>/`,
`limine.conf` generated, machine booted from it, and last-known-good recorded
once the system settled.

Running it found **six bugs, five of which failed silently**, four of those
landing on last-known-good pinning:

| Bug | How it failed |
|---|---|
| `systemd_dounit` without `inherit systemd` | QA notice; package merged, unit never installed |
| `ConditionPathExists=/boot` on a `/efi` ESP | unit skipped, reported inactive-by-condition |
| initrd read from `"$@"` (pre-systemd-251) | entry with no `module_path` — **unbootable, and the default** |
| `BOOT_ROOT` defaulting to `/boot` in the script | exited 0 having recorded nothing |
| unit `WantedBy=` the target it waits to finish | `is-system-running` can never return `running` |
| `layout=` alone in `pkg_postinst` | *(loud)* Gentoo's `05-check-config` aborts the install |

That distribution is not chance. Last-known-good only executes once something
else has already broken, so nothing exercises it in normal operation and every
failure mode is quiet by construction. The single loud failure came from
Gentoo's tooling, not from here.

The root cause was testability, not care: `LASTGOOD_FILE` was hardcoded to
`/etc/hadalos/lastgood`, so exercising the most important behaviour in the boot
layer needed write access to `/etc`. Both scripts now honour `$HADALOS_ETC`,
and `scripts/test-limine-hook.sh` covers the whole set unprivileged.

**Paths that only run in a crisis need tests that run them on purpose.**

One useful side effect: `hadalos-mark-boot-good.service` carries
`ProtectSystem=strict`, `ReadWritePaths=`, `PrivateNetwork=yes`,
`NoNewPrivileges=yes`, `RestrictAddressFamilies=AF_UNIX` and
`SystemCallFilter=@system-service` — the directive set §0 cites as the reason
systemd was chosen at all. That set has now run clean on real hardware, writing
the files it was permitted and nothing else. It is not proof the `hadald` and
broker units are right, but it is the first evidence the approach works outside
a syntax check. The one thing it did catch: `ReadWritePaths=` must name the
same boot root the service actually writes to, or `ProtectSystem=strict`
silently blocks the write.

141 checks currently pass, on real Linux, across six suites:

```bash
wsl -d Debian -u root -- bash scripts/wsl-verify.sh
bash scripts/test-limine-hook.sh          # no root, no ESP, no kernel needed
```

Then, as root on a systemd machine: `scripts/integration-test.sh`,
`scripts/e2e-test.sh` and `scripts/test-mkiso.sh`.

The kernel and catalyst layers are the honest exception — they are validated
statically but have never been built, because that needs a Gentoo host. See
[catalyst/README.md](catalyst/README.md).

---

## Getting to a build host

Reference host: Ryzen 9800X3D, 32 GB DDR5, RX 9060. Needs ≥60 GB free.

```bash
sudo ./scripts/bootstrap-buildhost.sh --root /var/hadalos/build --dry-run
```

Then drop `--dry-run`, bind the overlay, and enter:

```bash
sudo mount --bind "$PWD/overlay" /var/hadalos/build/var/db/repos/hadalos
sudo ./scripts/bootstrap-buildhost.sh --root /var/hadalos/build --enter
```

---

## Workflow

This changed. Authoring used to happen on a Windows laptop with execution on a
separate build host and git as the transport. It now happens on the machine
being converted: a Gentoo laptop, systemd, booted by Limine through the
HadalOS entries, carrying the `::hadalos` overlay.

That is a better arrangement than it sounds like an accident. The boot layer's
six bugs were all found by running it on hardware and five of them were silent;
authoring somewhere that cannot execute is what let them stay silent. The
9800X3D host in ARCHITECTURE.md §0 is still the right home for `catalyst` and
for anything needing 60 GB, and neither of those is the daily loop.

`.gitattributes` pins LF on everything that executes on Linux. Do not relax
it — a CRLF shebang fails as `bad interpreter: /bin/bash^M`.

---

## Layout

```
overlay/            ::hadalos ebuild overlay
  sys-boot/hadalos-limine-hook/    kernel-install plugin, lastgood pinning
catalyst/           stage + livecd specs
dbus/               interface definition + system bus policy
policy/             polkit capability actions
systemd/            unit files
src/                brokerd, hadalwm, shell, greeter
scripts/            build-host bootstrap
```
