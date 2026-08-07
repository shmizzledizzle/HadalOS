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
