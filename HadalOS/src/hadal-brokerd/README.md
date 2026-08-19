# hadal-brokerd

The HadalOS capability broker. Owns `org.hadal.Broker1` on the system bus.

This is the component that makes Hadal part of the operating system instead of
an application that ships with it — and the component that makes that safe.

## The shape of it

```
client ──CreateSession──▶ Broker ──▶ Session
                                       │
                              Ask ─────┤──▶ hadald (no privilege, no network)
                                       │       │
                                       │    prose ──▶ Delta signal
                                       │    ```hadal-action block
                                       │       │
                                       │    parse + validate  ──failure──▶ dropped
                                       │       │
                                       │    ActionProposed + single-use token
                                       │
                           Execute(token) ──▶ polkit ──deny──▶ AuthFailed
                                       │        │
                                       │      allow
                                       │        ▼
                                       └──▶ Executor (D-Bus to systemd, or argv)
```

Generation and execution are different calls, made at different times, by
different parties. Nothing on the path from `Ask` to `ActionProposed` touches
the system.

## Why prompt injection does not escalate here

Hadal reads build logs, journals and files — all of which can contain text
aimed at the model. That text can cause it to *propose* something. It cannot
cause anything to *happen*, because:

- proposals are parsed into a closed enum; anything else is discarded
  (`model.rs::parse_block`, `action.rs`);
- the user sees `Action::summary()` — derived from the parsed action, not from
  the model's prose about it — before confirming;
- `Execute` needs a token the model never sees, and polkit still gets the
  final word;
- mutating capabilities are `auth_admin` every time, never `auth_admin_keep`.

The worst case is a suggestion you decline.

## Files

| File | Role |
|---|---|
| `action.rs` | The closed action enum and its validating newtypes. **The security boundary.** |
| `capability.rs` | Capabilities, tiers, polkit action ids |
| `policy.rs` | polkit `CheckAuthorization`; startup verification that the policy file is installed |
| `token.rs` | Single-use, expiring, session-bound proposal handles |
| `executor.rs` | The privileged side. D-Bus to systemd where possible, explicit argv otherwise, never a shell |
| `model.rs` | hadald client and the incremental prose/proposal scanner |
| `session.rs` | `org.hadal.Session1` |
| `broker.rs` | `org.hadal.Broker1` |

## Building

```bash
cargo build --release
```

Requires a Rust toolchain. **MSRV 1.87**, set by zbus 5.18 rather than by this
crate's own source — distribution rustc is often too old (Debian 13 ships 1.85
and fails resolution). No TLS stack: this daemon speaks plaintext HTTP to
loopback inside a namespace with no route out.

## Testing

```bash
cargo test
```

The tests worth reading first are the rejection cases in `action.rs` — every
one of them is a command injection if the newtype validators are wrong — and
the scanner tests in `model.rs`, which cover fences split across token
boundaries.

## Verifying from the Windows authoring machine

The daemon only runs on Linux, but all three checks work from the dev laptop.
`cargo check` does no linking, so the Linux target needs no cross-linker:

```bash
rustup target add x86_64-unknown-linux-gnu
```

```bash
cargo clippy --target x86_64-unknown-linux-gnu --all-targets
```

Tests build for the host and run natively, because the logic under test is
platform-independent by construction — see
`action::tests::absoluteness_is_posix_not_host_defined`, which exists
specifically to keep it that way:

```bash
cargo test
```

Then the cross-language check that Rust cannot do:

```bash
python ../../scripts/check-consistency.py
```

## Installing

```bash
install -Dm755 target/release/hadal-brokerd /usr/libexec/hadal-brokerd
install -Dm644 ../../policy/org.hadal.broker.policy /usr/share/polkit-1/actions/
install -Dm644 ../../dbus/org.hadal.Broker1.conf   /usr/share/dbus-1/system.d/
install -Dm644 ../../systemd/hadal-brokerd.service /usr/lib/systemd/system/
systemctl daemon-reload && systemctl enable --now hadal-brokerd
```

The polkit file is not optional — the daemon refuses to start without it.

## Known gaps

- `network-lookup` returns an error. The daemon shares hadald's network
  namespace by design; the intended answer is a `systemd-socket-proxyd` unit
  pinned to a single upstream address, which is not built yet.
- `emerge-apply` hands off to a transient systemd unit and returns
  immediately. Following progress means polling `unit-status`; there is no
  push progress channel yet.
- Nothing here has run on real hardware.
