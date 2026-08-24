# probes

Three questions `sonar` cannot answer by reading sysfs, each with its own
program because each is its own claim.

| probe | question | answered in |
|---|---|---|
| `vkprobe` | does Vulkan compute actually dispatch here? | compute.md §3.1a |
| `gemcharge` | are GPU allocations charged to the memory cgroup? | compute.md §3.2a |
| `framejitter` | does a compute hog delay frame-cadence work? | compute.md §4.3b |

All three are C, link the system Vulkan loader against the system headers, and
add nothing to the repo's dependency set — which is the property under test.
Shaders are built with `glslc` (`dev-util/shaderc`); no Vulkan SDK.

```sh
glslc -fshader-stage=comp double.comp -o double.spv
glslc -fshader-stage=comp fill.comp   -o fill.spv
glslc -fshader-stage=comp spin.comp   -o spin.spv
glslc -fshader-stage=comp stream.comp -o stream.spv
for p in vkprobe gemcharge framejitter; do cc -O2 -Wall $p.c -lvulkan -lm -o $p; done
```

## gemcharge

```sh
systemd-run --user --scope -p MemoryMax=4G -q -- ./gemcharge 1024 gpu ./fill.spv
```

Modes `none|cpu|gpu` differ in who faults the pages in, which turns out to be
the whole question. Run under a *low* cap to see enforcement rather than
accounting — on this laptop `gpu` mode then returns
`VK_ERROR_DEVICE_LOST` rather than degrading.

## framejitter

Two processes, because two processes with separate `VkDevice`s is what the real
case looks like.

```sh
./framejitter hog 12 ./spin.spv &          # execution-unit saturation
./framejitter display 6 ./spin.spv         # frame-cadence latency

./framejitter hog 14 ./stream.spv 512 1 &  # DRAM bandwidth saturation
./framejitter display 6 ./spin.spv
```

`stream.comp` needs a working set larger than the LLC (12 MiB here) or it
measures cache rather than DRAM — hence the 512 MiB argument. The `display`
role's output is **a proxy and says so**: it is compute-queue latency for
frame-sized work, not evidence about what the compositor presented.

---

## vkprobe

Does Vulkan compute actually dispatch on this machine?

`docs/compute.md` §3.1 picks Vulkan compute as HadalOS's execution layer.
`sonar` reports Vulkan ICDs as *installed, not verified loadable*, because a
JSON file in `/usr/share/vulkan/icd.d` is not evidence a shader ran — this
laptop ships `radeon_icd` with no AMD GPU in it. This closes that gap by
enumerating and then **dispatching**, and checking the results came back
correct.

C rather than Rust on purpose. It links the system loader against the system
headers and adds nothing to the repo's dependency set, which is the property
under test. Promoting it to `sonar --probe-vulkan` — the idiom
`cusk --probe-drm` already established — means taking a Vulkan binding
(`ash`) as sonar's first dependency, and that is a decision to make
deliberately rather than by drift.

## Build and run

Needs `dev-util/vulkan-headers`, `media-libs/vulkan-loader`, and `glslc`
(`dev-util/shaderc`). No Vulkan SDK.

```sh
glslc -fshader-stage=comp double.comp -o double.spv
cc -O2 -Wall vkprobe.c -lvulkan -o vkprobe
./vkprobe double.spv
```

Exit 0 means a shader ran and every element came back correct. Any failure
mode — no devices, no compute queue, no host-visible memory, a timeout, or a
wrong result — exits 1 with the reason.

## Why it needs to run on the 9800X3D box too

Every finding recorded in `compute.md` §3.1a came from one integrated GPU.
The two that most need a second data point:

- **Queue topology.** This machine has a single queue family with a single
  queue doing graphics *and* compute. Whether a discrete RDNA4 part exposes
  separate compute queues decides whether §4.3's display reservation can be
  expressed as queue priority or must be admission control.
- **Heap layout.** One unified host-visible device-local heap here. A
  discrete card should show a distinct device-local heap, which is the case
  `Gpu::is_integrated` in `src/sonar/src/gpu.rs` derives but has never
  observed.
