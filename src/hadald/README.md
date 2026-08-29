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
| **Privacy** — system data never leaves the machine | **broken**, and `--fallback` breaks it wider — see below |
| Availability | depends on a third party and a rate limit; `--fallback` is the mitigation |

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

## The context budget

`hadal explain` failed against NVIDIA for as long as it existed, with an HTTP
error that looked like a rate limit and was not. Diagnosed 2026-08-25:

```
400: This model's maximum context length is 131072 tokens. However, you
requested 2048 output tokens and your prompt contains at least 129025 input
tokens, for a total of at least 131073 tokens.
```

Over by **one token**. The cause was two unbounded inputs sharing one fixed
window:

| Input | Was | Now |
|---|---|---|
| Build-log read (`executor.rs`) | 256 KiB, a round number picked in bytes | 187 KiB, derived from a token budget |
| Retrieval passages (`retrieve.rs`) | **no limit at all** — up to 20 chunks × ~6 KB | 72 KB |

The trap is that **build-log text tokenises at roughly 2.5 bytes per token**,
against 4 for English prose — absolute paths, compiler flags, hex offsets and
base64 are the worst case a tokeniser meets. Measured on a real 707 KB
llama-cpp log: 262,177 bytes of tail became 103,194 prompt tokens. So the old
256 KiB read alone claimed ~79% of the window before the action protocol, the
retrieval passages and the reserved output were added, and the total landed
either side of the limit depending on which package had failed. That is why it
looked intermittent.

Both caps now derive from one stated budget in `executor.rs`, and the numbers
above are recorded there rather than here so they sit next to the constant they
justify. Verified against the live endpoint at worst case: **87,931 prompt
tokens, 42,137 spare.**

**A 400 that means "too long" now says so.** hadald's standing rule is never to
forward an upstream body, because it can echo the prompt. Context-length
rejections are the one carve-out — they are arithmetic, not content — so they
come back as `413` with the numbers extracted and the prose discarded. The old
bare `upstream returned 400 Bad Request` was mistaken for a rate limit and then
an expired key before anyone read the endpoint's actual reply.

> **This is per-model, and the chain does not negotiate it.** One budget is
> fixed before the prompt is built, while the chain's windows range from 256k
> at the primary to 8k at the local reflex model. It is deliberately sized to
> **the widest window a fallback can serve** — Groq's 131k — rather than to the
> primary's, so that a large prompt is answerable by more than one link. The
> reasoning is recorded next to `CONTEXT_TOKENS` in `executor.rs`.
>
> A link narrower than the budget is not a misconfiguration: it returns a
> context-length `400` and the chain **moves past it** to something wider. Only
> if nothing remaining is wide enough does the request fail, as `413` with the
> arithmetic.

**Expect it to be slow.** A ~88k-token prompt against Nemotron Super 49B
measured 44s to first byte and 220s total, and the Ultra 550B that replaced it
is a larger model still. Much of that wait is reasoning: these models emit
roughly half their frames as `reasoning_content`, which hadald once dropped
entirely — producing a long silence that looked exactly like a hang. Those
frames are now surfaced as a distinct `thinking` signal, so the wait is legible
rather than blank. See **Reasoning models** below.

---

## The fallback chain

Free inference tiers cap by the day or by the minute, and the cap is *per
provider*. `--fallback` takes the obvious consequence: give hadald more than
one endpoint and let it walk them until one accepts.

```bash
hadald --serve \
       --model      nvidia/nemotron-3-ultra-550b-a55b \
       --upstream   https://integrate.api.nvidia.com/v1 \
       --key-file   /etc/hadal/upstream.key \
       --fallback   https://api.groq.com/openai/v1,llama-3.3-70b-versatile,/etc/hadal/groq.key \
       --fallback   https://api.cerebras.ai/v1,qwen-3-235b-a22b-instruct-2507,/etc/hadal/cerebras.key \
       --fallback   http://127.0.0.1:8080/v1,hadal-reflex \
       --egress-log /var/log/hadal/egress.log
```

Each link carries **its own URL, model id and key**, because none of the three
transfers between providers: `nvidia/nemotron-3-ultra-550b-a55b` is a
404 at Groq, and one key is a 401 everywhere but the endpoint that issued it. A
chain built on shared fields is a single point of failure with extra latency.

Omitting `--fallback` entirely leaves a one-link chain that behaves exactly as
hadald did before it existed.

**Order the chain by context window, not by model size.** Free tiers cap
context as well as throughput, and the caps differ: Groq serves
`llama-3.3-70b-versatile` at the full 131k, while Cerebras' free tier holds
its models to 64k. `hadal explain` on a large Portage log routinely builds an
88k-token prompt, so a 64k link placed second would be asked — and would
refuse — on exactly the requests the chain exists to rescue. The larger model
goes *behind* the narrower window, where it still serves every prompt that
fits it.

### Where it stops

**Failover ends the moment a link returns a successful status.** From there
hadald is committed: the body streams straight through, and restarting on
another link would splice two models' output into one stream. The broker's
`ProposalScanner` is a single pass over a single stream, so a concatenation of
two prefixes can form a *valid* proposal that neither model actually made —
and a spliced proposal is well-typed, so `action.rs` would pass it. An upstream
that returns 200 and then dies four tokens in is therefore a **truncated
answer, not a retry**.

**A `400` does not fail over** — with one exception. The request is hadald's
own construction, so every link will reject it identically; walking the chain
would turn one fast error into five slow ones and hand the prompt to four more
third parties for nothing.

The exception is a **context-length** `400`, which does advance the chain,
because a context window describes the *link* and not the request. That is the
whole reason the ordering rule above exists: a prompt Cerebras turns away at
64k may sit comfortably inside the 131k link behind it, and letting the
narrowest window in the chain end the walk would let it decide the largest log
the entire chain can explain. If every remaining link also refuses on length,
the arithmetic is what comes back — `413` with the numbers — rather than the
generic `502` that once sent a real diagnosis chasing rate limits and expired
keys for a day while the endpoint had been saying "your prompt contains at
least 129025 input tokens" the whole time.

`401`, `403`, `404`, `408`, `409`, `413`, `429` and `5xx` do fail
over — but the first three are logged as *standing misconfigurations*, because
a dead key does not heal the way a rate limit does, and a chain quietly running
one link short is a chain whose redundancy you discover missing at the worst
moment.

**Retrieval never fails over.** `--fallback` is a chat mechanism only.
`/api/embed` and `/api/retrieve` are pinned to the primary, because vectors are
not comparable across models: a query embedded by a fallback lands in a
different space, `search` ranks it against the index anyway, and retrieval
degrades to approximately random **with no error anywhere**. Chat degrades
loudly; embeddings cannot, so they do not get the chance.

### What it costs

A three-link chain is three companies that may see a Portage build log, not
one. The egress log therefore records **one line per attempt**, not one per
request — a prompt that was rate-limited at the first link still left this
machine:

```
1756100000 model=nvidia/llama-3.3-… upstream=https://integrate.api.nvidia.com/v1 attempt=1/3 prompt_bytes=4211 …
1756100001 model=qwen-3-235b-a22b   upstream=https://api.cerebras.ai/v1         attempt=2/3 prompt_bytes=4211 …
```

Local links are still omitted, because nothing left the machine. Every remote
link is named at startup for the same reason: the chain's failure mode is that
it *works*, and nothing about a good answer says which of five providers
produced it.

### Setting one up

The keys have to be created by hand — every provider requires accepting terms
under an account, which is not something a setup script can or should do on
someone's behalf.

1. **Groq** — <https://console.groq.com/keys>. Sign in, *Create API Key*.
   Serves `llama-3.3-70b-versatile` at the full 131k context, which is why it
   goes second.
2. **Cerebras** — <https://cloud.cerebras.ai/>. Sign in, *API Keys*. Note the
   free tier caps context at 64k regardless of the model's own limit, and that
   Cerebras has been shifting between a standing free tier and a credit-based
   trial — check which one the account actually has before relying on it as a
   link rather than a bonus.

Install each with mode `0600` and `hadal:hadal` ownership, matching
`upstream.key` — hadald refuses a key file it can read but that others can
too:

```bash
printf '%s\n' 'gsk_…' > /tmp/k && \
  sudo install -m 0600 -o hadal -g hadal /tmp/k /etc/hadal/groq.key && rm /tmp/k
```

Then add the links to `/etc/hadal/hadald.env` on one line, and restart:

```
HADAL_FALLBACKS=--fallback https://api.groq.com/openai/v1,llama-3.3-70b-versatile,/etc/hadal/groq.key --fallback https://api.cerebras.ai/v1,qwen-3-235b-a22b-instruct-2507,/etc/hadal/cerebras.key --fallback http://127.0.0.1:8080/v1,hadal-reflex
```

The startup banner lists the whole chain; a link that is missing from it was
rejected at parse time and the daemon is running shorter than it looks.

**The last link is `hadal-reflex.service`** — `llama-server` holding a small
model on this machine, for when there is no network at all. See that unit for
why it is not `WantedBy` hadald, and read the `PrivateNetwork` note under
**Confinement** before assuming it is reachable: it is the one link that can be
configured correctly and still never answer.

---

## Reasoning models

Nemotron spends roughly half its output frames thinking before it says
anything. Those frames arrive as `reasoning_content` (or `reasoning`, depending
on the provider) rather than `content`, and hadald used to drop them. The
visible symptom was a 44-second silence with no output and no error, which
looks exactly like a hang — and reasoning models are precisely the ones worth
falling back *to*, so the silence got longer as the chain got better.

hadald now translates those frames into a distinct `thinking` field on the
Ollama NDJSON it emits:

```json
{"response": "", "thinking": "The linker error names -lssl…", "done": false}
```

`response` and `thinking` are never populated in the same frame, and the
separation is carried by the type system the whole way down — `Delta::Reasoning`
in hadald, `Event::Thinking` in the broker, a `Thinking` signal on the system
bus. The broker routes it **around `ProposalScanner`, never through it**. That
is the point of the separation rather than a detail of it: a reasoning trace is
a model talking to itself, so it quotes protocol syntax and drafts action
blocks it then rejects. Scanned, a rejected draft becomes a well-formed
proposal that `action.rs` accepts and polkit prompts for — an action the model
decided *not* to take. `hadal-brokerd`'s
`a_fenced_action_inside_reasoning_never_becomes_a_proposal` pins this.

By default `hadal` shows a dim live counter (`thinking… 2225 chars`) that is
overwritten the moment real output starts, and notes the trace's size at the
end if it was substantial. Set `HADAL_SHOW_THINKING=1` to print the trace
itself, which is the fastest way to tell a model that is stuck from one that is
merely slow.

---

## Confinement

`systemd/hadald.service` already anticipated this and prescribed the answer:

> *"Running a deep model on another machine is incompatible with this as
> written. The intended path is NOT to drop `PrivateNetwork`, but to add a
> `systemd-socket-proxyd` unit in the host namespace pinned to exactly one
> upstream address — so egress stays limited to one host by unit configuration
> rather than by trusting the daemon. Until that exists, a LAN deep host
> requires a documented, deliberate drop-in."*

**`IPAddressDeny=any` blocks loopback, including the broker.** This is the trap
in the section below and it is worth stating before the fix rather than after:
`any` expands to `0.0.0.0/0 ::/0`, which contains `127.0.0.0/8`. There is no
implicit exemption for loopback — systemd provides `localhost` as a *separate*
named set precisely because it is not covered otherwise.

So the unit as shipped does not merely stop hadald reaching NVIDIA. It stops
`hadal-brokerd` reaching **hadald**, which it does over `127.0.0.1:11434`
inside their shared namespace. The observed symptom is `hadal status` reporting
the model unreachable and the broker logging:

```
ERROR generation failed: hadald is not reachable:
      error sending request for url (http://127.0.0.1:11434/api/generate)
```

three seconds *after* hadald logged that it was listening. Nothing in that
message points at an address filter, and both services report `active`.

A second thing hides in the same place: under `PrivateNetwork=yes` hadald's
`127.0.0.1:11434` is not the host's. On the reference machine an Ollama install
holds the host's 11434, so `curl` from a shell reaches Ollama and gets a
plausible-looking answer from a daemon that is not hadald. Two processes bind
"the same" port with no conflict because they are in different namespaces.
Check with `ss -ltnp` in the right namespace before concluding hadald is up.

**Interim — the documented, deliberate drop-in.** `systemctl edit hadald`:

```ini
[Service]
# Deliberate: hadald now reaches a remote inference endpoint. See
# src/hadald/README.md for what this costs.
PrivateNetwork=no

# Both lines are reset to empty on purpose, and the empty assignment is the
# load-bearing part: these are *cumulative* directives, so without clearing it
# first the `IPAddressDeny=any` in the unit stays in force and the drop-in
# grants nothing.
#
# An earlier revision of this block read `IPAddressDeny=any` /
# `IPAddressAllow=localhost` and was wrong in a way that is worth keeping a
# note about, because it looked careful: it permits loopback and nothing else,
# so it blocks every endpoint the drop-in exists to reach. A "no egress" rule
# and a "reach this remote endpoint" requirement cannot both be satisfied by an
# allow-list of localhost.
#
# It was, however, strictly better than the unit's bare `IPAddressDeny=any`,
# which permits nothing at all — not even the broker's hop to hadald. If you
# are running a local-only chain, `IPAddressDeny=any` with
# `IPAddressAllow=localhost` is the correct setting and this drop-in is not
# needed. Clearing both, below, is what a remote link costs.
IPAddressDeny=
IPAddressAllow=

# Pinning the providers' addresses instead is tempting and fragile — they sit
# behind CDNs and the addresses rotate. That fragility is the argument for the
# proxyd below; until it exists, what bounds egress is the egress log, which
# records every attempt that left the machine.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
LoadCredential=upstream.key:/etc/hadal/upstream.key
```

**`PrivateNetwork` also decides whether the local tier exists at all.** Under
`PrivateNetwork=yes` the daemon gets its own loopback, so
`--fallback http://127.0.0.1:8080/v1,…` addresses *that namespace's* port 8080
and not the host's. A `llama-server` running on the host is invisible to it,
and the failure is quiet in the worst way: `Locality::Local` correctly suppresses
the key and the egress-log line, so the link looks configured and simply never
answers. Either the drop-in above puts hadald in the host namespace, or the
`llama-server` unit needs `JoinsNamespaceOf=hadald.service` to be reachable —
the same mechanism `hadal-brokerd` already uses to find it.

**Target — `systemd-socket-proxyd`.** `hadald` keeps `PrivateNetwork=yes`; a
socket in its namespace is served by a proxy running in the host namespace, so
the reachable set is one upstream by unit configuration rather than by trusting
the daemon. `systemd-socket-proxyd` is present and `JoinsNamespaceOf=` is
supported, so this is buildable — **but it is not yet written or tested, and
should be labelled that way until it has run.** It also needs hadald to speak
TLS over the proxied connection with the upstream's SNI, which the current
`reqwest` setup does not do.

**What `--fallback` does to this plan.** "Pinned to exactly one upstream" was
written when there was exactly one. A chain has *N* remote destinations, so the
proxy grows to one socket unit per remote link, and hadald's links become
loopback addresses in its own namespace that each map to one external host.

That is more units, but it is the same property and arguably a better
demonstration of it: the reachable set stays enumerable in the unit files, and
adding a provider becomes a deliberate act of writing one down rather than an
edit to a command line. Until it exists, hadald logs the count at startup when
more than one remote link is configured, so the gap is visible rather than
assumed. The interim drop-in above already allows general egress and so is
unaffected — which is exactly why it is interim.

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
cargo test              # 42 unit tests — config, chains, key handling, SSE decoding
bash tests/e2e.sh       # 10 end-to-end against a fake upstream; no key, no network
bash tests/chain.sh     # 12 end-to-end against two misbehaving upstreams
```

> `tests/e2e.sh` currently reports **7/10**. The three failures predate the
> fallback work: the test points hadald at `127.0.0.1` and then asserts a
> `Bearer` header and an egress line, both of which `Locality` deliberately
> suppresses for a loopback upstream. The assertions are stale, not the daemon
> — but they are left failing rather than deleted, because the fix is to give
> the test a remote-classified upstream and that has not been written.

The end-to-end test streams the reply **one character per SSE frame** — the
worst case a tokeniser can produce — and asserts the ` ```hadal-action ` fence
survives reassembly byte-identically. That is the property the broker depends
on and the one most likely to break silently under chunking.

It also asserts the prompt body is *not* written to the egress log unless
`--log-bodies` is passed, because a privacy control that quietly logs
everything is worse than none.

`tests/chain.sh` covers the two failover questions that are really *negative*
questions — that a `400` does not reach the rest of the chain, and that a
stream dying after a `200` does not restart elsewhere. Both are cases where the
convenient behaviour is the wrong one, so they are asserted on the number of
upstreams that saw the prompt rather than on the reply.
