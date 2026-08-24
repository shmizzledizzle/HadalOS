// framejitter — does a compute hog delay short, frame-cadence GPU work?
//
// docs/compute.md §6 step 2: "Measure the contention that §4.3 asserts. An
// invariant nobody has observed being violated is a guess."
//
// §3.1a measured one queue family with one queue carrying graphics and compute
// together, and §4.3a concluded that arbitration therefore happens in the
// kernel's GuC scheduler rather than in any userspace component. That leaves
// one question that decides how strong a lease has to be:
//
//   When a long compute dispatch is running, does a short one submitted from
//   another process wait for it, or does the scheduler interleave them?
//
// If it interleaves at fine granularity, contention is mild and admission
// control can be coarse. If a short job waits behind a long one, then a lease
// must bound *dispatch duration*, not just memory — and that is a much more
// intrusive contract for a compute client to honour.
//
// Two roles, run as separate processes, because two processes with separate
// VkDevices is what the real case looks like:
//
//   display  submits a small dispatch on a 16.6 ms cadence and records
//            submit -> fence-signalled latency. This stands in for the
//            compositor. It is a proxy and is labelled as one: it measures
//            GPU queue latency for frame-sized work, not whether cusk missed
//            a frame.
//   hog      submits long dispatches back to back.
//
// cc -O2 -Wall framejitter.c -lvulkan -lm -o framejitter
// ./framejitter <display|hog> <seconds> [spv] [buffer_MiB] [iters]
//
// The buffer size matters for the bandwidth case: this iGPU has a 12 MiB LLC,
// so a working set below that measures cache, not DRAM.

#define _POSIX_C_SOURCE 200809L
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <vulkan/vulkan.h>

// Frame-sized work: small grid, few iterations. Tuned below 1 ms on this iGPU
// so that any latency above the cadence is contention, not our own cost.
#define DISPLAY_GROUPS 64
#define DISPLAY_ITERS 256

// Hog work: enough to occupy the GPU for several milliseconds per dispatch,
// which is the interesting case — a dispatch longer than a frame interval.
#define HOG_GROUPS 4096
#define HOG_ITERS 4096

#define FRAME_NS 16666667L  // 60 Hz
#define MAX_SAMPLES 100000
#define BUF_ELEMS (1024 * 1024)

#define CHECK(expr, what)                                                      \
    do {                                                                       \
        VkResult _r = (expr);                                                  \
        if (_r != VK_SUCCESS) {                                                \
            fprintf(stderr, "FAIL %s: VkResult %d\n", (what), (int)_r);        \
            return 1;                                                          \
        }                                                                      \
    } while (0)

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int cmp_ll(const void *a, const void *b) {
    long long x = *(const long long *)a, y = *(const long long *)b;
    return (x > y) - (x < y);
}

static double pct(long long *sorted, int n, double p) {
    if (n <= 0) return 0.0;
    double idx = p / 100.0 * (n - 1);
    int lo = (int)idx;
    int hi = lo + 1 < n ? lo + 1 : n - 1;
    double frac = idx - lo;
    return (sorted[lo] * (1.0 - frac) + sorted[hi] * frac) / 1e6;  // ms
}

static char *read_spirv(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); return NULL; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (n <= 0 || n % 4) { fclose(f); return NULL; }
    char *b = malloc((size_t)n);
    if (fread(b, 1, (size_t)n, f) != (size_t)n) { fclose(f); free(b); return NULL; }
    fclose(f);
    *len = (size_t)n;
    return b;
}

struct push { uint32_t n, iters; };

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <display|hog> <seconds> [spin.spv]\n", argv[0]);
        return 2;
    }
    int is_display = !strcmp(argv[1], "display");
    if (!is_display && strcmp(argv[1], "hog")) {
        fprintf(stderr, "unknown role: %s\n", argv[1]);
        return 2;
    }
    double seconds = atof(argv[2]);
    const char *spv = argc > 3 ? argv[3] : "spin.spv";
    long buf_mib = argc > 4 ? strtol(argv[4], NULL, 10) : 4;
    long iters_override = argc > 5 ? strtol(argv[5], NULL, 10) : 0;
    if (seconds <= 0) { fprintf(stderr, "seconds must be > 0\n"); return 2; }
    if (buf_mib <= 0) { fprintf(stderr, "buffer_MiB must be > 0\n"); return 2; }
    uint32_t buf_elems = (uint32_t)(buf_mib * 1048576 / 4);

    // -------- Vulkan setup

    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "framejitter",
        .apiVersion = VK_API_VERSION_1_1,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance inst;
    CHECK(vkCreateInstance(&ici, NULL, &inst), "vkCreateInstance");

    uint32_t ndev = 0;
    CHECK(vkEnumeratePhysicalDevices(inst, &ndev, NULL), "enumerate");
    if (!ndev) { fprintf(stderr, "FAIL no devices\n"); return 1; }
    VkPhysicalDevice *devs = calloc(ndev, sizeof *devs);
    CHECK(vkEnumeratePhysicalDevices(inst, &ndev, devs), "enumerate");
    VkPhysicalDevice pd = devs[0];

    uint32_t nqf = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, NULL);
    VkQueueFamilyProperties *qfs = calloc(nqf, sizeof *qfs);
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qfs);
    uint32_t qf = UINT32_MAX;
    for (uint32_t i = 0; i < nqf; i++)
        if (qfs[i].queueFlags & VK_QUEUE_COMPUTE_BIT) { qf = i; break; }
    if (qf == UINT32_MAX) { fprintf(stderr, "FAIL no compute queue\n"); return 1; }

    // Priority is set here and is the only knob Vulkan offers. Whether it does
    // anything on a single-queue family is part of what this measures — see the
    // note printed at the end of a display run.
    float prio = is_display ? 1.0f : 0.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qf, .queueCount = 1, .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci,
    };
    VkDevice dev;
    CHECK(vkCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice");
    VkQueue queue;
    vkGetDeviceQueue(dev, qf, 0, &queue);

    // -------- buffer

    VkDeviceSize bytes = (VkDeviceSize)buf_elems * 4;
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = bytes,
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bci, NULL, &buf), "vkCreateBuffer");

    VkMemoryRequirements mr;
    vkGetBufferMemoryRequirements(dev, buf, &mr);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    uint32_t mt = UINT32_MAX;
    for (uint32_t t = 0; t < mp.memoryTypeCount; t++)
        if ((mr.memoryTypeBits & (1u << t)) &&
            (mp.memoryTypes[t].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
            { mt = t; break; }
    if (mt == UINT32_MAX) { fprintf(stderr, "FAIL no device-local memory\n"); return 1; }

    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = mr.size, .memoryTypeIndex = mt,
    };
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mai, NULL, &mem), "vkAllocateMemory");
    CHECK(vkBindBufferMemory(dev, buf, mem, 0), "bind");

    // -------- pipeline

    size_t slen = 0;
    char *code = read_spirv(spv, &slen);
    if (!code) { fprintf(stderr, "FAIL cannot read %s\n", spv); return 1; }
    VkShaderModuleCreateInfo smci = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = slen, .pCode = (const uint32_t *)code,
    };
    VkShaderModule sm;
    CHECK(vkCreateShaderModule(dev, &smci, NULL, &sm), "shader module");

    VkDescriptorSetLayoutBinding b = {
        .binding = 0, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1, .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
    };
    VkDescriptorSetLayoutCreateInfo dslci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 1, .pBindings = &b,
    };
    VkDescriptorSetLayout dsl;
    CHECK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl), "dsl");

    VkPushConstantRange pcr = {
        .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT, .offset = 0,
        .size = sizeof(struct push),
    };
    VkPipelineLayoutCreateInfo plci = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1, .pSetLayouts = &dsl,
        .pushConstantRangeCount = 1, .pPushConstantRanges = &pcr,
    };
    VkPipelineLayout pl;
    CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &pl), "pipeline layout");

    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {
            .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
            .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "main",
        },
        .layout = pl,
    };
    VkPipeline pipe;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe), "pipeline");

    VkDescriptorPoolSize dps = {
        .type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 1,
    };
    VkDescriptorPoolCreateInfo dpci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps,
    };
    VkDescriptorPool dp;
    CHECK(vkCreateDescriptorPool(dev, &dpci, NULL, &dp), "descriptor pool");
    VkDescriptorSetAllocateInfo dsai = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = dp, .descriptorSetCount = 1, .pSetLayouts = &dsl,
    };
    VkDescriptorSet ds;
    CHECK(vkAllocateDescriptorSets(dev, &dsai, &ds), "descriptor set");
    VkDescriptorBufferInfo dbi = { .buffer = buf, .offset = 0, .range = bytes };
    VkWriteDescriptorSet w = {
        .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        .dstSet = ds, .dstBinding = 0, .descriptorCount = 1,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi,
    };
    vkUpdateDescriptorSets(dev, 1, &w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = qf,
        .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    };
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool), "command pool");
    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cbai, &cmd), "cmd buffer");
    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    VkFence fence;
    CHECK(vkCreateFence(dev, &fci, NULL, &fence), "fence");

    struct push pc = {
        .n = buf_elems,
        .iters = iters_override > 0 ? (uint32_t)iters_override
                                    : (is_display ? DISPLAY_ITERS : HOG_ITERS),
    };
    uint32_t groups = is_display ? DISPLAY_GROUPS : HOG_GROUPS;

    printf("role %s   groups %u   iters %u   buffer %ld MiB   for %.1fs\n",
           argv[1], groups, pc.iters, buf_mib, seconds);
    fflush(stdout);

    // -------- the loop

    long long *lat = malloc(sizeof(long long) * MAX_SAMPLES);
    int n = 0;
    long long t_end = now_ns() + (long long)(seconds * 1e9);
    long long next_tick = now_ns();

    while (now_ns() < t_end && n < MAX_SAMPLES) {
        VkCommandBufferBeginInfo cbbi = {
            .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
        };
        CHECK(vkResetCommandBuffer(cmd, 0), "reset cmd");
        CHECK(vkBeginCommandBuffer(cmd, &cbbi), "begin");
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
        vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1,
                                &ds, 0, NULL);
        vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_COMPUTE_BIT, 0,
                           sizeof pc, &pc);
        vkCmdDispatch(cmd, groups, 1, 1);
        CHECK(vkEndCommandBuffer(cmd), "end");

        CHECK(vkResetFences(dev, 1, &fence), "reset fence");
        VkSubmitInfo si = {
            .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
            .commandBufferCount = 1, .pCommandBuffers = &cmd,
        };

        long long t0 = now_ns();
        CHECK(vkQueueSubmit(queue, 1, &si, fence), "submit");
        VkResult wr = vkWaitForFences(dev, 1, &fence, VK_TRUE, 5000000000ull);
        long long t1 = now_ns();
        if (wr == VK_TIMEOUT) {
            fprintf(stderr, "FAIL fence timeout after 5s at sample %d\n", n);
            return 1;
        }
        CHECK(wr, "wait");

        if (is_display) {
            lat[n++] = t1 - t0;
            // Hold the cadence. Falling behind is itself a result, so the tick
            // is advanced rather than reset from "now" — otherwise a slow frame
            // would silently stretch the schedule and hide the overrun.
            next_tick += FRAME_NS;
            long long slack = next_tick - now_ns();
            if (slack > 0) {
                struct timespec ts = {
                    .tv_sec = slack / 1000000000LL,
                    .tv_nsec = slack % 1000000000LL,
                };
                nanosleep(&ts, NULL);
            } else {
                // Behind schedule: resync so the deficit is not carried
                // forward forever, but count it.
                next_tick = now_ns();
            }
        } else {
            lat[n++] = t1 - t0;
        }
    }

    // -------- report

    if (n == 0) {
        printf("no samples\n");
        return 1;
    }
    long long *sorted = malloc(sizeof(long long) * n);
    memcpy(sorted, lat, sizeof(long long) * n);
    qsort(sorted, n, sizeof(long long), cmp_ll);

    double sum = 0;
    int over = 0;
    for (int i = 0; i < n; i++) {
        sum += lat[i];
        if (lat[i] > FRAME_NS) over++;
    }

    printf("samples        %d\n", n);
    printf("mean           %.3f ms\n", sum / n / 1e6);
    printf("p50            %.3f ms\n", pct(sorted, n, 50));
    printf("p95            %.3f ms\n", pct(sorted, n, 95));
    printf("p99            %.3f ms\n", pct(sorted, n, 99));
    printf("max            %.3f ms\n", pct(sorted, n, 100));
    if (is_display) {
        printf("over 16.67ms   %d of %d  (%.1f%%)\n", over, n, 100.0 * over / n);
        printf("\nProxy, not the compositor: this is GPU queue latency for\n");
        printf("frame-sized work, submitted at frame cadence. It does not\n");
        printf("observe whether cusk presented on time.\n");
    }

    vkDestroyFence(dev, fence, NULL);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, dp, NULL);
    vkDestroyPipeline(dev, pipe, NULL);
    vkDestroyPipelineLayout(dev, pl, NULL);
    vkDestroyDescriptorSetLayout(dev, dsl, NULL);
    vkDestroyShaderModule(dev, sm, NULL);
    vkFreeMemory(dev, mem, NULL);
    vkDestroyBuffer(dev, buf, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    return 0;
}
