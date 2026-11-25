#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>
#include <string>
#include <fstream>

#define CK(x) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { fprintf(stderr, "%s = %d (line %d)\n", #x, r_, __LINE__); exit(1); } } while (0)

static std::vector<char> rd(const char* p) {
  std::ifstream f(p, std::ios::binary);
  return std::vector<char>((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
}

struct Buf { VkBuffer b; VkDeviceMemory m; void* p; };
VkDevice dev; VkPhysicalDevice pd;

static Buf mkbuf(VkDeviceSize n, VkBufferUsageFlags u) {
  Buf r;
  VkBufferCreateInfo ci{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
  ci.size = n; ci.usage = u;
  CK(vkCreateBuffer(dev, &ci, 0, &r.b));
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, r.b, &mr);
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd, &mp);
  uint32_t t = 0;
  for (; t < mp.memoryTypeCount; t++)
    if ((mr.memoryTypeBits & (1u << t)) && (mp.memoryTypes[t].propertyFlags & (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)) == (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)) break;
  VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
  ai.allocationSize = mr.size; ai.memoryTypeIndex = t;
  CK(vkAllocateMemory(dev, &ai, 0, &r.m));
  CK(vkBindBufferMemory(dev, r.b, r.m, 0));
  CK(vkMapMemory(dev, r.m, 0, n, 0, &r.p));
  return r;
}

static VkShaderModule mod(const char* p) {
  auto c = rd(p);
  VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
  ci.codeSize = c.size(); ci.pCode = (const uint32_t*)c.data();
  VkShaderModule m; CK(vkCreateShaderModule(dev, &ci, 0, &m)); return m;
}

int main(int argc, char** argv) {
  if (argc < 5) { fprintf(stderr, "usage: cb_probe <device substring> <in.bin> <npix> <out.bin>\n"); return 2; }
  const char* want = argv[1];
  auto in = rd(argv[2]);
  uint32_t W = atoi(argv[3]);
  VkApplicationInfo app{VK_STRUCTURE_TYPE_APPLICATION_INFO};
  app.apiVersion = VK_API_VERSION_1_3;
  VkInstanceCreateInfo ici{VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
  ici.pApplicationInfo = &app;
  VkInstance inst; CK(vkCreateInstance(&ici, 0, &inst));
  uint32_t n = 0; vkEnumeratePhysicalDevices(inst, &n, 0);
  std::vector<VkPhysicalDevice> pds(n); vkEnumeratePhysicalDevices(inst, &n, pds.data());
  pd = VK_NULL_HANDLE;
  for (auto p : pds) { VkPhysicalDeviceProperties pr; vkGetPhysicalDeviceProperties(p, &pr); if (strstr(pr.deviceName, want)) { pd = p; fprintf(stderr, "device: %s\n", pr.deviceName); break; } }
  if (!pd) { fprintf(stderr, "no device matching %s\n", want); return 1; }
  uint32_t qn = 0; vkGetPhysicalDeviceQueueFamilyProperties(pd, &qn, 0);
  std::vector<VkQueueFamilyProperties> qf(qn); vkGetPhysicalDeviceQueueFamilyProperties(pd, &qn, qf.data());
  uint32_t q = 0; while (!(qf[q].queueFlags & VK_QUEUE_GRAPHICS_BIT)) q++;
  float pri = 1;
  VkDeviceQueueCreateInfo qci{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
  qci.queueFamilyIndex = q; qci.queueCount = 1; qci.pQueuePriorities = &pri;
  VkPhysicalDeviceVulkan13Features f13{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES};
  f13.dynamicRendering = VK_TRUE;
  VkDeviceCreateInfo dci{VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
  dci.pNext = &f13; dci.queueCreateInfoCount = 1; dci.pQueueCreateInfos = &qci;
  CK(vkCreateDevice(pd, &dci, 0, &dev));
  VkQueue queue; vkGetDeviceQueue(dev, q, 0, &queue);

  Buf src = mkbuf(in.size(), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
  memcpy(src.p, in.data(), in.size());

  VkDescriptorSetLayoutBinding lb{0, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_FRAGMENT_BIT, 0};
  VkDescriptorSetLayoutCreateInfo lci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
  lci.bindingCount = 1; lci.pBindings = &lb;
  VkDescriptorSetLayout dsl; CK(vkCreateDescriptorSetLayout(dev, &lci, 0, &dsl));
  VkPushConstantRange pcr{VK_SHADER_STAGE_FRAGMENT_BIT, 0, 4};
  VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
  plci.setLayoutCount = 1; plci.pSetLayouts = &dsl; plci.pushConstantRangeCount = 1; plci.pPushConstantRanges = &pcr;
  VkPipelineLayout pl; CK(vkCreatePipelineLayout(dev, &plci, 0, &pl));
  VkDescriptorPoolSize ps{VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1};
  VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
  dpci.maxSets = 1; dpci.poolSizeCount = 1; dpci.pPoolSizes = &ps;
  VkDescriptorPool dp; CK(vkCreateDescriptorPool(dev, &dpci, 0, &dp));
  VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
  dsai.descriptorPool = dp; dsai.descriptorSetCount = 1; dsai.pSetLayouts = &dsl;
  VkDescriptorSet ds; CK(vkAllocateDescriptorSets(dev, &dsai, &ds));
  VkDescriptorBufferInfo dbi{src.b, 0, VK_WHOLE_SIZE};
  VkWriteDescriptorSet wds{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET};
  wds.dstSet = ds; wds.descriptorCount = 1; wds.descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; wds.pBufferInfo = &dbi;
  vkUpdateDescriptorSets(dev, 1, &wds, 0, 0);

  VkShaderModule vs = mod("cb_probe.vert.spv"), fs = mod("cb_probe.frag.spv");
  VkFormat fmts[] = {VK_FORMAT_R16G16B16A16_SFLOAT, VK_FORMAT_B10G11R11_UFLOAT_PACK32, VK_FORMAT_R32G32B32A32_SFLOAT, VK_FORMAT_R16_SFLOAT};
  uint32_t bpp[] = {8, 4, 16, 2};
  const int NM = 5;
  const char* mname[NM] = {"off", "one_zero", "add", "min", "max"};

  VkCommandPoolCreateInfo cpci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
  cpci.queueFamilyIndex = q;
  VkCommandPool cp; CK(vkCreateCommandPool(dev, &cpci, 0, &cp));
  FILE* out = fopen(argv[4], "wb");

  for (int fi = 0; fi < 4; fi++) {
    VkFormat fmt = fmts[fi];
    VkPipeline pipes[NM];
    for (int m = 0; m < NM; m++) {
      VkPipelineShaderStageCreateInfo st[2] = {{VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}, {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}};
      st[0].stage = VK_SHADER_STAGE_VERTEX_BIT; st[0].module = vs; st[0].pName = "main";
      st[1].stage = VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module = fs; st[1].pName = "main";
      VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
      VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO};
      ia.topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;
      VkViewport vp{0, 0, (float)W, 1, 0, 1}; VkRect2D sc{{0, 0}, {W, 1}};
      VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO};
      vps.viewportCount = 1; vps.pViewports = &vp; vps.scissorCount = 1; vps.pScissors = &sc;
      VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO};
      rs.polygonMode = VK_POLYGON_MODE_FILL; rs.cullMode = VK_CULL_MODE_NONE; rs.lineWidth = 1;
      VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO};
      ms.rasterizationSamples = VK_SAMPLE_COUNT_1_BIT;
      VkPipelineColorBlendAttachmentState ba{};
      ba.colorWriteMask = 0xf;
      if (m > 0) {
        ba.blendEnable = VK_TRUE;
        VkBlendFactor sf = VK_BLEND_FACTOR_ONE, df = m == 1 ? VK_BLEND_FACTOR_ZERO : VK_BLEND_FACTOR_ONE;
        VkBlendOp op = m == 3 ? VK_BLEND_OP_MIN : m == 4 ? VK_BLEND_OP_MAX : VK_BLEND_OP_ADD;
        ba.srcColorBlendFactor = sf; ba.dstColorBlendFactor = df; ba.colorBlendOp = op;
        ba.srcAlphaBlendFactor = sf; ba.dstAlphaBlendFactor = df; ba.alphaBlendOp = op;
      }
      VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO};
      cb.attachmentCount = 1; cb.pAttachments = &ba;
      VkPipelineRenderingCreateInfo pri2{VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO};
      pri2.colorAttachmentCount = 1; pri2.pColorAttachmentFormats = &fmt;
      VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
      gp.pNext = &pri2; gp.stageCount = 2; gp.pStages = st; gp.pVertexInputState = &vi; gp.pInputAssemblyState = &ia;
      gp.pViewportState = &vps; gp.pRasterizationState = &rs; gp.pMultisampleState = &ms; gp.pColorBlendState = &cb; gp.layout = pl;
      CK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, 0, &pipes[m]));
    }
    for (int m = 0; m < NM; m++) {
      VkImageCreateInfo ic{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
      ic.imageType = VK_IMAGE_TYPE_2D; ic.format = fmt; ic.extent = {W, 1, 1}; ic.mipLevels = 1; ic.arrayLayers = 1;
      ic.samples = VK_SAMPLE_COUNT_1_BIT; ic.tiling = VK_IMAGE_TILING_OPTIMAL;
      ic.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
      VkImage img; CK(vkCreateImage(dev, &ic, 0, &img));
      VkMemoryRequirements mr; vkGetImageMemoryRequirements(dev, img, &mr);
      VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd, &mp);
      uint32_t t = 0; while (!(mr.memoryTypeBits & (1u << t))) t++;
      VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; ai.allocationSize = mr.size; ai.memoryTypeIndex = t;
      VkDeviceMemory im; CK(vkAllocateMemory(dev, &ai, 0, &im)); CK(vkBindImageMemory(dev, img, im, 0));
      VkImageViewCreateInfo vci{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO};
      vci.image = img; vci.viewType = VK_IMAGE_VIEW_TYPE_2D; vci.format = fmt; vci.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
      VkImageView iv; CK(vkCreateImageView(dev, &vci, 0, &iv));
      Buf rb = mkbuf(W * bpp[fi], VK_BUFFER_USAGE_TRANSFER_DST_BIT);
      VkCommandBufferAllocateInfo cbai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
      cbai.commandPool = cp; cbai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount = 1;
      VkCommandBuffer c; CK(vkAllocateCommandBuffers(dev, &cbai, &c));
      VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
      CK(vkBeginCommandBuffer(c, &bi));
      VkImageMemoryBarrier b0{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER};
      b0.oldLayout = VK_IMAGE_LAYOUT_UNDEFINED; b0.newLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
      b0.dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT | VK_ACCESS_COLOR_ATTACHMENT_READ_BIT;
      b0.srcQueueFamilyIndex = b0.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED; b0.image = img; b0.subresourceRange = vci.subresourceRange;
      vkCmdPipelineBarrier(c, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, 0, 0, 0, 0, 0, 1, &b0);
      VkRenderingAttachmentInfo ra{VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO};
      ra.imageView = iv; ra.imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL; ra.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR; ra.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
      VkRenderingInfo rinfo{VK_STRUCTURE_TYPE_RENDERING_INFO};
      rinfo.renderArea = {{0, 0}, {W, 1}}; rinfo.layerCount = 1; rinfo.colorAttachmentCount = 1; rinfo.pColorAttachments = &ra;
      vkCmdBeginRendering(c, &rinfo);
      vkCmdBindDescriptorSets(c, VK_PIPELINE_BIND_POINT_GRAPHICS, pl, 0, 1, &ds, 0, 0);
      uint32_t base = W;
      vkCmdBindPipeline(c, VK_PIPELINE_BIND_POINT_GRAPHICS, pipes[0]);
      vkCmdPushConstants(c, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 4, &base);
      vkCmdDraw(c, 3, 1, 0, 0);
      vkCmdEndRendering(c);
      VkMemoryBarrier mb{VK_STRUCTURE_TYPE_MEMORY_BARRIER};
      mb.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT; mb.dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_READ_BIT | VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT;
      vkCmdPipelineBarrier(c, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, 0, 1, &mb, 0, 0, 0, 0);
      ra.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
      vkCmdBeginRendering(c, &rinfo);
      base = 0;
      vkCmdBindPipeline(c, VK_PIPELINE_BIND_POINT_GRAPHICS, pipes[m]);
      vkCmdPushConstants(c, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 4, &base);
      vkCmdDraw(c, 3, 1, 0, 0);
      vkCmdEndRendering(c);
      VkImageMemoryBarrier b1 = b0;
      b1.oldLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL; b1.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
      b1.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT; b1.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
      vkCmdPipelineBarrier(c, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, 0, 0, 0, 1, &b1);
      VkBufferImageCopy bic{}; bic.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}; bic.imageExtent = {W, 1, 1};
      vkCmdCopyImageToBuffer(c, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rb.b, 1, &bic);
      VkMemoryBarrier hb{VK_STRUCTURE_TYPE_MEMORY_BARRIER}; hb.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT; hb.dstAccessMask = VK_ACCESS_HOST_READ_BIT;
      vkCmdPipelineBarrier(c, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &hb, 0, 0, 0, 0);
      CK(vkEndCommandBuffer(c));
      VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount = 1; si.pCommandBuffers = &c;
      CK(vkQueueSubmit(queue, 1, &si, VK_NULL_HANDLE));
      CK(vkQueueWaitIdle(queue));
      fwrite(rb.p, 1, W * bpp[fi], out);
      fprintf(stderr, "format %d mode %s done\n", (int)fmt, mname[m]);
    }
  }
  fclose(out);
  return 0;
}
