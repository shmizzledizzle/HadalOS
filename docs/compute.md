# Compute

How HadalOS uses every processor on a machine without letting any of them
hurt the machine.

The request that produced this document was "our own version of CUDA." That
is the wrong target, and the reasons are worth writing down because the
*motivation* behind it is right and points at a component this project
actually needs.

---

## 1. Why not a CUDA

CUDA is three separable things, and only one of them is interesting here.

| Layer | What it is | Cost to reproduce |
|---|---|---|
| Language + compiler | A C++ dialect, `nvcc`/NVVM, PTX as a virtual ISA | Hundreds of engineer-years |
| Runtime / driver API | Allocate, copy, launch, synchronise, stream | Tractable — this is the small one |
| Libraries | cuBLAS, cuDNN, NCCL, cuFFT, and the ecosystem on top | The actual moat |

The instinct that CUDA is about memory is not baseless — memory locality is
where GPU performance is won or lost, and CUDA's shared-memory model,
coalescing rules and unified addressing are precisely why it wins. But that
is a *property of well-written kernels*, not a service the platform performs
on your behalf. There is no CUDA component that speeds up memory. Writing
one is not a thing that can be done.

The legal framing was also inverted in the asking, and it matters because it
changes what is safe to build:

- Reimplementing an API *surface* got safer, not riskier, after
  *Google v. Oracle* (2021). That is why ZLUDA, SCALE and HIP's `hipify`
  exist in the open.
- What NVIDIA actually enforces is an EULA term forbidding CUDA output from
  being run through a translation layer on non-NVIDIA hardware, plus closed
  libraries and patents.

So the constraint is narrow: do not ship their SDK, headers or binaries, and
do not build a CUDA translation layer. Writing an independent compute stack
is unremarkable and always has been. There is no fortress here to envy.

### The fact that settles it

ARCHITECTURE.md §0 fixes reference hardware at RX 9060 (RDNA4) and Iris Xe,
and states plainly that there is no proprietary NVIDIA module to lag behind
the kernel — that is load-bearing for the mainline-tip decision. A CUDA
clone would be a compute layer for the one vendor HadalOS does not target,
bought at the price of the in-tree property the kernel policy depends on.

**Decision (2026-08-23): no CUDA source compatibility, no `nvcc` equivalent,
no PTX, no translation layer.** Restated as a closed statement of scope so it
cannot drift: the deliverable below compiles no C++ dialect and parses no
CUDA.

---

## 2. What was actually being asked for

> any device it's installed on can find the best way to utilize all of its
> resources to the fullest without endangering the system

Read that again with the broker in mind. It is two clauses, and they are the
same two clauses `hadal-brokerd` already implements for privilege:

- **Find and use everything** — enumerate the machine's processors and route
  work to whichever one should have it.
- **Without endangering the system** — under a policy, with a reservation the
  policy may never spend.

That is not a compute *language*. It is a compute *broker*. HadalOS already
has the pattern for it, one layer down, and the shape transfers exactly:
a closed-set capability table, policy that a new entry cannot silently
default past, and an invariant worth testing.

Working name **`sonar`** — it maps what is down there before anything is
dispatched. Naming is not settled; the architecture below does not depend on
it.

---

## 3. Two layers, and only one of them is ours to write

### 3.1 Execution: Vulkan compute, SPIR-V as the portable ISA

The options for running a kernel on a GPU without a vendor blob:

| Option | Status on our hardware | Dependency cost |
|---|---|---|
| **Vulkan compute** | RADV (RDNA4) and ANV (Iris Xe), both in Mesa, both in-tree | **Already linked by cusk** |
| OpenCL via Rusticl | Real, Mesa's own, OpenCL 3.0 on radeonsi/iris | Another Mesa component + an ICD loader |
| ROCm / HIP | AMD-only, RDNA4 support recent and partial | Enormous, and famously fragile |
| SYCL / oneAPI | Intel-led, works, portable in principle | Enormous, and a C++ toolchain |

Vulkan wins on this project's own stated axis, and the argument is one line
long: **cusk already links Vulkan-capable Mesa to put pixels on screen.** A
compute path over Vulkan adds a use of a dependency the system cannot remove
anyway. Every other option adds a dependency tree to a distribution whose
explicit goal is to have as few as possible.

ROCm deserves the specific rejection rather than a general one, because it is
the obvious thing to reach for on an AMD box: it is AMD-only, which forfeits
the Iris Xe half of the reference hardware, and it is a multi-gigabyte stack
whose consumer-RDNA support has historically trailed its release notes.
Pulling it in would trade the in-tree property for one vendor's userspace.

**What this costs, stated rather than hidden.** Vulkan compute gives us no
BLAS and no cuDNN. There is no equivalent, and pretending otherwise is how
this document would become the greeter row in ARCHITECTURE.md §0 — an intent
recorded as a state. For general-purpose numerics that is a real gap. For
the workload HadalOS actually has, it is not, which is §5.

### 3.1a Verified, 2026-08-23 — and what it found

The table above was a claim about in-tree drivers. `src/sonar/probe/` now
tests it by dispatching a real compute shader and checking the results, since
an installed ICD is not evidence a shader ran — this laptop carries
`radeon_icd` with no AMD GPU in it.

```
[0] Intel(R) Iris(R) Xe Graphics (ADL GT2)
    type integrated   api 1.4.354   vendor 0x8086 device 0x46a8
    max workgroup invocations 1024   shared mem 48 KiB
    qf 0: 1 queue(s) graphics compute transfer
    heap 0: 11.4 GiB device-local host-visible
OK dispatched 4096 elements, all correct
```

**§3.1 holds.** ANV reports Vulkan 1.4.354 — current API level, no blob, no
vendor userspace. The execution layer is real on the reference laptop.

Two findings that are not "it works", and both constrain §4:

**One queue family, one queue, doing graphics *and* compute.** There is no
separate compute queue on this GPU to isolate work onto. That option does not
exist in the hardware, so §4.3 cannot be implemented by giving compute a
lower-priority queue — see §4.3a.

**One unified heap, 11.4 GiB, device-local *and* host-visible.** The iGPU's
"VRAM" is system RAM, out of 15.2 GiB total. So on integrated hardware the
§3.2 gap is not simply "systemd cannot cap VRAM" — the memory in question *is*
the memory `MemoryMax=` governs, reached through a different allocator. Whether
i915 GEM allocations are charged to the calling process's memory cgroup then
decides whether §3.2 is a gap or a much worse one. **Measured in §3.2a:** they
are charged, so the cap holds — but the way it enforces is worse than the way
it accounts.

### 3.2 Governance: the part that does not exist anywhere

This is the component worth building, and the reason it has to exist as a
daemon rather than a unit file is a concrete gap:

**systemd can cap CPU and RAM. It cannot cap VRAM.** `hadald` already carries
`MemoryMax=` and `CPUQuota=` (ARCHITECTURE.md §2.2) precisely so the
assistant "can never starve the desktop." That guarantee ends at the PCIe
bus. cgroups v2 has no VRAM controller; the DRM cgroup work has been in and
out of review for years and what has landed is per-driver, largely
accounting rather than enforcement. So the sentence "the assistant can never
starve the desktop" is currently **true for RAM and false for VRAM**, and a
model offloaded to the GPU is exactly the workload that makes that matter.

Riding mainline tip is an advantage here for once: whatever DRM cgroup
enforcement lands, we get it early, and `sonar` should consume it rather than
duplicate it when it does.

### 3.2a Measured, 2026-08-23 — the premise above is wrong on integrated

`src/sonar/probe/gemcharge.c`, run under `systemd-run --user --scope` with
`MemoryMax=`. Three modes, because "charged" depends on who faults the pages
in. 1 GiB in 128 MiB chunks, cap 4 GiB:

| pages touched by | charged | ratio | at a cap it exceeds |
|---|---|---|---|
| nothing — allocate and bind only | **no** | 0.03 | nothing happens |
| CPU — `mmap` + `memset` | yes | 1.03 | reclaimed to swap, process survives |
| GPU — compute shader writes | yes | 1.04 | **`VK_ERROR_DEVICE_LOST`** |

**The good news, and it retires the open question this test was filed under:**
i915 GEM pages faulted in by the GPU *are* charged to the allocating process's
memory cgroup. `MemoryMax=` is **not** bypassable by offloading to the GPU, so
ARCHITECTURE.md §2.2's promise that the assistant "can never starve the
desktop" holds on integrated hardware. §3.2's framing — "systemd cannot cap
VRAM" — is simply false here, because the VRAM *is* system RAM and the
accounting follows it.

**The bad news is the failure mode.** Rows two and three are charged
identically and behave nothing alike. CPU-touched shmem is reclaimable, so at
the limit the kernel swaps it and the process degrades. GPU-resident pages are
pinned while the GPU needs them, so the cgroup cannot reclaim; the submission
fails instead and ANV reports device loss. Measured with cap 512 MiB against a
1 GiB allocation:

```
mode=cpu   memory.current pinned at 511.6 MiB, ratio 0.50, process completed
mode=gpu   FAIL submit: VkResult -4 (VK_ERROR_DEVICE_LOST) + Mesa BO dump
```

`VK_ERROR_DEVICE_LOST` invalidates the entire `VkDevice`. Every resource must
be recreated; there is no partial recovery.

**Blast radius: contained, verified.** `journalctl -k` shows no GPU hang and no
GT reset — this was the kernel refusing to pin within the limit, not a hardware
fault — and the compositor and session were unaffected. So a compute client
that busts its cap kills its own context and nothing else. That is the answer
one wants, and it was worth checking rather than assuming, because the opposite
answer would have made §4.3 a much harder problem.

**Three consequences.**

1. **A cap that works but loses the device is not obviously better than one
   that does not work.** This is the real §3.2 gap, restated: not accounting,
   enforcement *granularity*. `hadald` offloading a model to the iGPU under a
   `MemoryMax=` slightly too low does not swap and slow down — it loses the
   GPU mid-inference. §5's claim has to survive that, so `hadald` must either
   size its offload to its cgroup budget up front or handle `DEVICE_LOST`.
2. **Leases must be denominated in bytes *touched*, not bytes allocated.**
   Row one is the trap: a 1 GiB reservation costs 27 MiB until something
   faults it. A lease that counts allocations is measuring a quantity the
   kernel does not charge, and would report a machine as full while
   `memory.current` sat near zero — or the reverse.
3. **It strengthens §4.3a.** Cooperative admission control is not merely the
   only lever available; it is *necessary*, because the mandatory lever
   available today fails destructively.

**Untested, and it inverts:** on a discrete GPU, VRAM is not system RAM and
almost certainly is *not* charged to any cgroup. So §3.2's original claim is
probably right for discrete and wrong for integrated, which means `sonar`
cannot use one accounting model for both. §6a's tower can answer this.

---

## 4. The model

### 4.1 Inventory — the sketch, and what running it found

The original sketch here was:

```rust
pub enum Unit {
    CpuCcx { id: u8, cores: u8, l3_mb: u16 },  // 9800X3D: cache asymmetry is real
    IntegratedGpu { drm: PathBuf },
    DiscreteGpu { drm: PathBuf, vram_mb: u32 },
    Npu { drm: PathBuf },
}
```

**`CpuCcx` was wrong, and step 1 found it the first time it ran** (2026-08-23,
`src/sonar`). It is AMD-shaped: on a 9800X3D the asymmetry is *cache at equal
frequency*, so "CCX" is both the grouping and the reason for it. The reference
laptop is an i5-1235U, where the asymmetry is *frequency and SMT at similar
cache* — 2 P-cores at 4.4 GHz with SMT and 1280K private L2, 8 E-cores at
3.3 GHz without SMT and 2048K shared per cluster of four. A type named after
one vendor's mechanism cannot hold the other's.

The replacement is a **core class** — a set of CPUs interchangeable with each
other and not with the rest — plus the *axis* on which classes differ,
reported rather than assumed. Both vendors' asymmetries are then expressible
without either being the special case, and a cache domain within a class
covers E-core clusters and CCXs with the same field.

This is what step 1 was for. The enum was written from one machine's datasheet
and it survived exactly until it met the other machine.

The grouping key is `(max_khz, smt, l2_kb, l3_kb)`, and the two halves of the
L3 rule are worth stating because they pull in opposite directions:

- **L3 size is in the key.** An X3D part has two CCXs at the same frequency
  and SMT width, one with stacked cache. Without L3 in the key that asymmetry
  is invisible — which was true of the first implementation, caught by a
  fixture rather than by hardware.
- **L3 *sharing* is not in the key.** Two same-size CCXs are interchangeable
  and must stay one class; splitting them would report a plain Ryzen as
  heterogeneous and send every consumer down a routing path for no gain.
  Locality stays visible as two `l3_domains` inside the one class.

Both cases are tested. Neither has run on AMD hardware — see §4.1c.

### 4.1a The attribute that lies

Worth recording separately, because it is a trap and not a design choice.

The obvious way to detect a heterogeneous CPU is `cpu_capacity`. On this
laptop **every CPU reports 1024** — the attribute is populated by the arm64
topology code and is uniform on x86 hybrid. `/sys/devices/system/cpu/types/`
(`intel_core` / `intel_atom`) would be the clean discriminator and does not
exist on this kernel either.

So code that reads the obvious attribute reports a hybrid CPU as homogeneous,
and reports it *confidently*. That is this project's signature failure — the
green line asserting nothing — reached this time through an attribute that is
present, readable, and wrong. `sonar` derives classes from `cpuinfo_max_freq`,
SMT width and L2 geometry instead, and prints an explicit note when
`cpu_capacity` is uniform across more than one class, so the trap is observed
on each machine rather than only described here.

### 4.1b `contends_with_display`

This is the whole point, and the iGPU is why it is a method and not a
device-class constant. An integrated GPU shares both memory bandwidth and
execution resources with the thing drawing the screen. A discrete GPU that is
not driving a display does not. Same device class, opposite answers.

Integrated vs discrete is itself derived, because nothing in sysfs says
"discrete". What distinguishes them is dedicated memory, under a per-driver
name: `mem_info_vram_total` for `amdgpu`, `lmem_total_bytes` for `i915`/`xe`
on discrete parts, neither for an iGPU. Matching on the driver name would get
the right answer on both known machines and the wrong one on an Arc card.

Measured here: `card0`, `i915`, integrated, driving `eDP-1`, contends — which
is the answer the laptop should give, arrived at without the code knowing it
was a laptop.

**Absence is only meaningful for a driver that would have spoken.** The first
implementation read "neither attribute present" as *integrated*, which §6a
predicted would misreport a nouveau card. There are now three states —
`Dedicated(bytes)`, `Shared`, `Unknown` — and absence maps to `Shared` only
for drivers known to expose dedicated memory when they have it (`i915`, `xe`,
`amdgpu`). Everything else is `Unknown`.

`Unknown` **contends**. Assuming a GPU we cannot classify does not compete
with the display is the unsafe direction, so the conservative answer is the
default — the same shape as refusing rather than substituting a remote model,
and as never garbage-collecting the pinned kernel.

### 4.1c Tested against hardware that does not exist here

`src/sonar/src/fixture.rs` builds synthetic sysfs trees, so the paths §6 calls
"written and unobserved" are at least *exercised*: AMD single-CCX, X3D dual-CCX,
equal-size dual-CCX, nouveau discrete, amdgpu discrete headless, iGPU+dGPU
hybrid, homogeneous CPU, absent `cpufreq`, connector status `unknown`, missing
render node. 27 tests, no dependencies.

The limit is worth being exact about, because a fixture that is mistaken for
hardware is the same error as a doc mistaken for a running system. **A fixture
tests what this code does with a given sysfs layout, not whether that layout is
what the driver writes.** The Intel layout is transcribed from this machine and
is therefore real. The AMD layouts are inferred from how those parts are
described and have never been observed; each such test says so in a comment.
They are a guard against regression and a way to find design errors early —
they are not evidence the hardware behaves that way.

### 4.2 Leases, not allocations

A caller does not take a device. It asks for a lease, and the lease is
revocable. `hadald` holding VRAM is fine until the compositor needs it, and
"until" has to be expressible or the reservation is decoration.

Two measurements since narrowed what a lease is *for*. §3.2a: it must count
bytes **touched**, not bytes allocated, because an untouched allocation is not
charged and a lease counting allocations measures a quantity the kernel
ignores. §4.3b: on this hardware it is not needed to protect frame deadlines
at all, so v1's lease is a memory-admission check and nothing else.

### 4.3 The invariant

Stated the way tier-routing.md §4 states its own, because this is the one
worth a test:

> No compute lease may cause the compositor to miss a frame deadline. Under
> contention the display reservation is not preemptible, not negotiable, and
> not subject to a fallback path that quietly spends it.

**Status: measured and unviolated on the reference laptop — see §4.3b.** It is
kept as the invariant because it states the property that matters, and because
§4.3b's margin is a property of 60 Hz and of Intel's GuC rather than of
anything this project controls. What changed is that enforcing it is not v1
work.

Which mirrors, deliberately, the two decisions this project already made in
the same shape: Limine never garbage-collects the kernel `lastgood` points
at, and a local-only prompt refuses rather than substituting a remote model.
When the safe action is unavailable, decline — do not substitute.

ARCHITECTURE.md §0 already raised compositor error handling from *should* to
*must*, on the grounds that a compositor crash takes every client with it.
A compute layer that can stall the compositor is the same blast radius
reached by a different route.

### 4.3a How it cannot be implemented

The obvious implementation of §4.3 is queue priority: put compute on a
low-priority compute queue and let the hardware scheduler prefer the graphics
queue. **That is not available on the reference laptop.** §3.1a measured one
queue family with one queue, carrying graphics, compute and transfer
together. There is no second queue to demote.

Worse, the lever is in the wrong place even where a second queue exists. Two
processes each create their own `VkDevice` and their own queue from the same
family, and arbitration between them happens in the kernel — i915's GuC
scheduler here — not in any userspace component. Once a command buffer is
submitted, `sonar` has no way to take it back.

So the reservation has to be **admission control before submit**: a lease is
permission to submit, checked by the client, and the enforcement point is
that `hadald` asks before dispatching rather than that `sonar` interrupts it
afterwards. That is weaker than it sounds only if the caller is untrusted —
and the caller here is `hadald`, a component the project already wrote to be
resource-capped by policy it does not enforce itself.

It does mean the invariant in §4.3 is **cooperative, not mandatory**, and
saying so is the point. A cooperative invariant that is documented as
mandatory is exactly the class of claim this project keeps finding in its own
notes. Whether a discrete GPU with real compute queues can make it mandatory
is a question for the tower in §6, not an assumption to build on.

### 4.3b Measured, 2026-08-23 — and the reservation is not justified here

`src/sonar/probe/framejitter.c`. Two processes with separate `VkDevice`s, which
is what the real case looks like. The `display` role submits a frame-sized
dispatch on a 16.67 ms cadence and records submit → fence-signalled latency;
the `hog` role submits long dispatches back to back. Panel is eDP-1 at
1920x1080@60, so the budget is 16.67 ms.

Two contention modes, because they stress different hardware. `spin.comp`
saturates execution units and deliberately touches almost no memory;
`stream.comp` saturates DRAM bandwidth over a 512 MiB working set — well past
this part's 12 MiB LLC — because **LLM inference is bandwidth-bound**, and on
an iGPU that bandwidth is the same bus the display controller scans out of.

| scenario | mean | p99 | max | over 16.67 ms |
|---|---|---|---|---|
| display alone | 0.888 | 1.289 | 4.429 | 0 / 360 |
| + occupancy hog (71 ms dispatches) | 1.416 | 2.610 | 3.630 | 0 / 360 |
| + bandwidth hog (~29 GB/s) | 1.282 | 2.212 | 4.227 | 0 / 360 |
| heavy display alone | 1.911 | 2.736 | 3.629 | 0 / 300 |
| heavy display + occupancy hog | 2.658 | 5.271 | 5.664 | 0 / 300 |

**§4.3's invariant was not violated once in 1,380 samples.** A 71 ms dispatch
does not block a sub-millisecond one: GuC preempts it. Contention costs a
roughly *fixed* +1.3 to +2.5 ms at p99, independent of how large the frame-sized
job is — which reads as preemption granularity rather than queueing, and is the
mechanism §4.3a said was in the kernel, working.

**This is the finding, and it reduces scope.** The frame-deadline reservation in
§4.2 and §4.3 is **not justified by measurement on this hardware**. The threat
to the desktop from compute is not scheduling — §3.2a already found what it is.
A compute client that saturates the GPU costs the desktop about 2 ms of a 16.67
ms budget. A compute client that exceeds its *memory* budget loses the device
outright. So `sonar` v1's job is **memory admission, not frame reservation**,
and building the lease machinery for frame deadlines first would have been
solving the wrong problem carefully.

Four limits, because a measurement believed past its evidence is worse than
none:

1. **This is a proxy, not the compositor.** It measures compute-queue latency
   for frame-sized work. A real frame involves the graphics pipeline, buffer
   sharing and a page flip, and nothing here observes whether cusk presented on
   time. The honest version of that test needs a Wayland client using
   `wp_presentation` feedback, and cusk has never hosted a session anyway.
2. **The margin is refresh-rate dependent, and that is not a footnote.** At
   60 Hz, +2.5 ms is 15% of the budget. At 240 Hz the budget is 4.17 ms and the
   same +2.5 ms is 60% of it — the conclusion probably inverts. Any HadalOS
   machine with a high-refresh panel needs this re-run before §4.3 is treated
   as satisfied.
3. **Integrated Intel with GuC only.** AMD's hardware scheduler is a different
   implementation with different preemption granularity. The §6a tower and the
   eventual 9800X3D box must re-run this; the result here does not transfer.
4. **Untested: a graphics hog.** Every hog above is compute. The compositor's
   own workload class is graphics, and whether a long graphics submission
   preempts as well is a separate question with the same shape.

### 4.4 Make the dispatch visible

tier-routing.md §5 argues that a tier system which silently picks the remote
model is the silent-failure pattern again, in the component whose whole
justification is that you can see what it does. Compute inherits the
argument without modification: `sonar` reports which unit ran the work and
what it was denied, or it is a black box that happens to be ours.

---

## 5. First customer, and the first measurable claim

`hadald`. Not a benchmark, not a demo — the component that already exists and
already has a resource policy that stops at the bus.

llama.cpp's Vulkan backend is real, shipping, and vendor-neutral, and it runs
on both halves of the reference hardware. That makes the v1 claim narrow
enough to be false:

> The reflex model runs on the GPU with no vendor blob, and the desktop does
> not stutter while it does.

Both halves are measurable, and the second one is the one nobody else ships.
It also retires the gap in §3.2 for the workload that motivated the RAM floor
in ARCHITECTURE.md §2.7 — a distribution that needs 20 GB free for its
assistant is one nobody can run, and the same reasoning applies to VRAM with
less headroom, not more.

Note what this does *not* need: no BLAS, no cuDNN, no compiler. llama.cpp
brings its own kernels. The §3.1 gap is real and does not block v1.

### 5a. What landed, and what the claim still needs

**Landed 2026-08-23, in `src/hadald`.** The observation that made it small:
`llama-server` speaks the same OpenAI-compatible API as the remote endpoint, so
hosting a model locally is a change of *address*, not of protocol. hadald
already had `--upstream` as a parameter. What it did not have was any notion of
whether that address is on this machine — and three things were unconditionally
wrong as a result:

| | before | now |
|---|---|---|
| API key | `read_key` refused to start without one | not required when local |
| egress log | a line per request, whatever the destination | remote only |
| startup warning | "sends system logs to a third party", always | remote only |

`Locality` is derived from the upstream URL, never from a flag, because a flag
can disagree with the URL and the URL is what decides where bytes go — the same
choice `contends_with_display` makes in §4.1b. Unparseable, or a hostname that
merely *resolves* to loopback, is classified **remote**: that costs a needless
key and a needless log line, where guessing local would suppress the record of
a real egress. Wrong in the harmless direction, deliberately.

The egress bug is the one worth naming, because it had not been written yet and
would have been invisible once it was. `/var/log/hadal/egress.log` answers
"what left this machine". Pointed at loopback, the old code would have written
a line per local request — a log that answers a different question than its
name, quietly. Verified both ways: an identical `/api/generate` request writes
nothing when local and a full line when remote, and `note_egress` runs *before*
the upstream call, so this holds even with nothing listening.

`/api/tags` and the startup line now report the tier, which is
`docs/tier-routing.md` §5's requirement that a reader be able to tell whether
an answer came from here or from someone else's datacentre.

**This also unblocks tier-routing.** That document routes on whether data must
stay here; it could not route anywhere, because hadald had exactly one place to
route to. Now there are two. The *policy* is still unwritten — this is the
mechanism only.

**What the §5 claim still needs, stated plainly.** Nothing above runs a model.
This machine has no `llama.cpp`, no `llama-server`, and **no model weights of
any kind** — `ollama` is installed with an empty blob store. So:

> The reflex model runs on the GPU with no vendor blob, and the desktop does
> not stutter while it does.

is exactly as unverified as it was this morning. The first half needs llama.cpp
built with its Vulkan backend and a 1–3B GGUF fetched; the second half is
`framejitter`'s methodology (§4.3b) pointed at a real inference load instead of
a synthetic one — and §4.3b's bandwidth case is the relevant one, since
inference is bandwidth-bound.

Recording the distinction because this document's neighbours did not: the
greeter row in ARCHITECTURE.md §0 and "the desktop is installed, nobody has
logged into it" in host-conversion.md are the same error caught twice.
`hadald` can now *address* a local model. There is no local model.

### 5b. Packaging the runtime, 2026-08-24

`sci-ml/llama-cpp-0.2.0` in `::hadalos`, plus `hadal-model.service`, plus a
`local-model` USE flag on `sys-apps/hadald` that installs the unit and pulls
the runtime. **Written and resolving; not yet merged.** host-conversion.md's
own tally is the reason that distinction is worth keeping: four bugs in the
last conversion, "found by merging rather than by reading."

**Two upstream defaults that an ebuild must override.**

`LLAMA_BUILD_UI` and `LLAMA_USE_PREBUILT_UI` both default ON, and the second
fetches a prebuilt bundle from a HuggingFace bucket *during the build*.
Portage's sandbox forbids that, and an ebuild that reaches the network
mid-build is not reproducible where it is permitted either. Both forced OFF —
and the Web UI is not wanted regardless: the server is loopback-only inside
hadald's network namespace, and `hadald` is its only client.

`GGML_NATIVE` defaults ON, adding `-march=native`. ARCHITECTURE.md §0 plans a
binhost; the build host is Zen 5 and the reference laptop is Alder Lake, so
that default produces packages which SIGILL on the target — a runtime failure,
not a merge failure. Forced OFF.

**`system-ggml` is pinned strict and is unsatisfiable today, on purpose.**
llama.cpp v0.2.0 vendors ggml 0.21.0; ::gentoo carries 0.19.0 and 0.20.0.
Upstream's `find_package(ggml REQUIRED)` has no version constraint, so an older
ggml fails as a link error deep into a long build rather than as a refusal.
Pinning `>=sci-ml/ggml-0.21.0` makes the flag either correct or unavailable.
Worth revisiting when ::gentoo catches up, because this machine already carries
`libggml-vulkan.so` and a second copy is duplication.

**On not using ::gentoo-zh.** That overlay already packages llama.cpp, and
competently — it gets `GGML_NATIVE=OFF` right and handles the Web UI more
neatly than this ebuild does, as a distfile rather than a disable. The reason
to carry our own anyway is that a distribution cannot instruct its users to add
a third-party overlay to install its own assistant. Both ::gentoo-zh ebuilds
are unkeyworded, so ordinary resolution picks ours and nothing needs masking.
If ::gentoo ever ships llama.cpp, ours should be deleted rather than kept in
step.

### 5b-bis. What merging found, 2026-08-24

It compiled. It did not install, and it took two bugs down with it — which is
the argument for §5b's caveat, made concrete on the same day it was written.

**1. The vendored ggml collides with `sci-ml/ggml`, and cannot be made to
coexist.** Portage refused the merge on 27 colliding files: every `ggml-*.h`,
`libggml*.so`, and `ggml-config.cmake`. `ggml/CMakeLists.txt:348` is a bare
`install(TARGETS ggml LIBRARY PUBLIC_HEADER)` with no option to suppress it,
and the CMake package files at `:417` are unconditional too.

The collision is the *good* outcome. What makes it interesting is what
"fixing" it naively would produce:

```
ggml 0.19.0 (::gentoo)  SONAME = libggml.so.0
ggml 0.21.0 (vendored)  SONAME = libggml.so.0
```

Same SONAME, two minor versions apart, on a fast-moving C API. Delete our
`.so` and keep theirs and `llama-server` carries a `DT_NEEDED` on
`libggml.so.0` that resolves to the *older ABI at runtime* — a crash or silent
corruption instead of a merge-time error. Install ours over theirs and every
other ggml consumer breaks the same way. A blocker would be wrong too:
`sci-ml/ggml` is a legitimate package other things link.

So shared coexistence is impossible, and the fix is `BUILD_SHARED_LIBS=OFF`
plus pruning the ggml interface from the image. ggml ends up inside the llama
binaries, nothing is exported, and whether `sci-ml/ggml` is installed stops
mattering. Costs paid knowingly: larger binaries, and no `libllama` for
external consumers — HadalOS has none, since hadald speaks HTTP and links
nothing. `src_install` also fails loudly if a future release grows another
exported ggml artifact, rather than deferring it to someone else's merge.

Worth noting ::gentoo-zh's ebuild has the same latent collision — no blocker,
no `src_install` override. It simply is not hit unless `sci-ml/ggml` happens to
be installed, which on this machine it is (an orphan, not in `@world`, nothing
depending on it — left over from an ollama attempt).

**2. The flagship feature failed to record the failure.** Buried in the
sandbox summary:

```
ACCESS DENIED  open_wr_creat: /var/log/portage/hadalos/…llama-cpp-0.2.0.log
ACCESS DENIED  open_wr_creat: /var/lib/hadalos/build-failures/…json
C: bash /usr/lib/portage/python3.14/misc-functions.sh die_hooks
```

ARCHITECTURE.md §2.5 calls the Portage build-failure hook "the flagship
feature". `EBUILD_DEATH_HOOKS` run **inside the sandbox**, so writes outside
the build tree are denied unless declared with `addwrite` — and
`10-hadalos.bashrc` never called it. The first real build failure this hook
ever saw was not recorded.

The mechanical fix is two lines. The interesting part is why nobody knew: the
hook silences stderr on every path and returns 0 unconditionally, so a failed
capture printed **nothing at all**. That defensiveness is correct in intent — a
bug in the assistant must not turn a failing emerge into a differently-failing
one — but total silence made `hadal explain` finding no recorded failure
indistinguishable from there having been no failure. The hook whose job is
catching failures failed silently.

Both halves are fixed: `addwrite` for the two directories, and a one-line
report when capture fails, naming the reason. Plus an end-state assertion —
an empty or missing record is a failure *even when every command returned 0*,
which is the check that would have caught this the day it was introduced.

Three tests added to `scripts/test-portage-hook.sh`, and verified to fail
against the pre-fix hook rather than merely pass against the fixed one: the
sandbox declaration, the failure being reported, and an unwritten record
counting as failure. 17 passed, 0 failed; 14 passed, 3 failed against the old
version.

### 5b-ter. Attempt 2, and two lessons about the harness rather than the code

The static-linkage fix was right and did not get tested, because of two
mistakes in how the merge was run. Both are worth recording: neither was in
the ebuild, and both produced output that looked like the ebuild's fault.

**`FEATURES="keepwork"` made the second attempt a re-run of the first.** I had
suggested it so a failure would leave the build tree readable. It also
preserves Portage's completed-phase markers, so the re-emerge **skipped
`src_configure`** and installed the image from attempt 1's shared-library
build. `-DBUILD_SHARED_LIBS=OFF` never ran.

The tell was in plain sight and I nearly missed it: the failure log carried
attempt 1's timestamp. `20260824-163130.log` for a run that failed at 164538.

The guard added in §5b-bis did its job exactly — it caught twelve exported
`libggml*.so.0.21.0` files and refused rather than shipping them. So the
belt-and-braces check earned its place on the first attempt after being
written, which is a better outcome than the merge succeeding would have been.

Separately verified that the flag itself is fine, without a 30-minute build: a
clean configure with `BUILD_SHARED_LIBS=OFF` yields
`libggml{,-base,-cpu,-vulkan}.a` and **no `.so` at all**.

**The hook fix was in the repo and not on the machine.** The sandbox denial
recurred verbatim, because `/etc/portage/bashrc.d/10-hadalos.bashrc` on disk is
still the Aug 7 copy with zero `addwrite` calls. Re-merging
`app-admin/hadalos-portage-hook` does not fix it either: `/etc` is
config-protected, so the new file lands as `._cfg0000_10-hadalos.bashrc` and
waits — §5 of installer.md is about exactly this, and host-conversion.md §1
records it happening to `/etc/os-release`.

**That makes three installed-vs-source drifts found in one session**, all the
same shape:

| file | state |
|---|---|
| `/etc/portage/package.accept_keywords/hadalos` | stale, missing the new package |
| `/etc/os-release` | reverted to baselayout's symlink; machine reports Gentoo |
| `/etc/portage/bashrc.d/10-hadalos.bashrc` | Aug 7 copy, no sandbox fix |

`scripts/test-overlay.sh` catches the second and is why it is known. The other
two had nothing asserting them. That is the argument for its "installed state
matches what the ebuilds intend" section growing rather than the section being
treated as done — and for `verify()` in installer.md §5 checking the end state
on disk rather than the exit status of a merge.

Note also that resolving these must stay *scoped*. Running `etc-update` to pick
up the hook would also offer to revert `/etc/os-release` to baselayout's
symlink, which is the fight this machine has already lost once. The merge
script applies the one `._cfg` file it means to and leaves the rest alone.

### 5e. §5 measured, 2026-08-24 — and the GPU turns out to be the wrong answer here

Built statically outside Portage to validate the ebuild's configuration before
spending another merge on it. That worked — `libggml{,-base,-cpu,-vulkan}.a`,
no `.so`, and `llama-server` linking `libvulkan.so.1` with no `libggml`
dependency at all.

**First half of the claim: verified.**

```
ggml_vulkan: Found 1 Vulkan devices:
  Intel(R) Iris(R) Xe Graphics (ADL GT2) (Intel open-source Mesa driver)
  uma: 1 | fp16: 1 | warp size: 32 | shared memory: 49152 | matrix cores: none
```

Mesa ANV, in-tree, no vendor blob, running a real model. `uma: 1` independently
confirms §3.1a's unified-heap finding from a second source.

**And then the numbers said not to use it.** Qwen3-1.7B Q8_0:

| config | pp512 (prompt) | tg128 (generation) |
|---|---|---|
| CPU, 8 threads | 220.46 | **16.22** ± 3.07 |
| CPU, 12 threads | — | 14.84 ± 0.32 |
| GPU, `-ngl 99`, 4t | **319.85** | 13.17 ± 0.12 |
| CPU, 4 threads | 215.89 | 12.51 ± 0.41 |

GPU offload wins prompt processing by ~1.5x and **loses token generation to a
well-tuned CPU config by ~23%.**

This is §4.3b's bandwidth argument arriving at its destination. Generating one
token streams the whole 1.7 GiB of weights; with `uma: 1` the GPU and the CPU
draw on the same DRAM controller, so the GPU brings no extra bandwidth to a
bandwidth-bound problem. Prompt processing is compute-bound batch matmul, which
is why it is the half that improves.

**Which half matters depends on the tier, and ARCHITECTURE.md §2.7 already
assigned it.** The reflex model does the shell widget, settings search and
routing — short prompts, short outputs, token-generation dominated. The deep
tier, and the flagship Portage build-failure job, is the opposite: a long log
in, a short answer out, prompt-processing dominated. So the correct offload
setting is not a constant, it is **a function of the tier and the hardware**.

**Second half of the claim: also verified, and the CPU wins that too.**
`framejitter`'s §4.3b methodology, real inference instead of a synthetic hog:

| scenario | mean | p99 | max | over 16.67 ms |
|---|---|---|---|---|
| idle baseline | 0.887 | 1.058 | 1.865 | 0 / 360 |
| during GPU inference | 0.941 | 1.577 | 2.921 | 0 / 360 |
| during CPU inference (8t) | **0.600** | **0.804** | 2.909 | 0 / 360 |
| synthetic occupancy hog (§4.3b) | 1.416 | 2.610 | 3.630 | 0 / 360 |
| synthetic bandwidth hog (§4.3b) | 1.282 | 2.212 | 4.227 | 0 / 360 |

> The reflex model runs on the GPU with no vendor blob, and the desktop does
> not stutter while it does.

True. Zero overruns either way. Two things worth reading off the table beyond
that:

- **The synthetic hogs were harsher than the real workload.** §4.3b's numbers
  were conservative, which is the correct direction for a safety measurement to
  err in, and is worth knowing before anyone tunes against them.
- **CPU inference measures *better than idle*.** Not noise: it never touches
  the GPU, so the probe has the device to itself, and the CPU load raises
  clocks so the submit path itself runs faster. An assistant that is invisible
  to the display pipeline is a stronger result than one that merely does not
  break it.

**So on the reference laptop the reflex tier should run on the CPU**, and it is
not a close call. It is faster (16.22 vs 13.17 t/s), it is gentler on the
compositor (p99 0.804 vs 1.577), it cannot lose the Vulkan device the way
§3.2a measured, and it needs no render node — which lets `hadal-model.service`
drop the one privilege that separates it from `hadald`.

**This does not generalise, and that is the point.** A discrete RX 9060 has
VRAM bandwidth far above system DDR, so offload should win there, and the
prompt-heavy deep tier may favour the GPU even here. `-ngl` is exactly the kind
of per-machine policy §2 said this component exists to decide: `sonar` already
distinguishes integrated from discrete (§4.1b) and already knows the CPU's core
classes (§4.1). The thread-count result is the same story — 8 threads beats 12,
because the E-cores stop helping once bandwidth saturates, which is the
asymmetry §4.1 built `CoreClass` to express.

Two measurements, one conclusion: **`sonar` should be choosing `-ngl` and
`-t`, and it now has both the inventory and the evidence to do it.**

### 5c. The device split, and a stale grant it exposed

Splitting the runtime out of `hadald` into `hadal-model.service` was not
tidiness. The runtime needs `/dev/dri/renderD128`; `hadald` sets
`PrivateDevices=yes` specifically so it cannot reach a device node, and that is
worth keeping — **the process holding the API key should not also hold the
GPU.** After the split, exactly one process has the render node and it is the
one with no credential.

Writing that unit turned up a grant in `hadald.service` that was wrong in three
ways and had never run:

| directive | problem |
|---|---|
| `DeviceAllow=/dev/kfd rw` | the ROCm compute device — §3.1 rejected ROCm, and it does not exist on the reference laptop |
| `DeviceAllow=char-drm rw` | not a node but the whole DRM char major, so it granted *every* DRM device including `card0`, the display path cusk owns |
| `SupplementaryGroups=video render` | `video` is not needed for compute, and `acct-user/hadal`'s ebuild already states the intent that device access be granted by the unit that needs it |

The comment above them described "an allowlist of two nodes rather than a
blanket unmask". The second directive was a blanket unmask. All three are
removed from `hadald.service`; `hadal-model.service` takes `renderD128` and
nothing else.

**The unit's isolation is now coherent for the first time.**
`hadald.service` carries `PrivateNetwork=yes` and `IPAddressDeny=any` while the
daemon's default upstream is a public HTTPS endpoint — the unit and the daemon
flatly disagreed, and the unit had never been started, so nobody found out. A
local upstream in a joined namespace is the configuration those two directives
were always describing. `hadal-model.service` uses
`JoinsNamespaceOf=hadald.service`, the same mechanism `hadal-brokerd.service`
already uses, so `127.0.0.1:8080` means *that* loopback and the runtime is
reachable by hadald and by nothing else — enforced by the kernel rather than by
a bind address.

Consequence, stated because it is real: hadald owns the namespace, so it starts
first and answers 502 until the runtime is listening. Better than making the
API surface depend on a loaded model.

**The memory ceiling moved with the model.** ARCHITECTURE.md §2.2 puts
`MemoryMax=` on `hadald` so the assistant "can never starve the desktop". Since
§3.2a established that GPU-resident pages *are* charged to the allocating
process's cgroup, that cap governs nothing once the model lives elsewhere. The
ceiling is now on `hadal-model.service` and is sized generously rather than
tightly — §3.2a's measurement is that too tight does not mean slow, it means
`VK_ERROR_DEVICE_LOST` and a dead Vulkan device.

### 5d. Weights

Qwen3-1.7B, Q8_0, **Apache-2.0** — chosen on licence first. HadalOS is a
distribution, so the model named in its docs carries legal shape: Llama 3.2 and
Gemma ship under custom non-OSI licences with acceptable-use policies and
redistribution conditions, which is a packaging problem the project does not
need. Apache-2.0 or MIT avoids it.

Q8_0 rather than Q4_K_M because the **official** `Qwen/Qwen3-1.7B-GGUF` repo
publishes only Q8_0. The alternative was a third-party requant, and for a
distribution that cares about provenance an official artefact beats a smaller
one from a stranger. If Q4_K_M is wanted later, `llama-quantize` — built by
this ebuild's `tools` flag — can produce it locally and auditably, which is a
better answer than trusting someone else's conversion.

Fetched and verified 2026-08-24: 1,834,426,016 bytes, GGUF v3, 310 tensors,
28 KV pairs, `sha256:061b54daade076b5…`. Living in
`~/.local/share/hadal/models` for now; `/var/lib/hadal/models` is the
packaged location, per `hadald.service`'s existing `HADAL_MODELS`.

**Not packaged as an ebuild, and that is a decision.** Model weights are data,
not software: multi-gigabyte distfiles, a quantisation matrix rather than a
version, and licences that vary per model. `sci-ml/ollama` does not package
weights either. `sci-ml/llama-cpp` is therefore deliberately *not* a dependency
of `app-misc/hadalos-assistant` — a runtime with no weights cannot answer
anything, so making it a hard dependency would force a long build for a
component that cannot yet do its job.

---

## 6. Order of work

1. ~~Inventory only. `sonar` enumerates and prints.~~ **Done on this laptop,
   2026-08-23** — `src/sonar`, no dependencies, 10 tests. Plus §3.1a, which
   verified the execution layer rather than assuming it.

   Every finding so far comes from **one integrated GPU**. The AMD/CCX half of
   §4.1 and the whole `DiscreteGpu` path are written and unobserved, which is
   the state §4.1 penalised the first version of that enum for. Note that
   ARCHITECTURE.md §0 lists a 9800X3D build host that **does not exist yet**;
   the second machine actually available is a Quadro K2200 tower on CachyOS,
   and what it can and cannot answer is §6a.
2. ~~Measure the contention that §4.3 asserts.~~ **Done, 2026-08-23** —
   §4.3b. The invariant was never violated: 0 overruns in 1,380 samples under
   both execution and bandwidth saturation. It cost about +2 ms of p99 against
   a 16.67 ms budget.
3. ~~Leases and the display reservation.~~ **Reordered by what step 2 found.**
   The display reservation is not justified on this hardware, and §3.2a
   identified the real hazard. So step 3 becomes **memory admission**: a lease
   denominated in bytes *touched* (§3.2a consequence 2), checked before
   allocation against the client's own cgroup budget. The frame reservation
   stays designed and unbuilt, with §4.3b's four limits as the conditions that
   would revive it — a high-refresh panel being the likeliest.
4. `hadald` over Vulkan, against the §5 claim. **Started 2026-08-23 — the
   plumbing exists, the claim is not verified.** See §5a.

Steps 1 and 2 were cheap and they did their job: between them they falsified
one type (§4.1), one attribute (§4.1a), one implementation strategy (§4.3a),
one premise (§3.2a) and one entire work item (step 3 above). That is the
argument for measuring before building, and it is the only reason this
document is worth anything.

## 6a. The Quadro K2200, and what it is good for

The available second machine is a Quadro K2200 (GM107, Maxwell, 4 GB GDDR5)
in a tower running CachyOS. It is worth running `sonar` and `vkprobe` on, and
it is worth being precise about which question that answers, because the
tempting reading — "this is how we test NVIDIA support" — is the wrong one.

**What it answers, and nothing else does.** It is a **discrete** GPU. Every
number in §3.1a and §4.1b came from an iGPU, and two derivations have never
been exercised: a distinct device-local heap that is not host-visible, and
whether real compute queues exist separately from the graphics queue. The
second one decides §4.3a — whether the display reservation can ever be
mandatory rather than cooperative. That is the most valuable open question in
this document and the K2200 can speak to it.

**The bug it would have found is already fixed.** `Gpu::is_integrated` derived
discreteness from a dedicated-memory attribute under a per-driver name —
`mem_info_vram_total` (amdgpu) or `lmem_total_bytes` (i915/xe). **Nouveau
exposes neither**, so `sonar` as first written reported a K2200 as
*integrated*, with a confident one-line answer: §4.1a's failure mode in code
this document had just congratulated itself for avoiding. §4.1b now has three
states and a nouveau regression test. Running it on the tower is still worth
doing — the fixture proves the code's logic, not that nouveau's sysfs looks
the way the fixture says.

**What it does not answer: whether HadalOS should support NVIDIA.** Maxwell
is past the end of NVIDIA's proprietary support — the 580 branch dropped
Maxwell, Pascal and Volta, so it needs the 570 legacy branch. On the open
side, Mesa's NVK targets Turing and later, so a nouveau-driven K2200 is
unlikely to present a working Vulkan device at all. Either way the machine
demonstrates the exact cost ARCHITECTURE.md §0 cited when it chose to ride
mainline tip: "there is no proprietary NVIDIA module to lag behind the
kernel." Putting a legacy-branch out-of-tree module on a mainline-tip kernel
is that fight, and running the experiment is a good way to *document* the
cost rather than a step toward paying it.

So: run it for the discrete-GPU data, expect the nouveau misclassification,
and treat any CUDA result as a measurement of a decision already made in §1
rather than a reopening of it.

---

## Open

- Where does the policy live? tier-routing.md asks this about routing and
  answers "the broker, because it has the security boundary." Compute has no
  security boundary — it is arbitration, not authorization. That argues for a
  separate daemon, but two policy stores is the two-lists failure
  `cusk::config` exists to prevent.
- Is an NPU a `Unit` at all in v1? The reference hardware has none. Listing
  it in the enum now risks a branch nobody can test; omitting it means the
  closed set reopens later.
- What is the frame-deadline signal? §4.3 needs the compositor to say "I am
  about to need the GPU," and cusk currently says nothing to anyone.
- Does `sonar` gate escalation the way tier-routing.md §Open notes nothing
  currently gates it? An escalated agent with compute access is a third
  trust model again.
- ~~**Are DRM/GEM allocations charged to the calling process's memory
  cgroup?**~~ **Answered 2026-08-23 — yes, on integrated.** See §3.2a. The
  cap holds; the failure mode is `VK_ERROR_DEVICE_LOST` rather than reclaim,
  which moved the problem from accounting to enforcement granularity and
  changed what a lease has to count. Still open for **discrete** VRAM, where
  the answer probably inverts.
- Does `hadald` size its GPU offload to its cgroup budget, or catch
  `DEVICE_LOST` and retry smaller? §3.2a makes this a real choice rather than
  defensive coding — the first is checkable before anything runs, the second
  is the only option if the budget can change under it.
- ~~A third state for `Gpu::is_integrated`.~~ **Done** — §4.1b, with a nouveau
  fixture test.
- Should `Unknown` be *reported* differently from a routing standpoint, not
  just treated conservatively? It currently contends, which is safe and also
  means an unclassifiable discrete GPU is permanently derated. On a machine
  whose only GPU is `Unknown` that is the whole compute capacity.
