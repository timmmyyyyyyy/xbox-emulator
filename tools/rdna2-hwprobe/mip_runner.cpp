#include <hip/hip_runtime.h>
#include <cstdio>
#include <cstdlib>
#include <vector>
#define CK(x) do { hipError_t e = (x); if (e != hipSuccess) { fprintf(stderr, "%s: %s\n", #x, hipGetErrorString(e)); exit(1); } } while (0)
static std::vector<unsigned char> rd(const char* p) {
  FILE* f = fopen(p, "rb"); if (!f) { fprintf(stderr, "missing %s\n", p); exit(1); }
  std::vector<unsigned char> v; int c; while ((c = fgetc(f)) != EOF) v.push_back((unsigned char)c); fclose(f); return v;
}
int main(int argc, char** argv) {
  if (argc < 3 || (argc - 2) % 3) { fprintf(stderr, "usage: mip_runner <k.co> (<kernel> <in> <out|->)...\n"); return 2; }
  hipModule_t mod; CK(hipModuleLoad(&mod, argv[1]));
  void* img; size_t isz = 1 << 20; CK(hipMalloc(&img, isz)); CK(hipMemset(img, 0, isz));
  for (int a = 2; a < argc; a += 3) {
    hipFunction_t fn; CK(hipModuleGetFunction(&fn, mod, argv[a]));
    auto in = rd(argv[a + 1]); size_t n = in.size() / 64;
    void *din, *dout; CK(hipMalloc(&din, in.size())); CK(hipMalloc(&dout, n * 16));
    CK(hipMemcpy(din, in.data(), in.size(), hipMemcpyHostToDevice)); CK(hipMemset(dout, 0xAA, n * 16));
    struct { void* in; void* out; void* img; } args = { din, dout, img };
    size_t asz = sizeof(args);
    void* cfg[] = { HIP_LAUNCH_PARAM_BUFFER_POINTER, &args, HIP_LAUNCH_PARAM_BUFFER_SIZE, &asz, HIP_LAUNCH_PARAM_END };
    CK(hipModuleLaunchKernel(fn, (unsigned)n, 1, 1, 32, 1, 1, 0, nullptr, nullptr, cfg));
    CK(hipDeviceSynchronize());
    if (argv[a + 2][0] != '-') {
      std::vector<unsigned char> out(n * 16); CK(hipMemcpy(out.data(), dout, n * 16, hipMemcpyDeviceToHost));
      FILE* o = fopen(argv[a + 2], "wb"); fwrite(out.data(), 1, out.size(), o); fclose(o);
    }
    CK(hipFree(din)); CK(hipFree(dout));
  }
  return 0;
}
