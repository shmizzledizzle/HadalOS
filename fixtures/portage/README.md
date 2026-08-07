# Portage failure fixtures

## `gentoo-kernel-bin-6.18.41-config-failure.log`

A real `emerge --config` failure from this machine, 2026-08-07. 239 lines.

Harder than a clean failure log, and not on purpose: Portage appended the
successful retry to the same file, so the log contains **the failure at lines
57–67 and a success ending `[ ok ]` at line 238**. A correct answer has to find
the error in the middle without being reassured by the ending.

**Known correct answer.** `05-check-config.install` aborts unless
`/etc/kernel/install.conf` sets *all three* of `layout=`, `initrd_generator=`
and `uki_generator=`. The file is plain `key=value`; it has no section headers.

## Result: nemotron-super-49b via hadald, 2026-08-07

The first end-to-end run of the flagship feature after the loop was closed.

### Right

- **Root cause, exactly.** *"`initrd_generator` was not configured in
  `install.conf`, causing `/usr/lib/kernel/install.d/05-check-config.install`
  to fail."*
- **Not fooled by the retry.** It noticed the log covers two runs and said so —
  *"the config phase ultimately succeeded in the second run"* — while still
  attributing the failure correctly. This was the part I expected it to fail.
- **Grounded.** It quoted the actual line, `No initrd_generator= configured by
  install.conf`, rather than paraphrasing.

### Wrong

- **Config path invented.** `/etc/kernel-install.conf`. It is
  `/etc/kernel/install.conf`.
- **Format invented.** It suggested an INI `[options]` section header. The real
  file is bare `key=value` and has no sections.
- **Incomplete fix.** It set only `initrd_generator=`. `05-check-config` checks
  `uki_generator=` immediately after, so following this advice fails again on
  the very next run — the exact error this session actually hit.
- **Cry-wolf remediation.** It advised `emerge --ask sys-auth/systemd
  net-misc/rpcbind` for dracut warnings. **Neither package exists**
  (`sys-apps/systemd` is the real one, and already installed at 260.1-r2);
  `rpcbind` matters only for NFS root. Same failure as the crash fixtures:
  confident advice about something that needs no action.

### What it means

Diagnosis: strong. Specifics: invented. The model reasoned correctly from
evidence it was given and fabricated every detail it was not — paths, file
formats, package names. That is the same shape as the earlier
`/var/log/portage/build.log` guess.

**This is the concrete argument for the RAG work.** The failure mode is not
reasoning, so a bigger model will not fix it; it is missing reference material,
which is exactly what an index of the Gentoo handbook, `man kernel-install` and
the ebuild tree supplies. See `docs/tier-routing.md` §6 — and note it predicts
the local tier benefits most, which this result supports: correct reasoning
plus correct references beats more parameters.

A useful acceptance test for the RAG work: re-run this fixture and require the
answer to name `/etc/kernel/install.conf`, omit any section header, and list
all three keys.

---

## With retrieval, 2026-08-07

Same fixture, same model, same prompt. The only change is that the broker now
retrieves reference passages and puts them in the prompt.

| | no retrieval | retrieval on the question | retrieval on question + evidence |
|---|---|---|---|
| config file | `/etc/kernel-install.conf` ✗ | `/etc/portage/make.conf` ✗✗ | **`/etc/kernel/install.conf`** ✓ |
| syntax | INI `[options]` ✗ | `INITRD_GENERATOR="dracut"` ✗ | **`initrd_generator=dracut`** ✓ |
| `uki_generator` | absent | absent | **named** ✓ |
| invented packages | 2 | 1 | **none** ✓ |
| notices the retry | yes | — | yes |

### The middle column is the interesting one

Retrieval made the answer **worse before it made it better**. Querying on the
question alone returned Kernel Configuration and USE-flag pages, because the
`explain` prompt says *"be specific about which USE flags, versions or
patches"* and contains no other technical content — the error text is in the
log, which arrives after retrieval has run. Handed make.conf documentation, the
model recommended a make.conf setting that does not exist.

Retrieval that fetches the wrong reference does not degrade gracefully. It
replaces a vague wrong answer with a confident wrong answer, and the confidence
comes from the citation. **A retrieval step that cannot be shown to fetch the
right passage is a liability, not a neutral addition.**

### What is still wrong

> *"This plugin enforces that either `initrd_generator` or `uki_generator` is
> defined"*

It does not. `05-check-config.install` has three independent checks —
`layout=`, `initrd_generator=`, `uki_generator=` — each exiting 1 on its own.
Following this answer and setting only `initrd_generator` fails on the very
next run, which is precisely what happened on this machine.

So against the acceptance test: names the right file ✓, drops the invented
section header ✓, lists all three keys ✗ — it names two and gets the
relationship between them wrong.

That is a large improvement and not a pass. Worth noting where the remaining
error comes from: the retrieved passages describe *what the keys are*, and the
"all three are required" fact lives in the plugin source, which is not in the
corpus. `/usr/lib/kernel/install.d/*.install` is 16 short shell scripts and
would be a cheap addition.

---

## Adding the plugin sources, 2026-08-07

The residual error — reporting `initrd_generator` and `uki_generator` as
either/or when `05-check-config.install` requires all three keys independently
— came from a gap in the corpus rather than the model. Documentation says what
a setting *is*; only the script says what happens when it is *missing*.

`/usr/lib/kernel/install.d/` is 16 shell scripts, 48 KiB. Added along with
`/etc/portage/bashrc.d/` and `/usr/share/portage/config/` as a distinct corpus
category: **behaviour that lives in code rather than prose.**

Incremental — `build_index.py` keys files by content hash, so only the 23 new
files were embedded: 36 chunks, 0.1 minutes, two API calls. 7576 → 7612.

Retrieval on the error text now ranks the plugin source *first*:

```
0.734  gentoo-scripts/…05-check-config.install:1-37     <- new
0.653  gentoo-handbook/wiki__Installkernel:169-243
0.641  gentoo-handbook/wiki__Installkernel:423-503
```

and `KERNEL_INSTALL_LAYOUT`, `KERNEL_INSTALL_INITRD_GENERATOR` and
`KERNEL_INSTALL_UKI_GENERATOR` are all present in the retrieved text — the
three independent checks, visible as source.

Whether the model uses it is the next measurement.

### Result: no improvement, and the reason is not the corpus

| run | path | syntax | `uki_generator` | invented pkgs |
|---|---|---|---|---|
| no retrieval | ✗ | ✗ | absent | 2 |
| retrieval on question | ✗✗ | ✗ | absent | 1 |
| + evidence in query | ✓ | ✓ | named, but "either/or" | 0 |
| **+ plugin source** | ✓ | ✓ | **absent** | 0 |

Adding the plugin source did not fix the completeness gap. It arguably made it
worse: the previous run at least named `uki_generator`, wrongly; this one omits
it. Following either answer fails on the next run.

**It was not a retrieval failure.** Reconstructing the exact query the broker
built for this run:

```
0.631  gentoo-scripts/…05-check-config.install:1-37    <- top hit
0.609  gentoo-handbook/wiki__Installkernel:169-243
```

`05-check-config` source in the prompt: **true**.
`KERNEL_INSTALL_UKI_GENERATOR` in the prompt: **true**.
And the log itself mentions `uki_generator` three times.

So the model had the fact twice over — as retrieved source showing three
independent `exit 1` checks, and in the log — and still reported only the key
the error message named.

### What this actually shows

Corpus completeness has stopped being the bottleneck. What remains is a
*reasoning* step: noticing that a check with three independent gates will fail
again on the next gate once the first is fixed. That is inference from the
source, not recall from it.

Note the error message invites exactly this mistake. `05-check-config.install`
exits at the **first** missing key, so the log only ever names one — and a
human following it hits the same wall. This machine did, at 01:51 on
2026-08-07. The model is reproducing a real human failure mode faithfully,
which is arguably correct behaviour for "explain this log" and is not what
anyone wants.

Two things left to try, and they are different experiments:

- **A larger model on the same fixture.** This is a synthesis step, and the
  earlier crash-fixture result — where a 49B beat a 70B — was about diagnosis,
  not synthesis. `gpt-oss-120b` or `nemotron-ultra-253b` would test whether the
  finding inverts.
- **Asking the question directly.** A prompt that says *"and state what will
  fail next once this is fixed"* tests whether the inference is absent or
  merely unprompted. Cheaper, and it isolates the variable.

Run the prompt experiment first: if it succeeds, the corpus and the model are
both fine and the persona is the gap.

---

## Correction: most of the table above is n=1

Running the *unchanged* prompt five times against the identical retrieved
context:

| metric | rate |
|---|---|
| correct `/etc/kernel/install.conf` path | **5/5** |
| `uki_generator` mentioned | **3/5** |
| all three keys named | **3/5** |
| drifts to `make.conf` somewhere in the answer | 3/5 |

Temperature is 0.2, not 0. Every row in the comparison table above is a single
sample of a stochastic process, and the completeness metric has roughly a 40%
failure rate on its own.

### What that retracts

**"Adding the plugin source made it worse" is withdrawn.** That run omitted
`uki_generator`; so do two runs in five of the identical configuration. It was
a sample from the same distribution, not a regression. The plugin source was
demonstrably retrieved and in the prompt — that part stands, and it means the
corpus is adequate.

**"Retrieval on the question alone made it worse" is weaker than stated.** The
single output naming `make.conf` was one sample. What survives is the
deterministic part: that query retrieved **0/3** of the key terms while
question-plus-evidence retrieved **3/3**. Retrieval content is measurable
without sampling, and that measurement is sound; the downstream answer was
n=1 and should not have been reported as a clean regression.

### What survives, and is now actually measured

**Retrieval fixed the fabricated path, reliably.** Before retrieval the model
invented the config file in every run observed. With evidence-derived
retrieval it is correct 5/5. That is the result the whole exercise was for and
it holds.

**Completeness is not a corpus problem and not a prompt problem.** It is
unstable at ~60% with the fact present in the prompt twice over. No single-run
comparison between configurations can resolve a difference smaller than that.

### Method, going forward

Any claim comparing configurations needs n≥5 and a rate, not an anecdote. The
cheap parts are deterministic and should be measured directly instead: *was the
right passage retrieved* is a fact about the index and needs one run; *did the
model use it* is a distribution and needs several.

This is the same error the rest of the project keeps producing, one level up —
a check that looked conclusive and was not. It cost four confident comparisons.

---

## Scored, n=5 per configuration (`scripts/eval-answers.py`)

| check | no retrieval | with retrieval |
|---|---|---|
| names the right config file | **0/5** | **5/5** |
| no invented config path | 2/5 | 5/5 |
| correct `key=value` syntax | 1/5 | **5/5** |
| names `uki_generator` | **0/5** | **5/5** |
| names all three keys | **0/5** | **5/5** |
| identifies the failing plugin | 4/5 | 5/5 |
| invents no packages | 5/5 | 5/5 |
| **mean** | **34%** | **100%** |

Retrieval works, and the effect is far outside the noise: four checks go 0% to
100%. Everything the ad-hoc runs suggested was true; none of it had been
measured.

**n=5 is still marginal.** An earlier batch of five scored `uki_generator` at
3/5 in the configuration that scores 5/5 here — two samples of the same
distribution. Rates in the 60–100% band are not separable at this n, which is
why the harness flags any metric strictly between 0 and 1. The 0-vs-100
comparisons are safe; a 3/5-vs-5/5 comparison is not a result.

Retrieval content is reported separately and needs no n, because it is
deterministic for a fixed query:

```
PASS  index contains '/etc/kernel/install.conf'
PASS  index contains 'KERNEL_INSTALL_UKI_GENERATOR'
PASS  index contains '05-check-config'
```

That split is the point of the harness. *Was the right passage retrieved* is a
fact about the index. *Did the model use it* is a distribution.
