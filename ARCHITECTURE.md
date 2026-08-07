# HadalOS Mobile — Architecture of Record

An Android ROM carrying Hadal as a first-class system component, targeting
Pixel 6a (`bluejay`).

This is the mobile counterpart to [HadalOS/ARCHITECTURE.md](HadalOS/ARCHITECTURE.md).
Read that one first. This document records only what is *different*, and the
first thing to record is that almost everything is.

---

## 0. Decisions

| Area | Decision | Date |
|---|---|---|
| Target device | Pixel 6a, `bluejay`, Tensor G1 (GS101), `arm64` | 2026-08-07 |
| Base | AOSP, **via CalyxOS's device tree** — not Google's | 2026-08-07 |
| Development target | Cuttlefish (`aosp_cf_x86_64_phone`) before hardware | 2026-08-07 |
| IPC | Binder + AIDL (`android.hadal.IHadalBroker`), not D-Bus | 2026-08-07 |
| Authorization | System-owned confirmation Activity + SELinux, not polkit | 2026-08-07 |
| Egress denial | SELinux `neverallow` at policy build time, not `PrivateNetwork=` | 2026-08-07 |
| Broker | Rust, `binder_rs` — carried over from desktop | 2026-08-07 |
| Model host | `hadald`, isolated uid, llama.cpp rather than Ollama | 2026-08-07 |
| Flagship surface | Crash/ANR triage + per-app network activity | 2026-08-07 |
| Shell UI | Kotlin/Compose in SystemUI. **Not** Avalonia | 2026-08-07 |

---

## 1. What ports, and what does not

The desktop project is a Gentoo distribution. Android is not a Linux
distribution in any sense that matters to this codebase — it shares a kernel
and essentially nothing above it. Stating the damage plainly:

| HadalOS | Android | Portable? |
|---|---|---|
| The capability-broker thesis | — | **yes, entirely** |
| Parse-is-validation newtypes | — | **yes, the pattern** |
| Tier model (Read/Inspect/Mutate/Egress) | — | **yes, 1:1** |
| Rust + `serde` | Rust + `serde` | **yes** |
| D-Bus `org.hadal.Broker1` | Binder AIDL | shape only |
| polkit `auth_admin` | confirmation Activity | shape only |
| systemd sandboxing directives | SELinux + init `.rc` | shape only |
| Gentoo, Portage, `emerge` | — | **no** |
| **Portage build-failure explanation** | — | **no — the flagship feature does not exist here** |
| systemd, journald, `systemctl` | init, logcat, `ctl.*` | no |
| XLibre, X11 | SurfaceFlinger | no |
| `hadalwm` (Rust/x11rb) | WindowManagerService | no |
| HadalOS Shell (C#/Avalonia) | SystemUI (Kotlin/Compose) | no |
| Limine, last-known-good pinning | Android A/B slots + AVB | no — see §3 |
| `catalyst` stage1→3, livecd | `repo` + `soong`/`ninja` | no |
| `amd64` | `arm64` | no |

So this is a reimplementation that shares a thesis, not a port. The honest
estimate is that `action.rs` and `capability.rs` are the only files with real
lineage, and even they keep the *structure* while replacing every *value*.

### 1.1 The flagship problem, and its answer

The desktop README is explicit that the feature justifying the whole project is
Portage build-failure explanation: *"dense, structured, high-volume logs are
exactly what a small local model handles well, and no distribution ships an
assistant that reads them."*

There is no Portage on Android. Taking that sentence seriously rather than
mourning it, the question is what on a phone is dense, structured,
high-volume, and unread. Two things are:

- **Crash and ANR reports.** `DropBoxManager` entries, tombstones, binder
  timeout traces. Structurally identical to a build log — long, formatted,
  diagnostic, and today shown to the user as "App keeps stopping."
- **Per-app network activity.** Which uid talked to what, how much, when.
  This is the surface CalyxOS already cares most about, and Datura already
  provides the enforcement half. Nobody provides the *explanation* half.

The second is the stronger claim. "Which apps phoned home overnight, and
should any of them stop?" is a question a resident 1–3B model can answer from
local data, that no shipping ROM answers, and whose remediation
(`SetAppNetworkPolicy`) is already a first-class CalyxOS concept. It preserves
the original argument exactly — including *"no distribution ships this"* —
while being native to the platform rather than transplanted onto it.

---

## 2. Hadal as an Android system component

### 2.1 The rule

Unchanged, and non-negotiable: **the model never gets a shell.** There is no
code path from model output to a command interpreter. Hadal emits typed
proposed actions from a closed enum; the broker validates, the user confirms,
the broker executes via a direct call.

If anything the rule binds harder here. The desktop executor can reach systemd
over D-Bus and never build an argv. Several Android surfaces have no stable
native binder API and are realistically driven through `cmd`/`pm` — which
*does* mean an argv, with `system` uid behind it. The newtypes in
`src/hadal-brokerd/src/action.rs` are load-bearing, not decorative.

### 2.2 Process split

- **`hadald`** — model host. Dedicated uid, own SELinux domain, **not** in
  `AID_INET` (gid 3003). llama.cpp with a GGUF reflex model rather than Ollama,
  because Ollama's server assumes a filesystem and network posture Android does
  not offer. Memory-capped via its cgroup in `task_profiles`, so the assistant
  can never starve the UI.

- **`hadal-brokerd`** — owns `android.hadal.IHadalBroker`. Runs as `system`,
  never `root`. Holds the capability table and policy; talks to `hadald` over a
  unix socket in its own domain. The only component with real privilege, and it
  contains no model code.

### 2.3 Egress denial is *stronger* here

The desktop achieves "local is a kernel guarantee" with
`PrivateNetwork=yes` — a runtime directive in a unit file, which an admin (or a
bad `systemctl edit`) can remove.

Android's equivalent is better. An SELinux `neverallow` rule denying the
`hadald` domain every socket-creating permission is checked by the policy
compiler **at build time**. A ROM whose policy grants `hadald` a socket does
not boot — it does not even build. That is a compile-time guarantee where the
desktop has a runtime one, and it should be called out as such rather than
treated as a lossy port:

```
neverallow hadald { self }:{ tcp_socket udp_socket rawip_socket } *;
neverallow hadald domain:{ tcp_socket udp_socket } *;
```

The `network-lookup` capability, when granted, is therefore *not* implemented
by relaxing this. It is implemented by the **broker** performing the lookup in
its own domain and handing back text. `hadald` never gets a socket, in any
configuration, ever.

### 2.4 Capability tiers

| Tier | Default | Capabilities |
|---|---|---|
| Read | allow | `read-logcat`, `read-crash-report`, `read-path`, `query-package`, `read-network-activity` |
| Inspect | allow | `service-status`, `permission-diff` |
| Mutate | confirm every time | `restart-service`, `revoke-permission`, `set-app-network-policy`, `write-setting` |
| Egress | deny by default | `network-lookup` |

Twelve capabilities against the desktop's ten. `read-path` keeps its prefix
allowlist, with Android roots and an unconditional denylist over `/data/data`,
`/data/user`, keystore, and `/storage` — app private storage is the most
sensitive thing on the device and no diagnostic need justifies reaching into
it.

### 2.5 What the capability system does not do

The desktop's §2.4 caveat has an Android analogue, and it is worse in one way
and better in another.

**Better:** there is no equivalent of "polkit authorizes root for everything."
The confirmation Activity is a real UI gate that applies to the `system` uid
too, because it is a user-interaction requirement rather than a
credential check.

**Worse:** that gate is a *window*, and windows can be covered. An app holding
`SYSTEM_ALERT_WINDOW` can draw over a confirmation dialog and harvest the tap —
classic tapjacking. The mitigation is mandatory, not optional:

> The confirmation Activity sets `HIDE_NON_SYSTEM_OVERLAY_WINDOWS` and
> `setFilterTouchesWhenObscured(true)`. A touch event arriving with
> `FLAG_WINDOW_IS_OBSCURED` is discarded, not counted as consent.

And the unavoidable one, stated plainly rather than discovered later:

> A compromised `system_server` defeats this design completely. So does an
> unlocked bootloader with a hostile `fastboot` operator. Neither is a threat
> any userspace design can address.

The threat actually being defended against is unchanged from the desktop: **a
prompt injection producing a bad proposal** — a crash log, a notification, or
a web page that talks the model into suggesting something destructive.

### 2.6 Deferred

Anything requiring a modified `system_server`, and the on-device deep model.
The 6a has 6 GB of RAM and a Tensor G1; a resident reflex model is realistic,
a 7B is not. `deepHost` over LAN — which Hadal's config already models — is the
answer, and it is exactly the `network-lookup` shape: the broker holds the
socket, the model never does.

---

## 3. The device problem

**Google removed Pixel device trees and driver binaries from AOSP as of
Android 16** (June 2025), moving the reference target to Cuttlefish, a virtual
device. That part stands and is the reason our base is CalyxOS's tree rather
than Google's.

**CalyxOS solved it for `bluejay`.** An earlier draft of this section quoted
their June 2025 statement — *"Without official source code, these devices are
currently unsupported for AOSP 16 builds"* — and treated Android 16 on the 6a
as an open risk. That statement is now over a year stale. Measured on the
target device, 2026-08-07:

```
ro.build.fingerprint   google/bluejay/bluejay:16/BP4A.251205.006/14401865:user/release-keys
ro.build.version.sdk   36            (Android 16)
ro.calyxos.version     7.2.2.0
ro.build.flavor        calyx_bluejay-user
security_patch         2026-06-01
kernel                 6.1.145-android14-11 (GKI)
```

So a maintained Android 16 `bluejay` tree exists and ships. Phase 3 is
materially less risky than first written: the device tree question is answered,
and the remaining work is rebasing onto their tree rather than reconstructing
one.

This has three consequences and they drive the whole plan:

1. **There is no "build AOSP for bluejay" anymore.** The device tree has to
   come from CalyxOS or LineageOS, who maintain it by reverse-engineering. Our
   base is therefore *CalyxOS's tree*, not Google's.
2. **Cuttlefish is the supported target**, needs no device tree and no vendor
   blobs, and runs locally. All broker, SELinux, and AIDL work belongs there.
3. **Boot resilience is already solved.** Android's A/B slots plus AVB give
   last-known-good behaviour natively, and better than Limine's two-entry
   layout. §3 of the desktop document has no work item here — this is the one
   place where the port is a straight deletion.

**The wipe — confirmed, not hypothetical.** Measured on the device:

```
ro.boot.flash.locked         1
ro.boot.verifiedbootstate    green
ro.boot.vbmeta.device_state  locked
```

The bootloader **is locked** and verified boot is green. Unlocking it
**erases the device**, with no way to take a backup across the operation.
Flashing a self-built ROM then means either running permanently unlocked —
which gives up the verified-boot guarantee this project otherwise leans on
heavily — or signing with a custom AVB key and re-locking. Pixels support
custom AVB keys, so the latter is real, but getting it wrong on a device with
no other OS on it is an afternoon lost.

This is the single irreversible step in the whole plan, which is why §5 puts it
last and why phases 1 and 2 are built to need none of it.

**SELinux is Enforcing**, as required for §2.3 to mean anything.

---

## 4. Build host reality

AOSP's stated requirements are **64 GB RAM minimum** and ~400 GB disk
(250 checkout + 150 build); Google quotes ~6 hours for a full build on a
6-core/64 GB machine.

Measured on the machine this repo currently sits on:

| | Available | Wanted | Verdict |
|---|---|---|---|
| RAM | 15 GiB + 4 GiB swap | 64 GB | **far under** |
| Disk | 459 GB free | 400 GB | workable, tight |
| CPU | i5-1235U (2P+8E, ULV laptop) | 6-core+ | slow, throttles |

A full platform build here is marginal — expect a long multi-hour build with a
real chance of OOM during linking, and swap or zram enlargement as a
prerequisite rather than an optimisation. The 9800X3D / 32 GB host named in the
desktop ARCHITECTURE.md is the better home for §5 phases 2 and 3.

Phase 1 needs none of this: it is a `cargo build --target aarch64-linux-android`
against the NDK, and it runs on the stock CalyxOS already installed.

---

## 5. Phasing

Ordered so that nothing is wiped until there is something worth flashing.

- **Phase 0 — toolchain.** `dev-util/android-tools`, a JDK, `repo`, `ccache`.
  `adb` talking to the device.
- **Phase 1 — broker as a plain aarch64 binary.** `hadal-brokerd` and a
  fake `hadald` cross-compiled and pushed to `/data/local/tmp`, driven over
  `adb shell`. Proves the action grammar, the validators, and the executor
  against real logcat and real DropBox entries. **No unlock, no wipe, no ROM.**
  The great majority of the interesting design work lives here.
- **Phase 2 — Cuttlefish.** AIDL surface, SELinux domains and `neverallow`
  rules, the confirmation Activity, `hadald` as a real init service. This is
  the actual ROM work, done where it is cheap and where AOSP still supports us.
- **Phase 3 — bluejay.** Rebase onto CalyxOS's device tree, build, sign, flash.
  The wipe happens here and only here.

---

## 6. Repository layout

```
src/hadal-brokerd/    Rust — action grammar, capabilities, executor
  src/action.rs         the closed action enum + validators
  src/capability.rs     tiers, confirmation contract, policy keys
aidl/                 android.hadal.IHadalBroker
sepolicy/             hadald + hadal_brokerd domains, neverallow rules
device/               CalyxOS tree overlay for bluejay
docs/                 open questions
HadalOS/              upstream desktop repo, reference only — not built here
```
