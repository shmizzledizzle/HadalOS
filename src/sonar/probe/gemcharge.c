// gemcharge — are DRM/GEM allocations charged to the calling process's memory
// cgroup?
//
// docs/compute.md §3.1a made this load-bearing rather than academic. The iGPU's
// 11.4 GiB Vulkan heap is device-local *and* host-visible: it is system RAM.
// ARCHITECTURE.md §2.2 promises that hadald's MemoryMax= means the assistant
// "can never starve the desktop." If GPU allocations escape that charge, the
// promise is not merely silent about VRAM — it is bypassable by offloading to
// the GPU, and nobody has noticed.
//
// Three modes, because "charged" depends on who faults the pages in:
//
//   none  allocate and bind, never touch          — is the reservation charged?
//   cpu   allocate, map, memset from the CPU      — control; should be charged
//   gpu   allocate, write from a compute shader   — the case that matters
//
// Reports the delta in memory.current against bytes allocated. A ratio near 1
// means cgroup accounting sees GPU memory. Near 0 means MemoryMax= does not.
//
// cc -O2 -Wall gemcharge.c -lvulkan -o gemcharge
// ./gemcharge <total_MiB> <none|cpu|gpu> [fill.spv]

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#define CHUNK_MIB 128

#define CHECK(expr, what)                                                      \
    do {                                                                       \
        VkResult _r = (expr);                                                  \
        if (_r != VK_SUCCESS) {                                                \
            fprintf(stderr, "FAIL %s: VkResult %d\n", (what), (int)_r);        \
            return 1;                                                          \
        }                                                                      \
    } while (0)

// ---------------------------------------------------------------- cgroup

static char cg_base[512];

// /proc/self/cgroup on cgroup v2 is a single line: "0::/path".
static int cgroup_init(void) {
    FILE *f = fopen("/proc/self/cgroup", "r");
    if (!f) return -1;
    char line[512];
    if (!fgets(line, sizeof line, f)) { fclose(f); return -1; }
    fclose(f);
    char *p = strstr(line, "0::");
    if (!p) return -1;
    p += 3;
    char *nl = strchr(p, '\n');
    if (nl) *nl = 0;
    snprintf(cg_base, sizeof cg_base, "/sys/fs/cgroup%s", p);
    return 0;
}

static long long cg_read(const char *attr) {
    char path[640];
    snprintf(path, sizeof path, "%s/%s", cg_base, attr);
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    long long v = -1;
    if (fscanf(f, "%lld", &v) != 1) {
        // memory.max reads "max" when unset.
        v = -2;
    }
    fclose(f);
    return v;
}

static double mib(long long bytes) { return bytes / 1048576.0; }

// ---------------------------------------------------------------- spirv

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

// ---------------------------------------------------------------- main

enum mode { M_NONE, M_CPU, M_GPU };

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <total_MiB> <none|cpu|gpu> [fill.spv]\n", argv[0]);
        return 2;
    }
    long total_mib = strtol(argv[1], NULL, 10);
    enum mode mode;
    if (!strcmp(argv[2], "none")) mode = M_NONE;
    else if (!strcmp(argv[2], "cpu")) mode = M_CPU;
    else if (!strcmp(argv[2], "gpu")) mode = M_GPU;
    else { fprintf(stderr, "unknown mode: %s\n", argv[2]); return 2; }
    const char *spv = argc > 3 ? argv[3] : "fill.spv";

    if (total_mib <= 0 || total_mib % CHUNK_MIB) {
        fprintf(stderr, "total_MiB must be a positive multiple of %d\n", CHUNK_MIB);
        return 2;
    }
    int nchunks = (int)(total_mib / CHUNK_MIB);

    if (cgroup_init() != 0) {
        fprintf(stderr, "FAIL cannot resolve own cgroup (v2 required)\n");
        return 1;
    }

    long long base = cg_read("memory.current");
    long long max = cg_read("memory.max");
    printf("cgroup   %s\n", cg_base);
    printf("mode     %s, %ld MiB in %d x %d MiB chunks\n",
           argv[2], total_mib, nchunks, CHUNK_MIB);
    if (max == -2)
        printf("limit    memory.max = max (unlimited)\n");
    else
        printf("limit    memory.max = %.0f MiB\n", mib(max));
    printf("baseline memory.current = %.1f MiB\n\n", mib(base));

    // -------- Vulkan setup

    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "gemcharge",
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

    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(pd, &props);

    uint32_t nqf = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, NULL);
    VkQueueFamilyProperties *qfs = calloc(nqf, sizeof *qfs);
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qfs);
    uint32_t qf = UINT32_MAX;
    for (uint32_t i = 0; i < nqf; i++)
        if (qfs[i].queueFlags & VK_QUEUE_COMPUTE_BIT) { qf = i; break; }
    if (qf == UINT32_MAX) { fprintf(stderr, "FAIL no compute queue\n"); return 1; }

    float prio = 1.0f;
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

    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);

    // -------- optional compute pipeline for gpu mode

    VkPipeline pipe = VK_NULL_HANDLE;
    VkPipelineLayout pl = VK_NULL_HANDLE;
    VkDescriptorSetLayout dsl = VK_NULL_HANDLE;
    VkCommandPool cpool = VK_NULL_HANDLE;

    if (mode == M_GPU) {
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
            .binding = 0,
            .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
            .descriptorCount = 1,
            .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
        };
        VkDescriptorSetLayoutCreateInfo dslci = {
            .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            .bindingCount = 1, .pBindings = &b,
        };
        CHECK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl), "dsl");

        VkPushConstantRange pcr = {
            .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT, .offset = 0, .size = 4,
        };
        VkPipelineLayoutCreateInfo plci = {
            .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            .setLayoutCount = 1, .pSetLayouts = &dsl,
            .pushConstantRangeCount = 1, .pPushConstantRanges = &pcr,
        };
        CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &pl), "pipeline layout");

        VkComputePipelineCreateInfo cpci = {
            .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
            .stage = {
                .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                .stage = VK_SHADER_STAGE_COMPUTE_BIT,
                .module = sm, .pName = "main",
            },
            .layout = pl,
        };
        CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe),
              "compute pipeline");

        VkCommandPoolCreateInfo cpi = {
            .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            .queueFamilyIndex = qf,
            .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
        };
        CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool), "command pool");
        free(code);
    }

    // -------- allocate

    const VkDeviceSize chunk = (VkDeviceSize)CHUNK_MIB * 1048576;
    VkBuffer *bufs = calloc(nchunks, sizeof *bufs);
    VkDeviceMemory *mems = calloc(nchunks, sizeof *mems);
    long long allocated = 0;

    printf("%-8s %-14s %-14s %s\n", "chunk", "allocated", "mem.current", "delta/alloc");

    for (int c = 0; c < nchunks; c++) {
        VkBufferCreateInfo bci = {
            .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            .size = chunk,
            .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
            .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
        };
        CHECK(vkCreateBuffer(dev, &bci, NULL, &bufs[c]), "vkCreateBuffer");

        VkMemoryRequirements mr;
        vkGetBufferMemoryRequirements(dev, bufs[c], &mr);

        // cpu mode needs a mappable type; the others prefer device-local. On
        // this iGPU they are the same heap, which is the point.
        VkMemoryPropertyFlags want = (mode == M_CPU)
            ? (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
            : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT;
        uint32_t mt = UINT32_MAX;
        for (uint32_t t = 0; t < mp.memoryTypeCount; t++)
            if ((mr.memoryTypeBits & (1u << t)) &&
                (mp.memoryTypes[t].propertyFlags & want) == want) { mt = t; break; }
        if (mt == UINT32_MAX) {
            fprintf(stderr, "FAIL no suitable memory type for mode %s\n", argv[2]);
            return 1;
        }

        VkMemoryAllocateInfo mai = {
            .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            .allocationSize = mr.size, .memoryTypeIndex = mt,
        };
        VkResult ar = vkAllocateMemory(dev, &mai, NULL, &mems[c]);
        if (ar != VK_SUCCESS) {
            // A refused allocation is a result, not a crash: it means the
            // driver or the cgroup pushed back, which is what we came to find.
            printf("\nallocation refused at chunk %d (%.0f MiB in): VkResult %d\n",
                   c, mib(allocated), (int)ar);
            break;
        }
        CHECK(vkBindBufferMemory(dev, bufs[c], mems[c], 0), "bind");
        allocated += (long long)mr.size;

        if (mode == M_CPU) {
            void *p = NULL;
            CHECK(vkMapMemory(dev, mems[c], 0, chunk, 0, &p), "map");
            memset(p, 0xA5, chunk);
            vkUnmapMemory(dev, mems[c]);
        } else if (mode == M_GPU) {
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
            VkDescriptorBufferInfo dbi = {
                .buffer = bufs[c], .offset = 0, .range = chunk,
            };
            VkWriteDescriptorSet w = {
                .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                .dstSet = ds, .dstBinding = 0, .descriptorCount = 1,
                .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                .pBufferInfo = &dbi,
            };
            vkUpdateDescriptorSets(dev, 1, &w, 0, NULL);

            VkCommandBufferAllocateInfo cbai = {
                .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                .commandPool = cpool,
                .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                .commandBufferCount = 1,
            };
            VkCommandBuffer cmd;
            CHECK(vkAllocateCommandBuffers(dev, &cbai, &cmd), "cmd buffer");
            VkCommandBufferBeginInfo cbbi = {
                .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            };
            CHECK(vkBeginCommandBuffer(cmd, &cbbi), "begin");
            vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
            vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1,
                                    &ds, 0, NULL);
            uint32_t n = (uint32_t)(chunk / 4);
            vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_COMPUTE_BIT, 0, 4, &n);
            vkCmdDispatch(cmd, 1024, 1, 1);
            CHECK(vkEndCommandBuffer(cmd), "end");

            VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
            VkFence fence;
            CHECK(vkCreateFence(dev, &fci, NULL, &fence), "fence");
            VkSubmitInfo si = {
                .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                .commandBufferCount = 1, .pCommandBuffers = &cmd,
            };
            CHECK(vkQueueSubmit(queue, 1, &si, fence), "submit");
            VkResult wr = vkWaitForFences(dev, 1, &fence, VK_TRUE, 30000000000ull);
            if (wr != VK_SUCCESS) {
                fprintf(stderr, "FAIL dispatch on chunk %d: VkResult %d\n", c, (int)wr);
                return 1;
            }
            vkDestroyFence(dev, fence, NULL);
            vkDestroyDescriptorPool(dev, dp, NULL);
        }

        long long cur = cg_read("memory.current");
        printf("%-8d %-14.0f %-14.1f %.2f\n", c + 1, mib(allocated), mib(cur),
               allocated ? (double)(cur - base) / (double)allocated : 0.0);
        fflush(stdout);
    }

    long long final = cg_read("memory.current");
    long long peak = cg_read("memory.peak");
    double ratio = allocated ? (double)(final - base) / (double)allocated : 0.0;

    printf("\nallocated      %.0f MiB\n", mib(allocated));
    printf("charged        %.1f MiB (memory.current delta)\n", mib(final - base));
    if (peak > 0) printf("memory.peak    %.1f MiB\n", mib(peak));
    printf("ratio          %.2f\n", ratio);
    printf("verdict        %s\n",
           ratio > 0.8 ? "CHARGED — MemoryMax= governs GPU allocations"
         : ratio < 0.2 ? "NOT CHARGED — GPU allocations escape MemoryMax="
                       : "PARTIAL — see above, do not generalise");

    for (int c = 0; c < nchunks; c++) {
        if (bufs[c]) vkDestroyBuffer(dev, bufs[c], NULL);
        if (mems[c]) vkFreeMemory(dev, mems[c], NULL);
    }
    if (pipe) vkDestroyPipeline(dev, pipe, NULL);
    if (pl) vkDestroyPipelineLayout(dev, pl, NULL);
    if (dsl) vkDestroyDescriptorSetLayout(dev, dsl, NULL);
    if (cpool) vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    return 0;
}
