# HadalOS — Architecture of Record

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
| Kernel | mainline `torvalds/linux`, pinned per release, via `sys-kernel/hadalos-sources` | 2026-08-01 |
| ~~X server~~ | ~~**XLibre**, not Xorg~~ — **superseded**, see below | ~~2026-08-01~~ |
| ~~WM core~~ | ~~Rust + `x11rb`~~ — **superseded**, see below | ~~2026-08-01~~ |
| ~~Shell UI~~ | ~~C# / Avalonia~~ — **superseded**, see below | ~~2026-08-01~~ |
| Display protocol | **Wayland**, not X11 | 2026-08-07 |
| WM core | Rust + `smithay` — **cusk** | 2026-08-07 |
| Shell UI | Rust + `iced` — dock, launcher, settings, as Wayland clients | 2026-08-07 |
| Legacy apps | XWayland, rootless | 2026-08-07 |
| Broker | Rust + `zbus` | 2026-08-01 |
| Greeter | ~~custom, against `greetd`~~ — SDDM for now, see below | 2026-08-19 |
| Release eng | `catalyst` (stage1→3 + livecd) + binhost | 2026-08-01 |
| Delivery | **conversion of a running Gentoo install**, ahead of any ISO | 2026-08-19 |
| Build host | Ryzen 9800X3D / 32 GB DDR5 / RX 9060 | 2026-08-01 |

### The X11 rows above are superseded

Three rows in that table describe a plan that was abandoned on 2026-08-07, and
the decision record is [docs/cusk.md](../docs/cusk.md) §1. They are struck
through rather than deleted because the reasoning that produced them was sound
and is worth being able to find.

The short version: the original argument was not "legacy X vs. modern Wayland",
it was that writing a *compliant X11 window manager* is tractable where writing
a *Wayland compositor* is "a categorically larger job". The X11 half of that
still stands. What changed is that `smithay` means a Wayland compositor is no
longer a matter of implementing the protocol — surfaces, seats, outputs,
xdg-shell, layer-shell and XWayland glue come with it, and two shipping
desktops are built on it. The job became "implement window management on top of
a display server", which is the job the X11 plan already was. It is still the
larger option; it is no longer *categorically* larger, and that word was
carrying the decision.

The Shell UI row went with it for a plainer reason: the panel is drawn by the
compositor, and `iced` cannot speak layer-shell, so the shell became Wayland
clients in the same language as everything else rather than a C# process.

What this costs is recorded in cusk.md §2 and is not hidden here: the
compositor owns every frame, owns input, needs `xdg-desktop-portal` for
anything privileged, hosts XWayland as a second window-management code path,
and — the one that raises the bar on error handling from *should* to *must* —
**a compositor crash takes every client with it.**

### The greeter row, and delivery

`greetd` with a custom greeter is still the intended end state. It is not what
is installed, and recording the intent as though it were the state is how this
project produced six silent boot-layer bugs. Today cusk is offered as one
session among others by the display manager that was already there, and the
existing desktop stays installed and stays default. That ordering is the same
two-entry principle as the Limine layout in §3: the recovery path exists before
it is needed, not after.

Delivery changed with it. §5 of this document describes authoring on a Windows
laptop and building elsewhere; the actual route to a running HadalOS turned out
to be converting a Gentoo install in place — this laptop now boots through
Limine with last-known-good pinning and carries the HadalOS overlay. An ISO
still matters and `catalyst/` still describes it, but a distribution that has
converted one real machine is further along than one that has produced an image
nobody has booted.

### Why XLibre and not Xorg — superseded, retained for its reasoning

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
rebase. HadalOS's reference hardware is RX 9060 (RDNA4) and Intel Iris Xe —
both fully in-tree. There is no proprietary NVIDIA module to lag behind the
kernel. RDNA4 + ROCm actively *wants* new kernels.

The mitigation for everything else is structural, not optional: **Limine always
carries at least two entries — newest and last-known-good.** See §3.

---

## 1. Layer map

```
┌─────────────────────────────────────────────────────────────┐
│  Shell clients (Rust/iced, layer-shell)                     │
│  cusk-dock · cusk-launcher · cusk-settings                  │
├─────────────────────────────────────────────────────────────┤
│  cusk (Rust/smithay)           │  SDDM  (greetd: intended)  │
│  compositor · panel · tiling   │                            │
│  and floating · XWayland       │                            │
├─────────────────────────────────┴───────────────────────────┤
│  Wayland  ·  wlr-layer-shell  ·  xdg-shell  ·  dmabuf       │
├─────────────────────────────────────────────────────────────┤
│  D-Bus system bus                                           │
│     org.hadal.Broker1  ←── the OS-component boundary        │
├─────────────────────────────────────────────────────────────┤
│  hadal-brokerd (Rust/zbus)   ── policy · capabilities       │
│       │                          polkit-gated execution     │
│       ↓                                                     │
│  hadald  ── model host, unprivileged, resource-capped       │
│       ↓                                                     │
│  reflex model (1–3B, resident)  ── INTENDED, not built      │
│  deep model  ── today a REMOTE endpoint, not LAN, not local │
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

### 2.4 What the capability system does not do

**polkit authorizes root for everything.** A session opened by a root-owned
client is permitted every capability, including the Mutate tier, with no
prompt. This is polkit's model working as designed — root is already
omnipotent, and gating it against itself would be theatre — but it has a
consequence worth stating plainly rather than discovering later:

> For a root caller, the client-side confirmation is the only gate. A
> compromised root client could call `Execute` directly and skip it.

This was found by running the end-to-end test as root, where every denial
assertion passed for the wrong reason. The test now runs as an unprivileged
user, which is the case that actually matters.

The threat being defended against is *prompt injection producing a bad
proposal* — a build log or a web page that talks the model into suggesting
something destructive. It is not a malicious root user, which no
userspace design can address.

The practical upshot inverts the usual pattern:

> **Do not run `hadal` as root.** You never need to. The broker already holds
> the privilege and executes on your behalf once polkit agrees, so running the
> client as your own user gets you the full capability set *and* keeps the
> authorization gate meaningful. `sudo hadal` is strictly worse than `hadal`.

### 2.5 Integration points

**Portage build-failure explanation.** A `/etc/portage/bashrc` hook captures
the failing build log and hands it to the broker. This is the flagship
feature: Gentoo build failures are dense, structured, high-volume logs, which
is precisely the workload a 7B handles well — and no distribution ships this.
The RAG index (reusing Hadal's existing resumable `build_index.py`) is
re-pointed at the Gentoo wiki, the handbook, the ebuild tree, and kernel
`Documentation/`.

**journald.** `hadal why` — reads the previous boot's failed units and
explains them.

**Shell.** A zsh/bash widget that turns a natural-language line into a
*proposed* command, rendered for confirmation and never auto-run. Plus a
`command_not_found_handler` backed by portage's file list.

**Desktop.** Hotkey overlay panel; per-window context ("explain this error
dialog"); settings search answered by Hadal.

### 2.6 Deferred to v2

Boot-rescue Hadal (Limine's fallback entry drops to a rescue rootfs carrying
the reflex model + index, explaining offline why boot failed) and the
conversational ISO installer. Both are the best demonstrations of the whole
idea. Both roughly double v1 scope, so they are v2 — but the Limine
two-entry layout in §3 is designed now so that v2 doesn't require re-doing it.

### 2.7 The RAM floor

A distribution that requires 20 GB free for its assistant is a distribution
nobody else can run. Tiering is mandatory:

- **reflex** (1–3B, always resident): shell widget, settings search, routing.
- **deep** (7B–30B, on demand or over LAN): build failures, real diagnosis.

Hadal's existing `miniHost`/`deepHost` config already models exactly this
split — the distro consumes it rather than reinventing it.

---

## 3. Boot

`sys-boot/limine` has no installkernel integration (unlike GRUB and
systemd-boot), so HadalOS provides its own `kernel-install` plugin that
generates `limine.conf` from installed kernels.

Invariant: **the generated config always contains a last-known-good entry**,
pinned in `/etc/hadalos/lastgood`, and never garbage-collects the kernel it
points at. Riding mainline without this is how you end up at a UEFI shell.

Limine also gives menu theming for free, which is where distro branding lands.

---

## 4. Repository layout

```
overlay/          Gentoo ebuild overlay (::hadalos)
catalyst/         stage + livecd specs
dbus/             D-Bus interface definitions and bus policy
policy/           polkit action definitions
systemd/          unit files
src/hadal-brokerd Rust — D-Bus service, capability broker
src/hadalwm      Rust — X11 window manager
src/hadalos-shell  C#/Avalonia — panel, launcher, settings, Hadal overlay
src/hadalos-greeter greetd greeter
scripts/          build-host bootstrap and release tooling
```

---

## 5. Workflow

Authoring happens on the Windows dev laptop. Execution happens on the 9800X3D
build host. Git is the transport — the same loop `hadal sync` already runs.

Nothing in this repo may assume it can reach the build host directly.
