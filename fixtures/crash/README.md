# Crash fixtures

Real `system_app_crash` DropBox entries pulled from the target device
(Pixel 6a, CalyxOS 7.2.2.0, Android 16) on 2026-08-07. Deduplicated by
process + exception; the device had three entries, of which two are distinct.

These are the evaluation corpus for the flagship feature. Before committing to
a resident reflex model, the question that has to be answered is *"can a 1–3B
model, given this text and nothing else, produce the explanation a person
actually needs?"* — and that question needs real inputs, not synthetic ones.

They contain no personal data: process names, uids, and Java stack traces from
platform components. `settings.txt` is uid 1000, `statementservice.txt` is
uid 10160.

## What each one is, and what a good answer looks like

### `settings.txt` — the one that matters

```
IllegalStateException: Gatekeeper Password is missing!!
  at BiometricUtils.requestGatekeeperHat
  ← BiometricEnrollIntroduction.onActivityResult
  ← FingerprintEnrollIntroduction.onChallengeGenerated
```

Fingerprint enrolment reached the biometric flow without a Gatekeeper HAT —
the auth token minted when the user confirms their device credential. Occurred
twice, 23s apart: a retry that failed the same way.

A good answer names the user-visible consequence (*your fingerprint did not
enrol*) and the recovery path (*set a screen lock, then enrol from
Security → Device unlock*). A bad answer explains what a HAT is.

This is the case that justifies the whole feature. The user's actual experience
was "Settings closed unexpectedly" — twice — with no indication of why or what
to do. Everything needed to say something useful was sitting in DropBox,
unread.

### `statementservice.txt` — the control case

```
IllegalStateException: WorkManager is not initialized properly ...
  at DomainVerificationReceiverV2.scheduleUnlockedV2
```

An AOSP bug: the manifest disables `WorkManagerInitializer` and the
Application does not implement `Configuration.Provider`. Fires once at boot
(38s uptime, dead 1.3s in). Breaks App Links domain verification, so `https://`
links open the chooser instead of the owning app.

This is the control, and arguably the harder test. **The correct answer is
"ignore this."** A model that invents a remedy for a platform bug the user
cannot fix is worse than one that says nothing, and an assistant that cries
wolf about every boot-time stack trace gets turned off inside a week.

## Note on capability surface

Both are diagnosable with `read-crash-report` alone — Tier `Read`, no
confirmation prompt. Neither remediation is something the broker does; one is a
human action and the other is nothing. The flagship read is fully useful with
*zero* Mutate capabilities granted, which is worth remembering when the
temptation arrives to widen the enum.

---

## First real result (2026-08-07)

Run through `hadald` against NVIDIA Build, via `src/hadald/tests/eval-crash.sh`.

| Prompt | Model | `settings` (actionable) | `statementservice` (control — correct answer is "ignore") |
|---|---|---|---|
| blessed declining | llama-3.3-70b | ✗ "nothing to do" | ✓ "nothing to do" |
| neutral | llama-3.3-70b | ~ explains, then generic advice | ✗ suggests restart / updates / contact support |
| neutral | nemotron-super-49b | **✓ correct and specific** | ✗ suggests restart / updates / support |

Three findings, none of them the one that was expected.

**Prompt design dominated model choice.** Telling the model that declining was
an acceptable and complete answer made the 70B decline *both* cases — including
the one where the user's fingerprint had genuinely failed to enrol. That is the
worst possible outcome: an assistant that always says "ignore it" is not
cautious, it is useless, and it fails silently in the same way everything else
in this project has.

**The 49B beat the 70B on the case that mattered.** Given the neutral prompt,
`nemotron-super-49b` produced: *"crashed while trying to set up biometric
authentication … because it couldn't find a password configured on the device
… set up a password, then try biometric enrollment again."* That is the answer
a person needs, from a model small enough to be interesting. Size is not the
variable.

**Nobody passed the control.** Both models, given a neutral prompt, told the
user to restart, check for updates, or contact support about an AOSP bug they
cannot fix and that has no user-visible consequence worth acting on. This is
the cry-wolf failure the fixture was written to detect, and it is unsolved.

### What this changes

The open question was *"can a reflex model do the flagship job?"* The answer is
not yes or no — it is that **the job has two halves with opposite failure
modes, and prompt-tuning trades one against the other.** Encourage declining
and you lose real diagnoses; stay neutral and you get advice on unfixable
platform bugs.

That points away from prompt engineering and toward structure: an explicit
actionability decision the model must commit to, rather than prose it can hedge
in. It is the same shape as the broker's own thesis — the model proposes into a
closed enum instead of being trusted to phrase things safely.

A `read-crash-report` result could plausibly carry
`{verdict: "actionable" | "platform-defect" | "unknown"}` alongside the prose,
typed and validated like everything else, so "should I have bothered you with
this" becomes a checkable field rather than a tone.
