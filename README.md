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

The daemon holding the model runs in a network namespace with no route out.
"Local" is a kernel guarantee here, not a marketing claim.

The feature that justifies the whole project: **Portage build failures.**
Dense, structured, high-volume logs are exactly what a small local model
handles well, and no distribution ships an assistant that reads them.

---

## Status

Foundation. Nothing boots yet.

| Component | State |
|---|---|
| Architecture / decision record | written |
| `org.hadal.Broker1` interface | defined |
| Capability + polkit policy | defined |
| Limine kernel-install integration | written, untested on hardware |
| Last-known-good pinning | written, untested on hardware |
| systemd sandboxing for `hadald` / broker | written, untested on hardware |
| Overlay skeleton | created |
| `hadal-brokerd` — capability model, validators, scanner | 30/30 unit tests |
| `hadal-brokerd` — D-Bus surface | 31/31 against real D-Bus + polkit |
| `hadal` CLI (`ask` / `explain` / `why` / `status`) | working |
| Portage build-failure capture | 13/13 |
| End-to-end chain (model → polkit → executor) | 27/27 |
| Cross-file capability consistency check | passing, negative-tested |
| `sys-kernel/hadalos-sources` | not started |
| `hadalwm` | not started |
| HadalOS Shell | not started |
| catalyst specs | not started |

Everything marked *untested on hardware* has been syntax-checked only. The
boot layer is the significant remaining gap — it has been functionally tested
against synthetic kernel trees but has never booted a machine.

101 checks currently pass, on real Linux, across four suites:

```bash
wsl -d Debian -u root -- bash scripts/wsl-verify.sh
```

Then, as root on a systemd machine: `scripts/integration-test.sh` and
`scripts/e2e-test.sh`.

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

Authoring happens on the Windows dev laptop; execution happens on the build
host; git is the transport. Same loop `hadal sync` already runs.

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
