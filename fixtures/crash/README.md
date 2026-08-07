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
