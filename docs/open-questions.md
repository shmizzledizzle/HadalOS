# Open questions

Decisions that have *not* been made. Anything settled belongs in
ARCHITECTURE.md instead.

## Blocking phase 1

- **Which NDK?** Gentoo has no `android-ndk` in `::gentoo`. Options are the
  upstream tarball unpacked to `/opt`, or building against the platform
  toolchain later and skipping standalone cross-compilation entirely.
  Standalone is much faster to iterate on, so probably the tarball.
- **`aarch64-linux-android` std.** Gentoo's `dev-lang/rust` lists the target
  but almost certainly ships no std for it, and there is no `rustup` on this
  box. Either add `rust-bin` with rustup, or build std with `-Z build-std`.

## Blocking phase 2

- **Does `hadald` need to be a native daemon at all?** An alternative is a
  bound `isolatedProcess="true"` Service, which gets uid isolation and no
  network for free from the framework rather than from hand-written sepolicy.
  Cheaper, but gives up cgroup-level memory capping and socket-activation
  equivalents. Undecided.
- **Where does the confirmation Activity live?** SystemUI is the obvious home
  but couples us to a component CalyxOS also patches. A standalone
  priv-app is more merge-friendly and less privileged. Leaning priv-app.
- **Model runtime.** llama.cpp is assumed in ARCHITECTURE.md §2.2 but not
  benchmarked on a Tensor G1. If the reflex model cannot hold a conversational
  latency budget on the 6a's TPU/CPU, the whole resident-model premise needs
  revisiting.
- ~~**Can a reflex model actually do the flagship job?**~~ Tested 2026-08-07
  against NVIDIA Build — see `fixtures/crash/README.md`. The answer is neither
  yes nor no: the job has **two halves with opposite failure modes**. A 49B
  nailed the actionable diagnosis; no model passed the control case; and
  blessing "you may decline" in the prompt made a 70B decline everything,
  including the real fault. Model size was not the variable — prompt framing
  was, and it trades one failure against the other.
- **How should actionability be represented?** Follows directly from the above.
  Prose lets a model hedge; the project's own thesis says the answer is a typed
  field. A `read-crash-report` result carrying
  `verdict: actionable | platform-defect | unknown` would make "should I have
  bothered you with this" checkable rather than a matter of tone. Undecided
  whether the model emits that verdict or a second pass classifies it.

- **Closing the loop.** `Execute`'s result reaches the user and never the
  model, so `hadal explain` reads a log aloud rather than diagnosing it. See
  `docs/open-loop.md`. The mechanism already exists — `build_prompt`'s
  "data, not instructions" context channel — and the open decision is whether
  the CLI or the session drives the follow-up, plus what bounds it.

## Blocking phase 3

- **AVB re-locking with a custom key.** Real on Pixels, but untested by us, and
  getting it wrong on a device with no other OS on it is a bad afternoon. Now
  the *only* remaining phase 3 unknown, and the only irreversible step in the
  plan.

## Unscoped

- Does any of this want a launcher/shell surface, or is Hadal purely a
  background system service with a Settings page and a notification? The
  desktop has a whole DE; the mobile version may reasonably have no UI of its
  own beyond the confirmation dialog.
- Whether `read-network-activity` should read from `NetworkStatsManager`
  (framework, aggregated, easy) or eBPF traffic-controller maps directly
  (precise, per-uid, harder, and the same source Datura uses). Leaning
  framework: `cmd netstats` reports *"No shell command implementation"* on the
  device, so there is no CLI shortcut to lean on either way, and
  `dumpsys netstats` already exposes 31 per-uid records — the aggregated data
  is real and present.

---

## Resolved

- ~~**Which NDK?**~~ r27d, unpacked to `~/Android/android-ndk-r27d`. Latest LTS
  patch; ::gentoo has no `android-ndk` and there is no need for one, since the
  tarball needs no root.
- ~~**`aarch64-linux-android` std.**~~ Confirmed absent from Gentoo's
  `dev-lang/rust` (only `x86_64-unknown-linux-gnu` is installed). Resolved with
  rustup in `~/.cargo` / `~/.rustup`, installed `--no-modify-path` so the system
  cargo still wins in `PATH`. `scripts/build-android.sh` puts rustup's cargo
  first explicitly.
