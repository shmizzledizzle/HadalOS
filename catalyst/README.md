# Building HadalOS with catalyst

Four stages, run in order, each consuming the previous one's output:

```
stage1  ← a stock Gentoo systemd stage3 (seed)
stage3  ← stage1            the base system tarball
livecd-stage1 ← stage3      everything the ISO will contain
livecd-stage2 ← livecd-stage1   cleaned rootfs, squashed
```

Then `scripts/mkiso.sh` turns that squashfs into a bootable image.

## Running it

On a Gentoo host with `dev-util/catalyst`, as root:

```bash
catalyst/build.sh --dry-run
```

Drop `--dry-run` to build. One timestamp and one tree snapshot are threaded
through every stage, which is what makes each stage consume the output the
previous one actually produced.

To resume after a failure without rebuilding earlier stages:

```bash
HADALOS_TIMESTAMP=20260802T120000Z catalyst/build.sh livecd-stage1 livecd-stage2
```

## The seed

stage1 needs a seed, because there is no HadalOS to build HadalOS with until
it has run once. Fetch a current Gentoo systemd stage3 and place it where the
spec expects:

```bash
install -Dm644 stage3-amd64-systemd-*.tar.xz /var/tmp/catalyst/builds/hadalos/stage3-amd64-systemd-seed.tar.xz
```

Once you have your own stage3, point `stage1.spec`'s `source_subpath` at it
instead and the distribution becomes self-hosting.

## Why the ISO is not built by catalyst

Catalyst can produce a bootable ISO itself, via `livecd/cdtar` and its own
isolinux/GRUB handling. HadalOS does not use that path.

Limine is installed differently from either: it wants `limine bios-install`
run against the finished image, plus its own `limine.conf`. Neither fits
catalyst's cdtar machinery, and bending it into shape would mean carrying a
patched cdtar and hoping the step order never changes upstream.

So catalyst does what it is genuinely good at — resolving, building and
cleaning a rootfs, then squashing it — and `scripts/mkiso.sh` assembles the
bootable image with xorriso and Limine directly. Each half is independently
testable, which the alternative is not: `scripts/test-mkiso.sh` builds a real
ISO from a synthetic rootfs and inspects both El Torito boot records without
needing Gentoo, catalyst, or a machine to reboot.

## Checks

```bash
python3 scripts/check-catalyst-specs.py
```

Validates spec syntax, required keys, and — the one that matters — that each
stage's `source_subpath` names the previous stage's actual output. Catalyst
does not check this. If it is wrong and a similarly-named artefact is already
on disk from an earlier run, the build succeeds against the wrong input and
says nothing.

## Flags

`portage_confdir/make.conf` compiles for `x86-64-v2`, deliberately **not**
`-march=native`. The build host's own `make.conf`, written by
`scripts/bootstrap-buildhost.sh`, does use native — correct there, and
catastrophic here. A stage3 built with native flags SIGILLs on any machine
with a different CPU, at some arbitrary later moment rather than at boot, so
the cause is far from obvious.
