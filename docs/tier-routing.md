# Tier routing

Which Hadal answers, and who decides.

Hadal already has tiers. `hadal.config.example.json` carries `miniModel`/
`miniHost` and `deepModel`/`deepHost`, and `escalations/` adds a third —
`[ESCALATE->CLAUDE]` for work Hadal judges beyond its depth. What does not
exist is a **policy**: `hadal_mcp.py:resolve()` routes on a string the caller
passes, so the tier is chosen by whoever calls, not by what the request is.

Adding a remote inference endpoint makes that gap load-bearing.

---

## 1. The decision is about data, not capability

The instinct is to route on "how dangerous is this operation". That is the
wrong axis, and `src/hadald/README.md` says why: backing the model remotely
leaves the **safety** property intact and breaks the **privacy** property. A
remote model cannot do anything a local one cannot — proposals are typed,
validated by `action.rs` and gated by polkit either way. What changes is what
leaves the machine.

So the question is not *"could this action hurt me"* but *"must this data stay
here"*. Those are different sets. `emerge-apply` is the most destructive action
in the enum and its **input** is a list of package names, which is nothing.
`read-journal` cannot change a thing and its input is your hostname, your unit
names and your activity.

## 2. Derive it from the capability table, not a parallel heuristic

The broker already classifies every capability. Extend that table rather than
inventing a second notion of sensitivity beside it:

```rust
impl Capability {
    /// May the *result* of this capability be included in a prompt sent to a
    /// model that is not on this machine?
    pub fn result_may_leave_machine(self) -> bool {
        match self {
            // Names and versions. Public facts about public software.
            Capability::QueryPackage | Capability::EmergePretend => true,
            // Everything that reads this machine's state.
            Capability::ReadJournal
            | Capability::ReadPortageLog
            | Capability::ReadPath
            | Capability::UnitStatus => false,
            // Mutations carry their operands, which are package names — but
            // routing a mutation remotely means a third party shapes a change
            // to this system. Keep it local on principle.
            Capability::RestartUnit
            | Capability::EmergeApply
            | Capability::WriteConfig => false,
            // Egress is the one that already means "leaves the machine".
            Capability::NetworkLookup => true,
        }
    }
}
```

Two properties fall out for free. It is a closed set, so a new capability
cannot quietly default to "sendable" — the match must be extended. And it is
checkable: `Capability::ALL` can be asserted against the routing policy in a
test, exactly as `verify_actions_installed` already asserts the capability
table against the polkit policy.

**Retrieved documentation is not system state.** The Gentoo Handbook is public;
sending handbook chunks upstream leaks nothing. Only the captured-state part of
a prompt is subject to this rule. That distinction matters for §5.

## 3. The routing rule

```
route(request):
    if any capability result in this prompt has !result_may_leave_machine():
        tier = LOCAL          # mandatory, not preferred
    elif remote is reachable:
        tier = REMOTE
    else:
        tier = LOCAL          # fallback
```

**Classify before probing connectivity, never the reverse.** Reaching for the
network to decide whether the request may use the network is backwards, and
the probe itself is a signal — it tells an observer that a request happened
and roughly when.

## 4. The asymmetry that matters

Falling back **remote → local** is safe: it degrades capability.

Falling back **local → remote** is a privacy breach.

So when a request is classified local-only and the local model is *not*
available, the correct behaviour is to **refuse**, not to quietly use the
remote one. This is the same shape as `90-hadalos-limine.install` refusing to
delete the pinned kernel: when the safe action is unavailable, decline rather
than substitute.

Stated as an invariant, because it is the one worth testing:

> No prompt containing a result from a capability where
> `!result_may_leave_machine()` may ever reach a non-local host, under any
> failure, fallback, retry or degraded mode.

## 5. Make the tier visible

Every answer should say which tier produced it, and remote answers should say
so before the user reads them, not after.

`hadal_mcp.py` already appends a stats line —
`[hadal-mini | 812 prompt tok | … | 14.2 tok/s]`. That is the natural place,
and the model name already distinguishes the tiers. What is missing is the
*locality*: a reader cannot tell from `hadal` whether that ran on the LAN
desktop or in someone else's datacentre.

Today's tally across the boot layer was six bugs, five of them silent. A tier
system that silently picks the remote model is that failure mode again, in the
component whose whole justification is that you can see what it does.

## 6. What the local model needs to be worth choosing

Routing to local is only useful if local can answer. Measured 2026-08-07
against the crash fixtures, a 49B beat a 70B on the case that mattered, so
capability is not simply a function of size — but a 1–3B reflex model is a
different proposition again.

`rag/build_index.py` currently indexes Terraria mod sources. ARCHITECTURE.md
§2.5 already calls for re-pointing it at the Gentoo wiki, the handbook, the
ebuild tree and kernel `Documentation/`. For tier routing that is not a nice-to-
have:

- **Local needs it most.** A small model plus the handbook may well beat a
  large model without it on Gentoo-specific questions. That is the hypothesis,
  and it is testable with the same harness the crash fixtures use.
- **It is egress-safe.** Public documentation in the prompt is not system
  state, so the index can serve both tiers without affecting §3.

Order of work: re-point the index, measure local-with-RAG against
remote-without on real Portage failures, and only then decide what the reflex
model has to be.

---

## Open

- Where does the policy live? The broker knows the capability table; the MCP
  server knows the tiers. One of them has to learn the other's vocabulary, and
  the broker is the one with the security boundary.
- `UnitStatus` is marked `false` above on the grounds that unit names leak
  system shape. Arguable, and the first thing to revisit if local turns out to
  be too weak to be useful.
- Escalation is a third tier with a fourth trust model — it hands work to an
  agent with its own network access. `result_may_leave_machine()` should
  presumably gate that path too, and currently nothing does.
