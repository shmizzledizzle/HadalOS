# ifixit — multi-step fixes without giving the model a shell

## The requirement

Hadal should be able to *fix* a diagnosed problem, not only explain it. Gated
on: local model only, the root password, and a comprehension question the user
must answer correctly.

## Why the comprehension check forces the design

A comprehension question is a safeguard only if it is **true of the thing that
will run**. If the model authors both the script and the question, a script
that does X with a question describing Y passes trivially, and the check is
ceremony. So the question must be derived by the **broker**, from the content,
with no model involvement.

That is not a new rule. It is the one the CLI already states:

> `summary` came from the broker's `Action::summary()`, derived from the parsed
> action — not from anything the model wrote. What is displayed is what will
> run.

Now apply it to a shell script. To ask *"how many files does this delete?"* the
broker must count deletions. `rm "$TARGET"` cannot be counted without executing
the script, and in general the question is undecidable. Every honest answer
here is one of:

1. Ask a question the broker cannot verify — theatre.
2. Ask only about surface syntax (*"how many lines mention rm?"*) — answerable
   without understanding, so it checks nothing.
3. Constrain the script to something analysable.

Option 3 is the only one where the check does work. And the natural constraint
is already built: **a plan is a sequence of typed `Action`s, not a script.**

## What that buys

Every step is already validated by `action.rs`, so a plan cannot contain
anything a single proposal could not. Counting is exact rather than
approximate, because the broker is counting parsed enum variants, not parsing
text. The comprehension question is therefore *provably* true — it is generated
from the same structure the executor will run.

And the property §2.1 calls the most important in the project survives intact:

> There is no code path from model output to a command interpreter.

A plan adds no interpreter. It adds a `Vec<Action>` where there was one
`Action`.

```rust
pub struct Plan {
    pub steps: Vec<Action>,
    /// The model's prose. Shown, never trusted, never the basis of the check.
    pub rationale: String,
}
```

## What it cannot do, stated plainly

A plan cannot do anything the action enum cannot. It cannot `sed` a file, write
arbitrary config, or run a command someone thought of yesterday. Widening what
Hadal can fix means adding a variant to the enum — deliberately, with a
validator, a polkit action and a test — which is the cost the design intends.

If arbitrary shell is genuinely wanted later, it should be understood as
abandoning the thesis rather than extending it, and the comprehension check
cannot come along: nothing can generate a true question about arbitrary code.

## The gate

Three independent things, and the order matters — cheapest and most reversible
first:

1. **Local model only.** A plan proposed by a remote model is refused outright,
   not downgraded. See `docs/tier-routing.md` §4: when the safe path is
   unavailable the answer is to decline, never to substitute.
2. **The comprehension question**, derived from the parsed plan.
3. **polkit `auth_admin`**, which is the root password, on the highest tier any
   step requires.

Asking the question *before* the password is deliberate. The password
authenticates a human; the question is the only part that establishes the human
knows what they are approving. Putting it last, after the credential, invites
answering it the way one clicks through an EULA.

## Generating the question

Derived from counts of parsed variants, so it cannot describe anything other
than what will run:

```
This plan will:
  restart 1 service          (hadald.service)
  change 2 configuration settings
  install 0 packages
  delete 0 files

How many services will it restart?
```

Properties worth keeping:

- **Numeric, from a count.** Free text invites near-misses; a number is either
  right or wrong.
- **The category asked about varies**, so the answer is not always the largest
  or first number, and cannot be guessed from position.
- **A plan with one trivial step still asks.** A gate that skips itself when it
  judges the change small is a gate that a model can talk into judging.
- **Wrong answer discards the plan.** It does not re-prompt. Retrying until the
  number is right is the same as no check at all.

## Recording

Every executed plan appends to a log: timestamp, steps as parsed, the question
asked, the answer given, and each step's result. "What did Hadal do to this
machine" needs an answer that is not reconstruction from shell history.
