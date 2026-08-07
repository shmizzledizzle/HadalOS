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

## Blocking phase 3

- **CalyxOS build reproducibility.** Their Android 16 status for `bluejay` was
  "currently unsupported" as of their June 2025 post. Needs re-checking against
  their current tree before phase 3 is scheduled at all — this may mean
  targeting their Android 15 branch instead.
- **AVB re-locking with a custom key.** Real on Pixels, but untested by us, and
  getting it wrong on a device with no other OS on it is a bad afternoon.

## Unscoped

- Does any of this want a launcher/shell surface, or is Hadal purely a
  background system service with a Settings page and a notification? The
  desktop has a whole DE; the mobile version may reasonably have no UI of its
  own beyond the confirmation dialog.
- Whether `read-network-activity` should read from `NetworkStatsManager`
  (framework, aggregated, easy) or eBPF traffic-controller maps directly
  (precise, per-uid, harder, and the same source Datura uses).
