# hadald

The Hadal model host. **Ollama-shaped inward, OpenAI-shaped outward.**

`hadal-brokerd` already speaks two Ollama endpoints over plaintext loopback —
`GET /api/tags` and `POST /api/generate` — and expects newline-delimited JSON
back. `hadald` serves exactly those and translates to an OpenAI-compatible
chat-completions endpoint, streaming the SSE reply back as NDJSON.

**The broker needs no changes.** `model.rs`, `ACTION_PROTOCOL`, the
` ```hadal-action ` fence and `ProposalScanner` all keep working untouched, and
swapping a remote 70B for a local GGUF later is a change to this crate alone.

```
hadal-brokerd ──plaintext loopback──▶ hadald ──TLS──▶ integrate.api.nvidia.com
   privileged                       unprivileged
   no TLS stack                     holds TLS + the key
   validates every proposal         has no privileges at all
```

That asymmetry is deliberate and inherited: the broker's `Cargo.toml` drops
TLS on purpose, because *"linking a TLS stack into the privileged component
would add attack surface it has no use for."* Backing the model remotely
doesn't change that — the TLS lands in the component that already had no
privileges.

---

## What this does to the "local" claim

HadalOS's README says the model daemon *"runs in a network namespace with no
route out"* and that local is *"a kernel guarantee here, not a marketing
claim."* **Backed by a remote endpoint, that sentence is false.** Say so;
pretending otherwise is worse than the change.

Precisely what changes, and what does not:

| Property | Status |
|---|---|
| **Safety** — no path from model output to a command interpreter | **intact**, and by design rather than luck |
| **Privacy** — system data never leaves the machine | **broken** |
| Availability | now depends on a third party and a rate limit |

The safety property survives because the broker was built not to trust the
model. Proposals are still typed, still validated by `action.rs`, still gated
by polkit. Where the model runs was never load-bearing for that.

The privacy property does not survive, and the payloads are the sharp end:
Portage build logs and journal excerpts carry hostnames, usernames, absolute
paths, and occasionally tokens. `--egress-log` exists so *"what left this
machine"* has an answer that is not a promise.

The free tier is licensed for **development, testing, research or
evaluation** — not production. For a project at "nothing boots yet" that is
fine, but it is a ceiling.

---

## Confinement

`systemd/hadald.service` already anticipated this and prescribed the answer:

> *"Running a deep model on another machine is incompatible with this as
> written. The intended path is NOT to drop `PrivateNetwork`, but to add a
> `systemd-socket-proxyd` unit in the host namespace pinned to exactly one
> upstream address — so egress stays limited to one host by unit configuration
> rather than by trusting the daemon. Until that exists, a LAN deep host
> requires a documented, deliberate drop-in."*

**Interim — the documented, deliberate drop-in.** `systemctl edit hadald`:

```ini
[Service]
# Deliberate: hadald now reaches a remote inference endpoint. See
# src/hadald/README.md for what this costs.
PrivateNetwork=no
IPAddressDeny=any
IPAddressAllow=localhost
# Pinning IPs here is tempting and fragile — the endpoint is behind a CDN and
# the addresses rotate. That fragility is the argument for the proxyd below.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
LoadCredential=upstream.key:/etc/hadal/upstream.key
```

**Target — `systemd-socket-proxyd`.** `hadald` keeps `PrivateNetwork=yes`; a
socket in its namespace is served by a proxy running in the host namespace, so
the reachable set is one upstream by unit configuration rather than by trusting
the daemon. `systemd-socket-proxyd` is present and `JoinsNamespaceOf=` is
supported, so this is buildable — **but it is not yet written or tested, and
should be labelled that way until it has run.** It also needs hadald to speak
TLS over the proxied connection with the upstream's SNI, which the current
`reqwest` setup does not do.

---

## Running it

```bash
install -d -m 0700 /etc/hadal
printf '%s' "$NVIDIA_KEY" > /etc/hadal/upstream.key
chmod 600 /etc/hadal/upstream.key      # hadald refuses anything looser

hadald --serve \
       --model <model-id> \
       --key-file /etc/hadal/upstream.key \
       --egress-log /var/log/hadal/egress.log
```

The key is read from a **file, never an environment variable** — a process
environment is readable through `/proc/<pid>/environ`, is inherited by
children, and turns up in crash dumps and `systemctl show`. A file has an owner
and a mode. systemd's `LoadCredential=` can supply it without it existing on
any filesystem hadald can see.

`--listen` refuses anything that is not loopback: the daemon speaks plaintext
and holds an API key, and binding it elsewhere would publish both.

---

## Tests

```bash
cargo test              # 15 unit tests — config, key handling, SSE decoding
bash tests/e2e.sh       # 10 end-to-end against a fake upstream; no key, no network
```

The end-to-end test streams the reply **one character per SSE frame** — the
worst case a tokeniser can produce — and asserts the ` ```hadal-action ` fence
survives reassembly byte-identically. That is the property the broker depends
on and the one most likely to break silently under chunking.

It also asserts the prompt body is *not* written to the egress log unless
`--log-bodies` is passed, because a privacy control that quietly logs
everything is worse than none.
