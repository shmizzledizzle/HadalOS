# Phase 4 — turning this installation into HadalOS

`host-systemd-migration.md` took this laptop from OpenRC Gentoo to a systemd
Gentoo booted by Limine through HadalOS entries, with last-known-good pinning
live. That is where it stops, and where this begins.

At the end of phase 3 the machine had the HadalOS **boot layer** and nothing
else: `sys-boot/hadalos-limine-hook` and `app-admin/hadalos-portage-hook`
installed, `/etc/os-release` still saying `Gentoo Linux 2.18`, SDDM starting
Plasma, and every line of cusk, the broker and the model host existing only as
`cargo` output in a working tree.

Phase 4 is the rest: package what was built, install it the way a distribution
installs itself, and let the machine identify as what it is.

---

## Status of this document

**Nothing here has been merged.** The ebuilds are written and the sources they
build are known to compile in release mode on this machine, but no `emerge` has
run against any of them, so every step below is unverified.

That is stated first because this project has now produced, twice, a component
that was correct in every part and wrong in composition — six boot-layer bugs,
five silent; and a flagship feature whose result reached the user and never the
model. A runbook that reads as though it has been executed is the same mistake
in prose.

| Piece | State |
|---|---|
| `sys-apps/hadalos-release` | written, **never merged** |
| `gui-wm/cusk` | written; source builds release clean (3m36s) |
| `gui-apps/cusk-{dock,launcher,settings}` | written; source builds release clean |
| `sys-apps/hadal-brokerd` | written, **never merged**, source not rebuilt since 2026-08-07 |
| `sys-apps/hadald` | written, **never merged** |
| `acct-{user,group}/hadal` | written, **never merged** |
| `app-misc/hadalos{,-base,-desktop,-assistant}` | written, **never merged** |
| `cusk` as a login session | `.desktop` + wrapper written, **never selected at a login screen** |

---

## 0. The overlay has to be findable

`/etc/portage/repos.conf/hadalos.conf` pointed at a directory that does not
exist — the tree had moved and the config had not. Portage reported this on
every single `emerge` invocation and it was survivable, so it survived:

```
!!! Section 'hadalos' in repos.conf has location attribute set to
    nonexistent directory: '/home/shmizzy/Documents/HadalOS-Mobile/HadalOS/overlay'
```

The consequence was not cosmetic. The two HadalOS packages already installed on
this machine could not be rebuilt, reinstalled or updated, because Portage
could not read the ebuilds they came from.

```ini
[hadalos]
location = /home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile/HadalOS/overlay
auto-sync = no
```

Verify with `emerge --info 2>&1 | grep hadalos` — silence is success.

### What the broken path had already cost

Running the boot layer's own regression suite the way its documentation says to
run it reports a failure on this machine:

```
$ bash scripts/test-limine-hook.sh
FAIL  refusing to remove the pinned kernel exits 1: got '0', want '1'
FAIL  PINNED KERNEL WAS DELETED
...
9/15 passed
```

That reads like the last-known-good pin has stopped protecting anything, which
would be the most serious possible failure in this layer. It is not what
happened, and the real explanation is worth following because it is a
consequence of the repos.conf breakage above.

The suite defaults to the **installed** copies — `/usr/bin/hadalos-limine-update`
and `/usr/lib/kernel/install.d/90-hadalos-limine.install` — so that it doubles
as a post-merge smoke test. Against the repo copies it still passes:

```
$ bash scripts/test-limine-hook.sh \
    overlay/sys-boot/hadalos-limine-hook/files/90-hadalos-limine.install \
    overlay/sys-boot/hadalos-limine-hook/files/hadalos-limine-update
15/15 passed
```

The installed copies predate the `$HADALOS_ETC` fix and still hardcode
`LASTGOOD_FILE=/etc/hadalos/lastgood`. So the test's temporary pin is ignored,
the script reads *this machine's real pin* instead, and six assertions fail
against a file the test never wrote:

```
hadalos-limine-update: recorded last-known-good 6.18.43-gentoo-dist-bin
                       is no longer installed; ignoring
```

**The boot layer itself is fine.** `/etc/hadalos` is the correct production
path and both versions use it, the two scripts differ in nothing else, and
`hadalos-mark-boot-good`, its service and its timer are byte-identical to the
repo. There is no functional regression on this machine.

What there is: an installed package that no longer matches its source, because
the overlay it was built from could not be read, because a path in repos.conf
was stale. Rebuilding it is the fix, and rebuilding it required fixing the path
first:

```bash
emerge -av1 sys-boot/hadalos-limine-hook
bash scripts/test-limine-hook.sh     # should be 15/15 against the installed copies
```

The lesson is the one this layer keeps teaching. The fix that made the crisis
path testable was written, committed and never installed, and the only thing
that noticed was a test suite whose failure looked like something far worse
than the truth.

---

## 0b. The live packages have to be accepted

Six of the packages are `-9999` live ebuilds and carry `KEYWORDS=""`. Portage
reports them as

```
!!! All ebuilds that could satisfy "gui-wm/cusk" have been masked.
```

which reads like a policy decision somewhere in the profile rather than a
missing line in this repo. Empty keywords are deliberate: a keyworded live
ebuild gets pulled into ordinary dependency resolution and rebuilt from a
moving target without anyone asking for it.

```bash
sudo install -d /etc/portage/package.accept_keywords
sudo install -m 0644 HadalOS/portage/hadalos.accept_keywords \
                     /etc/portage/package.accept_keywords/hadalos
```

`**` is the only accept_keywords value that matches a package with no keywords
at all — `~amd64` on a live ebuild silently matches nothing and looks exactly
like not having listed it. `scripts/test-overlay.sh` asserts both that every
package in the overlay appears in that file and that every live one is
accepted with `**`; it caught `sys-kernel/hadalos-kernel` missing from the list
on its first run.

Know what this signs up for: **`emerge -uDN @world` will rebuild all six from
whatever is committed at that moment.** That is what a live ebuild is, and it
is the reason to cut versioned ebuilds before anyone else installs this.

## 1. Identity

```bash
emerge -av sys-apps/hadalos-release
etc-update            # keep the new /etc/os-release
. /etc/os-release && echo "$PRETTY_NAME ($ID, like $ID_LIKE)"
```

`/etc` is config-protected, so **merging this package changes nothing on its
own** — the file lands as `/etc/._cfg0000_os-release` and waits. Skipping
`etc-update` leaves a machine that reports itself as Gentoo and a package
database that says otherwise.

`ID=hadalos`, `ID_LIKE=gentoo`. Portage does not read `os-release`, so `emerge`
is unaffected; anything that reads it and honours `ID_LIKE` keeps taking the
Gentoo path.

Every future `sys-apps/baselayout` upgrade offers to restore its own symlink
over this file. Keeping the HadalOS file is the answer, every time.

---

## 2. The desktop

```bash
emerge -av app-misc/hadalos-desktop
```

Then **log out, and choose "HadalOS (Cusk)" from the session menu.**

Cusk is not made the default and Plasma is not removed. On Wayland a compositor
crash takes every client with it, which is a materially worse failure than the
X11 equivalent, and this is the only machine. The compositor should earn the
default rather than be handed it — the same two-entry principle as the Limine
layout, where the recovery path exists before it is needed.

Before trusting it with a session, confirm it can drive the display. This is
safe from inside a running desktop because it never takes DRM master:

```bash
cusk --probe-drm
```

If the session ends immediately: `journalctl --user -t cusk -b`.

### Three things the session wrapper exists to prevent

`cusk-session` is what the `.desktop` file runs, rather than the bare binary.
Each of the three differences between "started by hand from a VT" — the only
way cusk had ever been started — and "started by a display manager" fails
quietly:

1. **`cusk --tty` defaults to `--seconds=30` and then exits cleanly.** It is a
   watchdog for a compositor that could strand a display. As a session it is a
   desktop that vanishes after half a minute and reports success. `--seconds=0`
   means no limit and is not optional here.
2. **The dock hosts `org.kde.StatusNotifierWatcher` on the session bus.** With
   no session bus the dock still starts, still draws, and has a permanently
   empty tray with nothing logged to say why.
3. **Session output goes wherever the display manager decided.** Routed to the
   journal under a known identifier, "why did my session not start" has an
   answer that is not a hunt through `~/.local`.

---

## 3. The assistant

```bash
emerge -av app-misc/hadalos-assistant

install -d -m 0700 -o hadal -g hadal /etc/hadal
echo 'HADAL_MODEL=<model-id>' > /etc/hadal/hadald.env
printf '%s' "$UPSTREAM_KEY" > /etc/hadal/upstream.key
chown hadal:hadal /etc/hadal/upstream.key
chmod 600 /etc/hadal/upstream.key

systemctl enable --now hadald.service
systemctl enable --now hadal-brokerd.service
```

Then, **as your own user and not as root**:

```bash
hadal status
hadal explain      # needs a recorded Portage failure
```

`sudo hadal` is strictly worse than `hadal`. polkit authorizes root for every
capability with no prompt, so running the client as root gets the same
capability set with the authorization gate switched off. The broker already
holds the privilege and acts on your behalf.

### Two bugs in the shipped unit, found by reading it against the daemon

Both were introduced by writing the unit before the daemon existed and never
starting the two together. Both are fixed in `HadalOS/systemd/hadald.service`,
and neither had ever run:

- **`ExecStart` passed `--config /etc/hadal/hadald.toml`.** `hadald` has no
  `--config` flag and never has. `config.rs` parses arguments with a closed
  match whose fallback is `unknown argument: {other}`, so the daemon would have
  exited with a usage error before opening a socket. The model id now comes
  from `EnvironmentFile=/etc/hadal/hadald.env`; the key deliberately stays a
  file, because a process environment is readable through `/proc/<pid>/environ`,
  is inherited by children, and shows up in `systemctl show`.

- **`--egress-log` wrote under `/var/log` with `ProtectSystem=strict` set.**
  The path exists and the write is refused. This is the boot layer's finding
  wearing a different hat — *"`ReadWritePaths=` must name the same boot root the
  service actually writes to, or `ProtectSystem=strict` silently blocks the
  write"* — and the egress log is the worst place to rediscover it, because a
  privacy record that silently fails to open is indistinguishable from one with
  nothing to report. `LogsDirectory=hadal` creates the directory *and* punches
  it through the read-only mount; `ReadWritePaths=` alone would do the second
  without the first.

### What this costs, said before it is switched on

`hadald` is backed by a remote endpoint. **Safety is intact and privacy is
broken** — proposals are still typed, validated and polkit-gated, because the
broker was built not to trust the model wherever it runs, but Portage build
logs and journal excerpts carry hostnames, usernames, absolute paths and
occasionally tokens. `/var/log/hadal/egress.log` is what makes *"what left this
machine"* answerable.

The route back is `docs/tier-routing.md`: route on **whether the data must stay
here**, derived from the capability table rather than a parallel heuristic, and
**refuse rather than substitute** when a local-only request has no local model.
Designed, not built.

---

## 4. Reproducing this somewhere else

The conversion is `emerge app-misc/hadalos` plus the three steps an ebuild
cannot take — accepting the identity file, enabling the boot-good timer, and
giving `hadald` a model and a key. The package layering is what makes that
work, and it is deliberate:

| Package | Pulls | For |
|---|---|---|
| `app-misc/hadalos-base` | identity, Limine hook, Portage hook | a build host: captures failures, boots pinned, runs no model |
| `app-misc/hadalos-desktop` | cusk + dock + launcher + settings | a workstation |
| `app-misc/hadalos-assistant` | broker + model host | anything that answers questions |
| `app-misc/hadalos` | all three | the whole system |

`hadalos-base` runs no model on purpose. `app-admin/hadalos-portage-hook`
records build failures whether or not anything is around to read them, so a
build host can capture material without hosting an assistant.

### The gap that stops this being reproducible today

**There is no published remote.** The live ebuilds fetch from
`EGIT_REPO_URI="${HADALOS_GIT_REPO:-/home/shmizzy/Hadalpoint/Projects/HadalOS-Mobile}"`
— a path on this laptop. That is enough to install HadalOS *here* and not
enough to install it anywhere else, and no amount of package layering fixes it.

Publishing the tree is therefore not housekeeping, it is the remaining
requirement for the word "distribution" to apply. Until then `HADALOS_GIT_REPO`
can point at a clone, which moves the problem rather than solving it.

### And the pinning question behind it

The ebuilds are `-9999` live ebuilds: they build whatever is committed, and a
merge is only reproducible with respect to a commit nobody recorded. Cutting
versioned ebuilds means pinning the dependency set, which is what
`scripts/gen-crates.sh` is for — it turns a `Cargo.lock` into the `CRATES` list
`cargo.eclass` expands into `SRC_URI`, and refuses on git-sourced dependencies
rather than emitting a list that would fetch the wrong thing. Measured on the
four cusk crates: 270, 559, 533 and 434 crates, no git sources in any of them.

Live ebuilds are the right choice while the thing is being built daily. They
are the wrong choice for anything anyone else installs.
