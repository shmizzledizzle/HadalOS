# Installer

How HadalOS gets onto a machine that is not this laptop.

`host-conversion.md` §4 already states the whole job in one sentence:

> The conversion is `emerge app-misc/hadalos` plus the three steps an ebuild
> cannot take — accepting the identity file, enabling the boot-good timer, and
> giving `hadald` a model and a key.

That is the installer's remit, and it is worth noticing how small it is. The
package layering does the installing. **The installer exists to perform the
steps an ebuild cannot**, plus the two that conversion never had to do at all:
partition a disk, and put a bootloader on a system that has none.

---

## 1. Not Calamares

Calamares is the right reference and the wrong dependency.

The good idea is its core: an install is a **queue of jobs**, not a script.
Modules contribute jobs, the queue is inspectable, and a job either succeeds
or fails the install. That model is correct and this document keeps it.

The reason not to adopt the implementation is a dependency count. Calamares
is C++/Qt with KPMcore underneath. `host-conversion.md` §2 has SDDM as the
last Qt component in the stack, and `ARCHITECTURE.md` §0 records `greetd`
with a custom greeter as the intended end state that replaces it. Adopting
Calamares would re-entrench Qt6 in a distribution that is in the middle of
removing it, to get a job queue that is a few hundred lines of Rust.

The other reason is that Calamares' CLI story does not exist. There is
`-d` for debug and there are unattended-ish paths, but there is no TUI with
parity to the GUI, because the frontend and the engine are one process. The
GUI↔CLI switch asked for here is not a feature to bolt onto that shape. It
is a consequence of a different one.

Working name **`descent`**.

---

## 2. The plan is the product

The engine does not install. It **emits a plan**: an ordered, serialisable
list of typed jobs. A separate step executes it.

This is `ARCHITECTURE.md` §2.1 applied one layer out. The model never gets a
shell; it emits typed proposed actions that a validator checks and an executor
runs. An installer frontend never gets a shell either. It assembles typed jobs
that a validator checks and an executor runs.

Five things fall out of that, and only the first was asked for.

**The GUI↔CLI switch is not implemented.** The engine runs as a daemon over a
unix socket. Both frontends are clients attached to the same running install,
rendering the same plan and the same job states. Switching is a client
detaching and another attaching — there is no state to hand over, because
neither frontend held any. Ctrl-Alt-F2 out of the GUI, `descent attach`, and
the progress bar is where you left it.

**Dry run is free.** A plan can be printed without being executed. That is
`emerge --pretend` for installation, which is the right idiom for a Gentoo
derivative and the right answer for a step that repartitions a disk.

**The install is resumable.** A plan plus per-job state is a checkpoint. A
failed `emerge` at job 19 of 24 does not mean starting from the partitioner.

**Unattended install is the same code path.** A saved plan file is an answer
file. No second implementation, no divergence between what the GUI does and
what the automation does.

**The conversational installer gets cheap.** `ARCHITECTURE.md` §2.6 defers it
to v2 as something that "roughly doubles v1 scope." Against this shape it does
not: Hadal emits a plan — typed data, validated by the same validator, subject
to the same confirmation — rather than driving a GUI. It becomes a third
frontend, and the §2.1 rule holds without an exception being carved for it.

---

## 3. Jobs as a closed set

```rust
pub enum Job {
    Partition { disk: DiskId, scheme: Scheme },
    Format { part: PartId, fs: Fs, label: String },
    Mount { part: PartId, at: PathBuf },
    UnpackStage3 { source: Source, root: PathBuf },
    ConfigurePortage { profile: Profile, accept_keywords: bool },
    EmergeSet { set: ProfileSet },        // base | desktop | assistant | all
    AcceptIdentity,                       // the /etc/os-release step, see §5
    InstallLimine { esp: PathBuf },
    PinLastGood { kernel: String },
    EnableUnit { unit: String },
    ProvisionHadal { model: String, key: KeySource },
    CreateUser { name: String, groups: Vec<String> },
}
```

`ProfileSet` is not invented here. It is the four packages
`host-conversion.md` §4 already layers — `hadalos-base`, `-desktop`,
`-assistant`, and `hadalos` — so "what kind of machine is this" is a choice
among existing packages rather than a parallel notion of profiles that can
disagree with them. A build host picks `base` and runs no model, exactly as
that table intends.

Three methods on the enum carry the design:

```rust
impl Job {
    fn describe(&self) -> String;      // what the frontends render
    fn is_destructive(&self) -> bool;  // §4
    fn verify(&self) -> Result<()>;    // §5 — and this is the important one
}
```

Closed set, same discipline as `Capability` and for the same reason: a new job
cannot inherit another job's answers by default. The match must be extended.

---

## 4. The one-way doors

Everything conversion did was, in principle, undoable. It operated on a system
that already booted. The installer has two jobs that are not:
`Partition` and `Format`. This is the genuinely new risk surface and it should
be treated as the only dangerous part of the program, because it is.

**Disks are named by stable ID, never by kernel name.** `/dev/sda` is
enumeration order and enumeration order is not stable across boots. A plan
that says `/dev/sda` and is executed after a reboot — which resumability in §2
makes an ordinary occurrence — is a plan that can wipe a different disk than
the one the user pointed at. `DiskId` wraps `/dev/disk/by-id/…`, and the
partitioner resolves it at execution time and fails if it does not resolve.

**A destructive job must be confirmed against what it will destroy**, by
listing the existing partitions and their contents, not by naming the target.
"Erase `/dev/disk/by-id/nvme-eui.0025...`" is not information. "Erase 1.8 TB
containing an ext4 filesystem labelled `home`" is.

**Unattended plans carry destructive jobs only with an explicit flag.** An
answer file that silently repartitions is a footgun aimed at whoever copies it
from a wiki.

---

## 5. Verify, or this document is worthless

`host-conversion.md` records four bugs found by merging rather than reading,
and says the thing that matters about them:

> Two of those four were *silent successes* — output that read as working.
> That ratio is the same one the boot layer produced.

Six bugs in the boot layer, five silent. Four in the conversion, two silent.
This project's characteristic failure is not a crash. It is a green line
asserting nothing — a check helper named `head` shadowing `/usr/bin/head` and
printing six `ok`s; `/etc/os-release` merged over baselayout's symlink,
identity correct, bytes in the wrong file, silent revert pending.

An installer is the worst possible venue for that failure, because the
operator has no prior knowledge of the machine to notice it against.

So `verify()` is not optional per job and it does not check the invocation. It
observes the end state on the target:

> **A job may not report success until `verify()` has observed its end state
> on the installed system.** Not "the command exited 0." Not "the package
> merged." The state the job existed to produce.

`AcceptIdentity` is in the enum specifically because of this. §1 of
`host-conversion.md` records that `/etc` is config-protected, so merging
`sys-apps/hadalos-release` **changes nothing on its own** — the file lands as
`/etc/._cfg0000_os-release` and waits. An installer that emerges the package
and reports success has installed a machine that calls itself Gentoo. Its
`verify()` sources `/etc/os-release` on the target root and asserts
`ID=hadalos`, and it is a distinct job rather than a step inside `EmergeSet`
because the thing that can silently fail deserves its own green line.

---

## 6. Two frontends, and which one is first

The TUI ships first. The reason is in the repo's own status table, not in a
preference.

`host-conversion.md` §Status: cusk is "installed and offered by SDDM, **never
yet selected**." The document is blunt about it — "The desktop is installed.
Nobody has logged into it. Those are different claims and this project has a
history of eliding exactly that difference."

A GUI installer on a live medium is a Wayland client under cusk, on hardware
nobody chose, with no fallback desktop present — the ISO has no Plasma to fall
back to, which is the whole safety net §2 of that document relies on. And
`ARCHITECTURE.md` §0 already notes a compositor crash takes every client with
it. So the GUI installer's prerequisite is not "write the GUI." It is "cusk
reliably hosts a session on unknown hardware," which is currently unproven on
one machine.

The TUI has no such prerequisite. It needs a terminal.

This is the two-entry principle again, and it is now the third time it has
decided something: Limine always carries a last-known-good entry, cusk was
offered as a session without being made default, and the installer's recovery
path — a working TUI — exists before the GUI needs it rather than after.

Both are clients of §2's socket, so this is sequencing, not scope reduction.

---

## 7. What blocks an ISO today

Stated here rather than discovered later, because it is absolute.

This was the blocking one, and it is no longer blocking. The live ebuilds used
to fetch from a path inside one developer's home directory, which an ISO booted
on another machine cannot resolve — publishing the tree was a prerequisite for
the installer to install anything, and no amount of installer design
substituted for it.

Closed 2026-08-25. `host-conversion.md` §4 records the default the ebuilds now
carry:

```
EGIT_REPO_URI="${HADALOS_GIT_REPO:-https://github.com/shmizzledizzle/HadalOS.git}"
```

An ISO can resolve that, which means the installer's remaining problems are its
own.

Two consequences worth being honest about:

- The six `-9999` live ebuilds build whatever is committed at that moment. An
  installer that runs them produces a machine reproducible with respect to a
  commit nobody recorded. Versioned ebuilds via `scripts/gen-crates.sh` are
  therefore an installer prerequisite too, not housekeeping.
- Until both are true, `descent` can be developed and tested against a local
  binhost and a loopback disk image, which is worth doing — but a green
  install in that harness is not evidence that anyone else can install this.

## 8. The last-known-good problem at install time

`ARCHITECTURE.md` §3's invariant is that the generated `limine.conf` always
contains a last-known-good entry and never garbage-collects the kernel it
points at. On a fresh install there is exactly one kernel, so "newest" and
"last known good" are the same entry, and the invariant is satisfied
vacuously by a configuration that protects nothing.

`PinLastGood` is in the enum to force an answer rather than let the vacuous
case ship. The open question is which answer:

- Pin the single kernel immediately, accepting that the pin is meaningless
  until a second kernel exists.
- Leave the pin unset and have `hadalos-mark-boot-good` establish it on first
  successful boot, accepting a window with no recovery entry.

The second is more honest and has the worse failure mode. Undecided.

---

## Open

- Does `descent` reuse Portage's own partitioning-adjacent tooling, or own
  `Partition`/`Format` outright? Owning it means writing the one part of the
  installer that can destroy data. Not owning it means a dependency in the
  one place §4 wants total clarity about behaviour.
- TUI toolkit. `ratatui` is the obvious answer and adds a dependency tree to a
  binary that may need to run in early boot. `clay` is the opposite trade —
  zero dependencies, but no text shaping and no renderer, both of which then
  have to exist somewhere. Leaning `ratatui`, because the installer runs on a
  live medium with a real userspace, not in an initramfs.
- Is the engine daemon socket-activated, or started by the first frontend to
  attach? Socket activation is the systemd-native answer and matches the rest
  of the stack; on a live medium the first-attach model has fewer moving parts.
- Where does `descent` live — this repo, or its own? It is the first component
  that is not part of a running HadalOS.
