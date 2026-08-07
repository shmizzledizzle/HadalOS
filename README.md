# HadalOS Mobile

Hadal as an Android system component, targeting Pixel 6a (`bluejay`).

- **Base** — AOSP via CalyxOS's device tree, `arm64`
- **Assistant** — Hadal behind a Binder capability broker
- **Dev target** — Cuttlefish first, hardware last

Read [ARCHITECTURE.md](ARCHITECTURE.md) first. It is the decision record.

---

## The one idea

Unchanged from [HadalOS](HadalOS/README.md): the model never gets a shell. It
proposes typed actions from a closed enum; a separate privileged broker
containing no model code validates them, the user confirms, and the broker
executes via a direct call.

What changed is the platform, and it changed almost everything else with it —
see ARCHITECTURE.md §1 for the honest list. This is a reimplementation sharing
a thesis, not a port.

Two things came out *better* on Android:

- **Egress denial is compile-time.** An SELinux `neverallow` rule denying
  `hadald` any socket is checked by the policy compiler. A ROM that grants it
  one does not build. The desktop's `PrivateNetwork=yes` is a runtime directive
  someone can edit out.
- **Boot resilience is free.** A/B slots and AVB already do what Limine's
  two-entry last-known-good layout was built to do.

One thing came out worse: **the flagship feature does not exist here.** There
is no Portage on Android. Its replacement is crash/ANR triage and per-app
network activity — the same argument (dense, structured, high-volume, unread)
applied to data a phone actually has. ARCHITECTURE.md §1.1.

---

## Status

Foundation. Nothing boots. The phone has been read from, never written to.

| Component | State |
|---|---|
| Architecture / decision record | written |
| Action grammar + validators | 38/38 unit tests, 23/23 on device |
| Capability tiers + confirmation contract | covered by the above |
| `android.hadal.IHadalBroker` AIDL | not started |
| SELinux domains + `neverallow` rules | not started |
| Confirmation Activity | not started |
| `hadald` model host | not started |
| Cuttlefish integration | not started |
| bluejay device tree rebase | not started |

```bash
cd src/hadal-brokerd && cargo test
```

The action grammar has been cross-compiled to `aarch64-linux-android` and its
corpus run on the target device (Pixel 6a, CalyxOS 7.2.2.0, Android 16) —
23/23. Everything below that line is host-validated only. No bootloader has
been unlocked and nothing has been flashed.

```bash
./scripts/build-android.sh && ./scripts/probe-device.sh
```

---

## Layout

```
src/hadal-brokerd/    Rust — action grammar, capabilities, executor
aidl/                 android.hadal.IHadalBroker
sepolicy/             hadald + hadal_brokerd domains
device/               CalyxOS tree overlay for bluejay
HadalOS/              upstream desktop repo, reference only — not built here
```

`.gitattributes` pins LF on everything that executes on Linux, for the same
reason the desktop repo does. Do not relax it.
