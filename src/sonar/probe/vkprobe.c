// vkprobe — does Vulkan compute actually dispatch on this machine?
//
// docs/compute.md §3.1 chooses Vulkan compute as HadalOS's execution layer on
// the strength of "RADV and ANV are in Mesa and in-tree." sonar deliberately
// reports Vulkan ICDs as *installed, not verified loadable*, because a JSON
// file in /usr/share/vulkan/icd.d is not evidence a shader ran.
//
// This closes that gap: enumerate, then actually dispatch a compute shader and
// check the results came back correct. Enumeration alone proves the loader
// found an ICD; only a verified dispatch proves the GPU ran the work.
//
// Throwaway diagnostic. Uses the system headers and loader, adds nothing to
// the repo. cc -O2 vkprobe.c -lvulkan -o vkprobe

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

#define N 4096
#define LOCAL_SIZE 64

#define CHECK(expr, what)                                                      \
    do {                                                                       \
        VkResult _r = (expr);                                                  \
        if (_r != VK_SUCCESS) {                                                \
            fprintf(stderr, "FAIL %s: VkResult %d\n", (what), (int)_r);        \
            return 1;                                                          \
        }                                                                      \
    } while (0)

static const char *dev_type(VkPhysicalDeviceType t) {
    switch (t) {
    case VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU: return "integrated";
    case VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU:   return "discrete";
    case VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU:    return "virtual";
    case VK_PHYSICAL_DEVICE_TYPE_CPU:            return "cpu";
    default:                                     return "other";
    }
}

static char *read_spirv(const char *path, size_t *out_len) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); return NULL; }
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (len <= 0 || len % 4 != 0) {
        fprintf(stderr, "%s: not a SPIR-V module (%ld bytes)\n", path, len);
        fclose(f);
        return NULL;
    }
    char *buf = malloc((size_t)len);
    if (fread(buf, 1, (size_t)len, f) != (size_t)len) {
        fclose(f); free(buf); return NULL;
    }
    fclose(f);
    *out_len = (size_t)len;
    return buf;
}

int main(int argc, char **argv) {
    const char *spv = argc > 1 ? argv[1] : "double.spv";

    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "vkprobe",
        .apiVersion = VK_API_VERSION_1_1,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance inst;
    CHECK(vkCreateInstance(&ici, NULL, &inst), "vkCreateInstance");

    uint32_t ndev = 0;
    CHECK(vkEnumeratePhysicalDevices(inst, &ndev, NULL), "enumerate count");
    if (ndev == 0) {
        fprintf(stderr, "FAIL no Vulkan physical devices — ICDs installed but none loadable\n");
        return 1;
    }
    VkPhysicalDevice *devs = calloc(ndev, sizeof *devs);
    CHECK(vkEnumeratePhysicalDevices(inst, &ndev, devs), "enumerate");

    printf("%u physical device(s)\n\n", ndev);

    VkPhysicalDevice pick = VK_NULL_HANDLE;
    uint32_t pick_qf = 0;

    for (uint32_t i = 0; i < ndev; i++) {
        VkPhysicalDeviceProperties p;
        vkGetPhysicalDeviceProperties(devs[i], &p);
        printf("  [%u] %s\n", i, p.deviceName);
        printf("      type %s   api %u.%u.%u   driver 0x%x   vendor 0x%04x device 0x%04x\n",
               dev_type(p.deviceType),
               VK_VERSION_MAJOR(p.apiVersion), VK_VERSION_MINOR(p.apiVersion),
               VK_VERSION_PATCH(p.apiVersion),
               p.driverVersion, p.vendorID, p.deviceID);
        printf("      max workgroup invocations %u   shared mem %u KiB\n",
               p.limits.maxComputeWorkGroupInvocations,
               p.limits.maxComputeSharedMemorySize / 1024);

        uint32_t nqf = 0;
        vkGetPhysicalDeviceQueueFamilyProperties(devs[i], &nqf, NULL);
        VkQueueFamilyProperties *qf = calloc(nqf, sizeof *qf);
        vkGetPhysicalDeviceQueueFamilyProperties(devs[i], &nqf, qf);
        int found = -1;
        for (uint32_t q = 0; q < nqf; q++) {
            int compute  = (qf[q].queueFlags & VK_QUEUE_COMPUTE_BIT) != 0;
            int graphics = (qf[q].queueFlags & VK_QUEUE_GRAPHICS_BIT) != 0;
            printf("      qf %u: %u queue(s)%s%s%s\n", q, qf[q].queueCount,
                   graphics ? " graphics" : "",
                   compute ? " compute" : "",
                   (qf[q].queueFlags & VK_QUEUE_TRANSFER_BIT) ? " transfer" : "");
            // Prefer a compute-capable family; note whether it is shared with
            // graphics, because a shared family is the hardware reason
            // compute.md §4.3 needs a display reservation at all.
            if (compute && found < 0) found = (int)q;
        }
        free(qf);

        // Unified memory shows up here: on an iGPU the DEVICE_LOCAL heap is
        // also HOST_VISIBLE, i.e. it is system RAM, reached over the same bus
        // the display controller scans out of.
        VkPhysicalDeviceMemoryProperties mp;
        vkGetPhysicalDeviceMemoryProperties(devs[i], &mp);
        for (uint32_t h = 0; h < mp.memoryHeapCount; h++) {
            int local = (mp.memoryHeaps[h].flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) != 0;
            int host_visible = 0;
            for (uint32_t t = 0; t < mp.memoryTypeCount; t++)
                if (mp.memoryTypes[t].heapIndex == h &&
                    (mp.memoryTypes[t].propertyFlags & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT))
                    host_visible = 1;
            printf("      heap %u: %.1f GiB%s%s\n", h,
                   mp.memoryHeaps[h].size / 1073741824.0,
                   local ? " device-local" : "",
                   host_visible ? " host-visible" : "");
        }

        if (found >= 0 && pick == VK_NULL_HANDLE) {
            pick = devs[i];
            pick_qf = (uint32_t)found;
        }
        printf("\n");
    }

    if (pick == VK_NULL_HANDLE) {
        fprintf(stderr, "FAIL no device exposes a compute queue\n");
        return 1;
    }

    VkPhysicalDeviceProperties pp;
    vkGetPhysicalDeviceProperties(pick, &pp);
    printf("dispatching on: %s (queue family %u)\n", pp.deviceName, pick_qf);

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = pick_qf,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
    };
    VkDevice dev;
    CHECK(vkCreateDevice(pick, &dci, NULL, &dev), "vkCreateDevice");

    VkQueue queue;
    vkGetDeviceQueue(dev, pick_qf, 0, &queue);

    const VkDeviceSize bytes = N * sizeof(uint32_t);
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
    vkGetPhysicalDeviceMemoryProperties(pick, &mp);
    uint32_t mem_type = UINT32_MAX;
    for (uint32_t t = 0; t < mp.memoryTypeCount; t++) {
        VkMemoryPropertyFlags want = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                     VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        if ((mr.memoryTypeBits & (1u << t)) &&
            (mp.memoryTypes[t].propertyFlags & want) == want) {
            mem_type = t;
            break;
        }
    }
    if (mem_type == UINT32_MAX) {
        fprintf(stderr, "FAIL no host-visible coherent memory type\n");
        return 1;
    }

    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = mr.size,
        .memoryTypeIndex = mem_type,
    };
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mai, NULL, &mem), "vkAllocateMemory");
    CHECK(vkBindBufferMemory(dev, buf, mem, 0), "vkBindBufferMemory");

    uint32_t *host = NULL;
    CHECK(vkMapMemory(dev, mem, 0, bytes, 0, (void **)&host), "vkMapMemory");
    for (uint32_t i = 0; i < N; i++) host[i] = i;

    size_t spv_len = 0;
    char *code = read_spirv(spv, &spv_len);
    if (!code) return 1;

    VkShaderModuleCreateInfo smci = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = spv_len,
        .pCode = (const uint32_t *)code,
    };
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &smci, NULL, &shader), "vkCreateShaderModule");

    VkDescriptorSetLayoutBinding bind = {
        .binding = 0,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1,
        .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
    };
    VkDescriptorSetLayoutCreateInfo dslci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 1,
        .pBindings = &bind,
    };
    VkDescriptorSetLayout dsl;
    CHECK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl), "descriptor layout");

    VkPipelineLayoutCreateInfo plci = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1,
        .pSetLayouts = &dsl,
    };
    VkPipelineLayout pl;
    CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &pl), "pipeline layout");

    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {
            .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
            .stage = VK_SHADER_STAGE_COMPUTE_BIT,
            .module = shader,
            .pName = "main",
        },
        .layout = pl,
    };
    VkPipeline pipe;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe),
          "vkCreateComputePipelines");

    VkDescriptorPoolSize dps = {
        .type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1,
    };
    VkDescriptorPoolCreateInfo dpci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 1,
        .poolSizeCount = 1,
        .pPoolSizes = &dps,
    };
    VkDescriptorPool dpool;
    CHECK(vkCreateDescriptorPool(dev, &dpci, NULL, &dpool), "descriptor pool");

    VkDescriptorSetAllocateInfo dsai = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = dpool,
        .descriptorSetCount = 1,
        .pSetLayouts = &dsl,
    };
    VkDescriptorSet dset;
    CHECK(vkAllocateDescriptorSets(dev, &dsai, &dset), "allocate descriptor set");

    VkDescriptorBufferInfo dbi = { .buffer = buf, .offset = 0, .range = bytes };
    VkWriteDescriptorSet wds = {
        .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        .dstSet = dset,
        .dstBinding = 0,
        .descriptorCount = 1,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .pBufferInfo = &dbi,
    };
    vkUpdateDescriptorSets(dev, 1, &wds, 0, NULL);

    VkCommandPoolCreateInfo cpi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = pick_qf,
    };
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool), "command pool");

    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = cpool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cbai, &cmd), "allocate command buffer");

    VkCommandBufferBeginInfo cbbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    CHECK(vkBeginCommandBuffer(cmd, &cbbi), "begin");
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1, &dset, 0, NULL);
    vkCmdDispatch(cmd, N / LOCAL_SIZE, 1, 1);
    CHECK(vkEndCommandBuffer(cmd), "end");

    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    VkFence fence;
    CHECK(vkCreateFence(dev, &fci, NULL, &fence), "fence");

    VkSubmitInfo si = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &cmd,
    };
    CHECK(vkQueueSubmit(queue, 1, &si, fence), "vkQueueSubmit");

    VkResult wr = vkWaitForFences(dev, 1, &fence, VK_TRUE, 5000000000ull); // 5s
    if (wr == VK_TIMEOUT) {
        fprintf(stderr, "FAIL dispatch did not complete within 5s\n");
        return 1;
    }
    CHECK(wr, "vkWaitForFences");

    // The whole point. Enumeration got us this far; only this says the GPU
    // executed the shader.
    uint32_t bad = 0;
    for (uint32_t i = 0; i < N; i++) {
        uint32_t want = i * 2u + 1u;
        if (host[i] != want) {
            if (bad < 4)
                fprintf(stderr, "  mismatch at %u: got %u want %u\n", i, host[i], want);
            bad++;
        }
    }

    if (bad) {
        fprintf(stderr, "FAIL %u/%u elements wrong\n", bad, (uint32_t)N);
        return 1;
    }
    printf("OK dispatched %d elements, all correct\n", N);

    vkDestroyFence(dev, fence, NULL);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, dpool, NULL);
    vkDestroyPipeline(dev, pipe, NULL);
    vkDestroyPipelineLayout(dev, pl, NULL);
    vkDestroyDescriptorSetLayout(dev, dsl, NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    vkDestroyBuffer(dev, buf, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    free(code);
    free(devs);
    return 0;
}
