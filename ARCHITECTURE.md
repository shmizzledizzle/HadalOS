# Trench Linux — Architecture of Record

A Gentoo-derived Linux distribution with a locally-hosted AI (Hadal) as a
first-class system component, booted by Limine, running mainline Linux, with a
custom X11 desktop environment.

Everything here is a decision that has been made. Open questions live in
`docs/open-questions.md`, not here.

---

## 0. Decisions

| Area | Decision | Date |
|---|---|---|
| Base | Gentoo, `amd64`, **systemd** profile | 2026-08-01 |
| Init | systemd — required for D-Bus system services, socket activation, and the sandboxing directives that make the capability broker enforceable | 2026-08-01 |
| Bootloader | Limine (`sys-boot/limine`, 12.x) | 2026-08-01 |
| Kernel | mainline `torvalds/linux`, pinned per release, via `sys-kernel/trench-sources` | 2026-08-01 |
| X server | **XLibre** (`X11Libre/ports-gentoo` overlay), not Xorg | 2026-08-01 |
| WM core | Rust + `x11rb` | 2026-08-01 |
| Shell UI | C# / Avalonia | 2026-08-01 |
| Broker | Rust + `zbus` | 2026-08-01 |
| Greeter | custom, against `greetd` | 2026-08-01 |
| Release eng | `catalyst` (stage1→3 + livecd) + binhost | 2026-08-01 |
| Build host | Ryzen 9800X3D / 32 GB DDR5 / RX 9060 | 2026-08-01 |

### Why XLibre and not Xorg

This is the load-bearing justification for choosing X11 in 2026. Xorg upstream
has become effectively unmaintained — major releases stopped and even bugfix
releases went rare. XLibre forked it in June 2025, shipped its first stable
25.1 series in June 2026 with active CVE remediation, and is now the default
X11 server in eleven distributions. It is not in `::gentoo`, but
`X11Libre/ports-gentoo` provides ebuilds.

So the choice is not "legacy X vs. modern Wayland." It is "the maintained X
server, on which writing a compliant window manager is a tractable project"
vs. "writing a Wayland compositor," which is a categorically larger job.

### Why mainline-tip is safe *here* specifically

Riding `torvalds/linux` normally means fighting out-of-tree modules on every
rebase. Trench's reference hardware is RX 9060 (RDNA4) and Intel Iris Xe —
both fully in-tree. There is no proprietary NVIDIA module to lag behind the
kernel. RDNA4 + ROCm actively *wants* new kernels.

The mitigation for everything else is structural, not optional: **Limine always
carries at least two entries — newest and last-known-good.** See §3.

---

## 1. Layer map

```
┌─────────────────────────────────────────────────────────────┐
│  Trench Shell (C#/Avalonia)                                 │
│  panel · launcher · settings · Hadal overlay (hotkey)       │
├─────────────────────────────────────────────────────────────┤
│  trenchwm (Rust/x11rb)          │  trench-greeter (greetd)  │
├─────────────────────────────────┴───────────────────────────┤
│  XLibre X server                                            │
├─────────────────────────────────────────────────────────────┤
│  D-Bus system bus                                           │
│     org.hadal.Broker1  ←── the OS-component boundary        │
├─────────────────────────────────────────────────────────────┤
│  hadal-brokerd (Rust/zbus)   ── policy · capabilities       │
│       │                          polkit-gated execution     │
│       ↓                                                     │
│  hadald  ── model host (Ollama), sandboxed, resource-capped │
│       ↓                                                     │
│  reflex model (1–3B, resident) │ deep model (on demand/LAN) │
├─────────────────────────────────────────────────────────────┤
│  systemd · Portage · mainline kernel · Limine               │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. Hadal as a system component

### 2.1 The rule

**The model never gets a shell.** Not sandboxed, not restricted, not
"read-only mode." There is no code path from model output to a command
interpreter.

Hadal emits *typed proposed actions* — an enum, with typed and validated
parameters. `emerge-apply` takes a list of package atoms, not a command
string. `restart-unit` takes a unit name matched against a pattern, not
arguments to `systemctl`. The broker validates, then polkit authorizes, then
the broker executes via a direct API. The model's output is data all the way
through.

This is the single most important design constraint in the project. An LLM
with an unbrokered path to root on your own machine is how you lose a
filesystem.

### 2.2 Process split

- **`hadald`** — model host. Wraps the existing Hadal/Ollama stack. Socket
  activated. `MemoryMax=`/`CPUQuota=` so the assistant can never starve the
  desktop. `PrivateNetwork=yes` unless the `network-lookup` capability is
  granted. `ProtectSystem=strict`, `NoNewPrivileges=yes`, seccomp filtered.
  Has no privileges of its own and no direct access to anything interesting.

- **`hadal-brokerd`** — owns `org.hadal.Broker1` on the system bus. Holds the
  capability table and the policy. Talks to `hadald` over a private socket.
  This is the only component with real privilege, and it contains no model
  code — it is a validator and an executor.

### 2.3 Capability tiers

| Tier | Default | Capabilities |
|---|---|---|
| Read | allow (active session) | `read-journal`, `read-portage-log`, `read-path`, `query-package` |
| Inspect | allow (active session) | `emerge-pretend`, `unit-status` |
| Mutate | `auth_admin` every time | `restart-unit`, `emerge-apply`, `write-config` |
| Egress | deny by default | `network-lookup` |

Each is a distinct polkit action ID, so the Settings surface can expose
per-capability allow/ask/never without inventing its own policy store.

`read-path` is constrained by prefix allowlist — not "the model may read
files," but "the model may read files under these roots."

### 2.4 Integration points

**Portage build-failure explanation.** A `/etc/portage/bashrc` hook captures
the failing build log and hands it to the broker. This is the flagship
feature: Gentoo build failures are dense, structured, high-volume logs, which
is precisely the workload a 7B handles well — and no distribution ships this.
The RAG index (reusing Hadal's existing resumable `build_index.py`) is
re-pointed at the Gentoo wiki, the handbook, the ebuild tree, and kernel
`Documentation/`.

**journald.** `trench why` — reads the previous boot's failed units and
explains them.

**Shell.** A zsh/bash widget that turns a natural-language line into a
*proposed* command, rendered for confirmation and never auto-run. Plus a
`command_not_found_handler` backed by portage's file list.

**Desktop.** Hotkey overlay panel; per-window context ("explain this error
dialog"); settings search answered by Hadal.

### 2.5 Deferred to v2

Boot-rescue Hadal (Limine's fallback entry drops to a rescue rootfs carrying
the reflex model + index, explaining offline why boot failed) and the
conversational ISO installer. Both are the best demonstrations of the whole
idea. Both roughly double v1 scope, so they are v2 — but the Limine
two-entry layout in §3 is designed now so that v2 doesn't require re-doing it.

### 2.6 The RAM floor

A distribution that requires 20 GB free for its assistant is a distribution
nobody else can run. Tiering is mandatory:

- **reflex** (1–3B, always resident): shell widget, settings search, routing.
- **deep** (7B–30B, on demand or over LAN): build failures, real diagnosis.

Hadal's existing `miniHost`/`deepHost` config already models exactly this
split — the distro consumes it rather than reinventing it.

---

## 3. Boot

`sys-boot/limine` has no installkernel integration (unlike GRUB and
systemd-boot), so Trench provides its own `kernel-install` plugin that
generates `limine.conf` from installed kernels.

Invariant: **the generated config always contains a last-known-good entry**,
pinned in `/etc/trench/lastgood`, and never garbage-collects the kernel it
points at. Riding mainline without this is how you end up at a UEFI shell.

Limine also gives menu theming for free, which is where distro branding lands.

---

## 4. Repository layout

```
overlay/          Gentoo ebuild overlay (::trench)
catalyst/         stage + livecd specs
dbus/             D-Bus interface definitions and bus policy
policy/           polkit action definitions
systemd/          unit files
src/hadal-brokerd Rust — D-Bus service, capability broker
src/trenchwm      Rust — X11 window manager
src/trench-shell  C#/Avalonia — panel, launcher, settings, Hadal overlay
src/trench-greeter greetd greeter
scripts/          build-host bootstrap and release tooling
```

---

## 5. Workflow

Authoring happens on the Windows dev laptop. Execution happens on the 9800X3D
build host. Git is the transport — the same loop `hadal sync` already runs.

Nothing in this repo may assume it can reach the build host directly.
