rdna2-hwprobe
=============

Runs hand-written RDNA2 kernels on a real AMD GPU and records exactly what the hardware returns.
It settles questions the public ISA documentation leaves open. Currently it probes
tbuffer_{load,store}_format_d16_* (and 32-bit partial stores) for every unified buffer format.
The translator's MTBUF d16 lowering (crates/tombox-xshader/src/hlsl/mtbuf.rs) and its test
model (tests/exec_mem/mtbuf.rs, hardware table tests/exec_mem/mtbuf_hw.rs) follow these results.

No game or console files are involved: the kernels are generated from scratch and the inputs are
synthetic.


Requirements
------------

- Linux with an RDNA2 GPU on the amdgpu driver (/dev/kfd). Recorded on the gfx1036 iGPU of a
  Ryzen 5 9600X; any RDNA2 part should do with GFX=gfx10xx.
- ROCm HIP runtime and headers, plus LLVM 18. On Ubuntu 24.04 / Mint 22:
    sudo apt install rocminfo libamdhip64-dev clang-18 lld-18
  rocminfo should list the GPU agent (e.g. gfx1036).


Run
---

    tools/rdna2-hwprobe/run.sh /tmp/probe                     builds runner + kernels, runs, checks both models
    python3 tools/rdna2-hwprobe/analyze_loads.py /tmp/probe   candidate narrowing rules, layout
    python3 tools/rdna2-hwprobe/nan_rule.py /tmp/probe        fp32 -> fp16 NaN rule
    python3 tools/rdna2-hwprobe/vectors.py /tmp/probe         regenerate the tests/exec_mem/mtbuf_hw.rs body

GFX selects the target (default gfx1036).

Each format gets one load kernel and one store kernel, run on 65536 threads.

- Inputs: every 8- and 16-bit component value appears in every component position. 32-bit
  components get edge values (rounding ties, the fp16 overflow boundary, subnormals, NaNs, int
  extremes) and then random values.
- Loads: registers are preset to sentinels 0xdeadbeef / 0xcafef00d, so preserved halves
  are visible.
- Stores: the output starts filled with 0xAA, so partial writes are visible.

"partial_store32.py gen <dir>", then build and run as run.sh does, followed by
"partial_store32.py show <dir>", does the same check for the 32-bit store forms.


Files
-----

run.sh              Builds runner.cpp and the kernels from gen.py, runs them, then runs model.py and
                    analyze_stores.py. Usage: run.sh <outdir>
runner.cpp          Loads <dir>/k.co and runs each kernel in <dir>/manifest.txt on 65536 threads
                    (256 per group) with args (in, out, N). The output buffer is pre-filled with
                    0xAA. Usage: runner <dir>
gen.py              Generates the probe: one load and one store kernel per buffer format, plus
                    inputs. Each thread loads two dwords, i.e. four packed 16-bit components, and
                    each component is exhaustive over 16 bits. Stores go to the out buffer in
                    regions of N*b bytes each. The buffer descriptor's word 3 (0x31016FAC) is
                    dst_sel xyzw, format 32_FLOAT (overridden by MTBUF), resource_level 1,
                    oob_select raw. Usage: gen.py <outdir> [fmt,fmt,...]
fmts.py             Unified buffer formats (GFX10 numbering), mirroring ufmt() in
                    crates/tombox-xshader/src/hlsl/mtbuf.rs.
model.py            Reference model for d16 loads from raw element bits (what the translator
                    implements), checked against every recorded element. Usage: model.py <outdir>
analyze_stores.py   Compares d16 store results (whole element, missing components 0) with the
                    store model, per format. Usage: analyze_stores.py <outdir>
analyze_loads.py    Compares every d16 load result with the hardware's own 32-bit result for the
                    same element, narrowed by candidate rules (fp32 -> fp16 nearest-even vs toward
                    zero; int truncate vs clamp), and checks the register layout and preserved
                    halves. Usage: analyze_loads.py <outdir>
nan_rule.py         Which fp32 -> fp16 NaN rule d16 loads follow. Usage: nan_rule.py <outdir>
partial_store32.py  32-bit (non-d16) partial stores: whether tbuffer_store_format_x/xy/xyz on a
                    wider format writes the whole element. Every thread stores
                    (0.25, 0.5, 0.75, 1.0) as fp32. Usage: partial_store32.py gen <outdir> |
                    show <outdir>
vectors.py          Prints the hardware vector table for tests/exec_mem/mtbuf_hw.rs: per format a
                    few elements, plus those where naive models (fp32 round-to-nearest-even, int
                    truncation) disagree with the hardware, and d16 stores of +-0.5, NaN, max
                    finite and -0. Usage: vectors.py <outdir>
valu_probe.py       VALU / SALU probes (f16 FMA, s_bfe widths, f32 NaN rules, normalized
                    conversions), see the last section. Usage: valu_probe.py gen <outdir> |
                    show <outdir>


Results (gfx1036)
-----------------

These results come from 77 formats x 65536 elements. model.py and analyze_stores.py report 0
mismatches. The Rust model in the tests matched all 5.0M load and 14.4M store elements.

d16 loads
- Two components per VGPR: c0/c1 in vdata lo/hi, c2/c3 in vdata+1.
- _x and _xyz leave the upper half of their last VGPR unchanged.
- Components the format lacks read 0, w included. The usual w = 1 comes from the descriptor's
  dst_sel, and these kernels use an identity swizzle.
- unorm / snorm / uscaled / sscaled: the exact value is rounded once to the nearest-even fp16,
  saturating at +-65504. It is not rounded through fp32: unorm16 65519 gives 0x3bff, where
  fp32 would give 0x3c00.
- 32-bit float: rounded toward zero. Finite overflow gives +-65504 and infinity stays infinity.
- NaN: sign | 0x7c00 | ((mant >> 13) or 1). The payload is truncated and not quieted.
- 32-bit int: clamped to 16 bits, never truncated.
- 11-/10-bit float: exact expansion.

d16 stores
- The fp16 input is widened exactly; a NaN keeps its payload.
- unorm: round_ne(saturate(x) * max).
- snorm: halves round away from zero (2-bit field: +-0.5 gives +-1).
- uint / sint: the 16-bit input is zero- or sign-extended, then clamped.
- uscaled / sscaled / 11-/10-bit float stores follow none of the simple rules tried; they are
  not lowered.

Partial stores (d16 and 32-bit): storing fewer components than the format has writes the whole
element, with the missing components as 0. For example, tbuffer_store_format_x of 0.25 to
8_8_8_8_UNORM writes 40 00 00 00.


Transcendental VALU ops (trans_probe.py, trans_runner.cpp, trans16_gen.py)
---------------------------------------------------------------------------

trans_probe.py writes one kernel per op and denormal mode (v_rcp/rcp_iflag/rsq/sqrt/log/exp/sin/cos_f32,
the seven _f16 forms, v_rcp/rsq/sqrt_f64; .amdhsa_float_denorm_mode 0 and 3, IEEE mode 0). Each kernel makes
its own input x = base + global_id * stride, so no input buffer is needed; the f16 kernels clear vdst
first, because the f16 VALU ops keep its upper 16 bits.

    python3 tools/rdna2-hwprobe/trans_probe.py k.s
    clang-18 -x assembler -target amdgcn-amd-amdhsa -mcpu=gfx1036 -c k.s -o k.o
    ld.lld-18 -shared k.o -o k.co
    g++ -O2 -D__HIP_PLATFORM_AMD__ tools/rdna2-hwprobe/trans_runner.cpp -o trans_runner -lamdhip64
    ./trans_runner k.co v_rcp_f16_d3 65536 0 1 out/v_rcp_f16_d3.bin     (kernel, threads, base, stride, output)

Run every sweep twice and keep it only if both outputs are identical (one early launch on gfx1036
returned all zeros once and was not reproducible).

trans16_gen.py <dir> tables.rs rdna2_hw.rs <gfx> <machine> reads v_<op>_f16_d3.bin for the seven f16 ops
(all 65536 inputs), runs the same integer algorithm as crates/tombox-xshader/src/trans16, and writes
the tables, the exception list (inputs where the algorithm and the hardware differ) and FNV-1a-64
digests of the hardware results.

Results (gfx1036):
- f16 rcp, rsq, sqrt, log, exp: correctly rounded on every input. sin, cos: correctly rounded except
  10 + 10 inputs within 2e-4 ulp of a rounding midpoint. Each f16 result equals the hardware's f32 result
  for the same input rounded to f16 (one exp input sits on an exact f32 tie and rounds up in f16).
- NaN inputs are returned unchanged; invalid operations (sqrt/rsq/log of a negative, sin/cos of an
  infinity) give 0xfe00; log(+-0) = -inf; sin/cos give +0 for every exact zero except sin(-0) = -0.
- Denormal mode 0 equals mode 3 with denormal inputs and outputs flushed to signed zero (all 7 ops).
- f32 rcp, rsq, sqrt are faithful, not correctly rounded (rcp: 89.3% correctly rounded, |error| < 0.89
  ulp), mantissa-only for rcp, v_rcp_iflag_f32 == v_rcp_f32 on normal inputs. rcp and rsq fit a
  32-segment cubic in the low 18 mantissa bits (sqrt 16 segments) with the square and cube terms
  taken from the mantissa without its lowest bit; the remaining ~0.015 ulp is not modelled yet.


f32 rcp, rcp_iflag, rsq, sqrt (trans32_solve.cpp, trans32_gen.py)
------------------------------------------------------------------

v_rcp_f64, v_rsq_f64 and v_sqrt_f64 return the transcendental unit's internal value with no
final rounding (34 bits for rcp, 35 for rsq/sqrt). The f32 instructions round that same value:

    f32 = round_to_nearest_even(internal + shift) to 24 bits, shift = -2^-27
    (0 in the table entry whose results reach 1.0: rcp and rsq segment 0, sqrt segment 15)

so the f64 outputs show the datapath directly. Per table segment (rcp and rsq: top 5 mantissa
bits, t = low 18 bits; sqrt: exponent parity and top 4 bits, t = low 19 bits), in units of the
internal LSB, with every rounding half-to-even:

    P      = c1 * t / 2^bits(t), held with a 24-bit significand (quantum q below the switch, 2q above)
    coarse = c0 + P, rounded to 2^-29
    S      = ((t >> k)^2) >> 10            k = 1 (rcp, rsq) or 2 (sqrt)
    H      = c2 + Q * rne(M * (4t + 1) / (4 Dn))       (sqrt: M * (2t + 1) / (2 Dn))
             (trans32_solve fits the f32-equivalent floor((M t + phi) / Dn); trans32_gen drops phi)
    fine   = H * S / 2^24, held with a 24-bit significand
    internal = coarse + fine

Denormal inputs and results are flushed to signed zero whatever MODE says; NaN inputs are
returned unchanged; rsq/sqrt of a negative nonzero input give 0xffc00000; sqrt(-0) = -0,
rsq(-0) = -inf, rcp(+-0) = +-inf, rcp(+-inf) = +-0. v_rcp_iflag_f32 equals v_rcp_f32.

Data (trans_probe.py kernels, every sweep run twice and kept only when identical):

    trans_runner k.co v_rcp_f32_d3 8388608 0x3f800000 1 data/v_rcp_f32_d3_e0.bin       (and 0x40000000 -> _e1,
                                                                                       rsq, sqrt, rcp_iflag)
    trans_runner k.co v_rcp_f64_d3 1048576 0x3ff00000 $((k << 29)) data/rcp64_k$k.bin 2           (k = 0..7)
    trans_runner k.co v_rsq_f64_d3 1048576 $((0x3ff00000 + (e << 20))) $((k << 29)) data/rsq64_e${e}_k$k.bin 2
    trans_runner k.co v_rcp_f32_d3 1048576 0 4099 data/edge_rcp_d3.bin           (whole float range; d0 too)

Tables (C++17, g++ -O3 trans32_solve.cpp -o trans32_solve; arguments: first segment, last segment,
2 2 (half-even ties), output file):

    OP=rcp SC=0 MUL=256 ./trans32_solve 0 31 2 2 rcp64_tables.txt
    OP=rsq EX=e SC=2 MUL=256 ./trans32_solve 0 31 2 2 rsq64_e$e.txt
    OP=sqrt EX=e SC=4 SB=19 SQ=2 MUL=256 ./trans32_solve 0 15 2 2 sqrt64_e$e.txt

Every row reports 0 mismatches against all of its f64 outputs. Then

    python3 trans32_gen.py <dir> crates/tombox-xshader/src/trans32/tables.rs crates/tombox-xshader/src/trans32/rdna2_hw.rs gfx1036 "<machine>"

writes the tables and FNV-1a-64 digests of the recorded f32 results (every mantissa of [1,4), and the
whole-range sweep in both denormal modes) that crates/tombox-xshader/src/trans32 is tested against.


f32 log, exp, sin, cos (crates/tombox-xshader/src/trans32/amd.rs)
-----------------------------------------------------------------

These are ported from AMD rocjitsu (ROCm/rocm-systems, emulation/rocjitsu/lib/util/include/util/
amdgpu_{exp,log,trig}.h, MIT; see trans32/LICENSE-rocjitsu.txt), which was fitted to gfx1100 and
gfx1201 captures. Compiled unchanged against the gfx1036 data here (quiet_snan = false, denormal mode 3)
it reproduces every recorded result: 25 complete 2^23-input ranges and the whole-range sweep in both
denormal modes. Denormal mode 0 equals mode 3 with denormal inputs and results flushed to signed zero.

    trans_runner k.co v_sin_f32_d3 8388608 0x39800000 1 data/x_sin_0x39800000.bin
    (log: 0x3f000000 0x00800000 0x7f000000; exp: 0x40000000 0xc0000000 0x42800000 0xc2800000 0x33800000;
     sin, cos: 0x3f000000 0x3f800000 0x40800000 0x4a800000 0x39800000 0x39000000 0xbe800000 0x00800000)


f64 rcp, rsq, sqrt (trans32::eval64, tb_t64)
--------------------------------------------

v_rcp_f64, v_rsq_f64 and v_sqrt_f64 run the same unit and write its internal value unrounded
(rcp A * 2^(-35-E), rsq A * 2^(-36-Eh), sqrt A * 2^(-35+Eh)). They read the top 29 mantissa bits
(truncated), so the datapath above sees a 24-bit t: the linear term uses all of it (rsq table
entry 31: the top two extra bits are ignored there), the cubic coefficient uses its top 20 bits
(TH, in rne(M * (TH + 1) / (4 Dn)) above, or 2 Dn for sqrt), the square term the same 17 as f32.
Other behaviour, from the whole-range sweeps in both denormal modes:

    denormal inputs are normalized (mode 3) or flushed (mode 0); results are exact (mode 3) or flushed
    rcp: mantissa top 29 bits zero, low bits not -> the power of two minus one ulp (of the result format)
         mantissa all ones                         -> the power of two plus one ulp
    rsq, even exponent: mantissa top 29 bits zero, low bits not -> the power of two minus one ulp
    exact powers of two: rcp and rsq exact; sqrt one 2^-26 step above (sqrt(4.0) = 0x4000000004000000)
    NaN in -> unchanged; rsq/sqrt of a negative -> 0xfff8000000000000

Recorded: the whole-range sweep (x = (i * 4099) << 32 | lo, lo = 0, 0x9e3779b9, 0xffffffff, both modes),
every mantissa of [1, 4) for each of the 32 combinations of the 3 mantissa bits and 2 extra bits the
low word carries, and every low-bit pattern at both mantissa corners: 723,517,440 results, all reproduced.

    trans_runner k.co v_rcp_f64x_d3 1048576 0 0x9e3779b9 data/edge64_rcp_d3_0x9e3779b9.bin 2      (hi = i * 4099, lo fixed)
    trans_runner k.co v_rcp_f64_d0 1048576 0x3ff00000 $(( (ml << 29) | (eh << 27) | (15 << 23) )) \
                 data/exh/rcp_0x3ff00000_$(( (ml << 29) | (eh << 27) | (15 << 23) )).bin 2       (hi = 0x3ff00000 + i)
    trans_runner k.co v_rcp_f64m_d0 8388608 1 0x3ff00000 data/cz_rcp_0x3ff00000.bin 2            (lo = i, hi fixed)
    trans_runner k.co v_rcp_f64m_d0 8388608 0xffffffff 0x3fffffff data/co_rcp_0x3fffffff.bin 2   (lo = -i)

Instruction checks (verify_probe.py, verify_runner.cpp, verify_gen.py)
----------------------------------------------------------------------

One kernel per instruction: each workgroup loads four input dwords, runs the instruction once and
stores v10, s24, s25 and SCC, so the recorded results can be replayed by the translator test with
the same encodings.

    python3 verify_probe.py k.s in 64
    clang-18 -x assembler -target amdgcn-amd-amdhsa -mcpu=gfx1036 -c k.s -o k.o && ld.lld-18 -shared k.o -o k.co
    g++ -O2 -D__HIP_PLATFORM_AMD__ -o verify_runner verify_runner.cpp -lamdhip64
    ./verify_runner k.co hv_s_bfe_u32 in/s_bfe_u32.in out/s_bfe_u32.a                 (twice; keep when identical)
    python3 verify_gen.py . crates/tombox-xshader/tests/exec_vop/rdna2_silicon.rs 32 "<machine>"

bs_x / bs_xy / bs_xyz store through a V# with stride 16 and the formats in stores.in (75 32_32_32_32_UINT,
77 32_32_32_32_FLOAT, 72 32_32_32_UINT, 62, 20 32_UINT) into memory filled with 0xaa; ds_hi writes a
dword, then ds_write_b16_d16_hi at byte offset 0 or 2, and reads the dword back. On gfx1036:

    format stores, 4-component formats: x, y[, z], then 0 in every component the instruction does not write
    format stores, 3- and 1-component formats: only the format's components are written
    s_bfe width >= 32: S0 >> offset (all remaining bits)
    v_fma*_f16 with a signalling NaN operand (IEEE mode off): the NaN comes back unquieted

image_probe.py (image atomics): each workgroup fills an 8-texel linear R32_UINT image (T# built in
s[12:19], dim 2D, height 1) with the initial texels of exec_mem::mimg::image_mimg_atomics, runs one
image_atomic_* (with and without glc) and reads the texels back.

    python3 image_probe.py k.s cases.in       (then assemble as above)
    ./verify_runner k.co ia_add_1 cases.in rec/add_1.bin 8 256                       (each ia_<op>_<glc> kernel)
    python3 image_probe.py check rec                                                (208/208 on gfx1036)
    python3 image_probe.py rust rec crates/tombox-xshader/tests/exec_mem/mimg_rdna2.rs "<machine>"


VALU / SALU probes (valu_probe.py)
----------------------------------

Same runner (runner.cpp) and kernel preamble (gen.py) as the MTBUF probe, for four questions outside
MTBUF that the ISA text leaves open. 33 kernels.

    fma16   VOP2 0x36 / 0x37 / 0x38: fused (v_fmac_f16 / v_fmamk_f16 / v_fmaak_f16, the LLVM names for
            GFX10) or multiply-then-add? Each kernel also runs v_fma_f16 and v_mul_f16 + v_add_f16 on
            the same inputs, so both references come from the hardware.
    sbfe    s_bfe_{u,i}{32,64} with a width field of 32 or more (64 or more for the 64-bit forms) and
            junk in the unused bits of S1. show compares three candidates: 7-bit width with sign-fill
            past the top bit, 7-bit width with the literal ISA formula (zero-fill), and width modulo
            the operand size.
    nan     which NaN v_fma_f32, v_mul_f32, v_add_f32, v_sub_f32, v_fmac_f32, v_min_f32 and v_max_f32
            return, with IEEE mode off and on. show checks the outputs against the rules measured on
            gfx1036 (below), so a different GPU or a changed kernel shows up as a nonzero count.
    norm    rounding, NaN and range handling of v_cvt_pknorm_{i,u}16_{f32,f16} and
            v_cvt_norm_{i,u}16_f16.

    python3 valu_probe.py gen <dir>
    clang-18 -x assembler -target amdgcn-amd-amdhsa -mcpu=gfx1036 -c <dir>/k.s -o <dir>/k.o
    ld.lld-18 -shared <dir>/k.o -o <dir>/k.co
    <runner built by run.sh> <dir>
    python3 valu_probe.py show <dir>

On gfx1036 (measured by SchmashinPrunes on the iGPU of a Ryzen 5 9600X: Linux Mint, kernel 7.0.0-42,
ROCm/HIP 5.7.1, clang / ld.lld 18.1.3; all 33 kernels ran and no kernel had outputs that differ
between threads). This is RDNA2, not the console's gfx1013.

    fma16   fused. VOP2 0x36 / 0x37 / 0x38 match hardware v_fma_f16 on all 254,891 inputs that separate
            the two models (three K values) and never match v_mul_f16 + v_add_f16. Hardware v_fma_f16
            matches the exact single-rounding model with 0 mismatches. The upper half of D is kept.
    sbfe    7-bit width, sign-fill: 152 / 152 (literal ISA formula 114, width modulo the operand size
            100). A signed field that reaches the top bit sign-fills. Junk in the unused bits of S1 is
            ignored, and so is offset bit 5 (32-bit forms) or bit 6 (64-bit forms). SCC = result != 0.
    norm    round to nearest, 0 mismatches for all six instructions, NaN gives 0. Half-up
            (floor(x + 0.5)) fails on the signed forms. v_cvt_norm_* keep the upper half of D.
    nan     the rules below, fitted by SchmashinPrunes on the raw outputs, reproduce every NaN result
            in both IEEE modes. show checks them (nan_arith / nan_minmax) and prints the number of
            results that differ per instruction: 0 on gfx1036.

NaN rules, arithmetic (v_fma_f32, v_fmac_f32, v_mul_f32, v_add_f32, v_sub_f32), operand order
(a, b, c) or (src0, src1):

    1. v_sub_f32 negates src1 first, so a NaN in src1 comes out with its sign flipped.
    2. fma / fmac: if neither a nor b is a NaN and a * b is invalid (0 * inf), the result is the
       default NaN even when c is a NaN.
    3. Otherwise the result is the first NaN operand: unchanged in IEEE mode 0 (a signalling NaN stays
       signalling), quieted (| 0x400000) in IEEE mode 1.
    4. An invalid operation without a NaN input (0 * inf, inf - inf, the fma sum) returns 0xffc00000
       in both modes.

NaN rules, v_min_f32 / v_max_f32: in IEEE mode 0 a NaN operand is ignored when the other one is a
number, and two NaNs give src0 unchanged. In IEEE mode 1 a signalling NaN wins and is quieted (src0
first); otherwise a quiet NaN is ignored when the other operand is a number, and two quiet NaNs give
src0.

Colour export and blend probe (cb_probe.cpp, cb_probe.vert, cb_probe.frag, cb_probe.py)
--------------------------------------------------------------------------------------

A Vulkan program, not raw ISA: it draws one row of pixels whose fragment shader writes four f32
bit patterns from a storage buffer, into RGBA16F, B10G11R11_UFLOAT, RGBA32F and R16F targets, with
blending off, ONE/ZERO ADD, ONE/ONE ADD, MIN and MAX. A first unblended pass writes the destination
values. It runs on any Vulkan 1.3 driver; the first device whose name contains the given text is used.
Needs libvulkan-dev and glslang-tools.

    glslangValidator -V cb_probe.vert -o cb_probe.vert.spv
    glslangValidator -V cb_probe.frag -o cb_probe.frag.spv
    g++ -O1 -o cb_probe cb_probe.cpp -lvulkan
    python3 cb_probe.py gen cb.in                        (prints the pixel count, 340)
    ./cb_probe RADV cb.in 340 cb.out                     (twice; keep when identical)
    python3 cb_probe.py show cb.out                      (every stored value)
    python3 cb_probe.py check cb.out                     (the gfx1036 rules; prints the differences)

RADV_DEBUG=shaders shows the export: on RADV, RGBA16F and R11G11B10 are written with
v_cvt_pkrtz_f16_f32 and a compressed export, RGBA32F with a 32-bit export. On gfx1036 (Mesa 25.2.8
RADV), check prints 0 differences:

    unblended / ONE,ZERO  +Inf stays Inf, every NaN becomes the format's NaN (fp16 0xfe00), finite
                          overflow becomes the largest finite value, negatives become 0 in R11G11B10
    ONE,ONE ADD           Inf + finite = Inf, Inf + -Inf = NaN, NaN in either operand gives NaN; finite
                          sums round toward zero and saturate (40000 + 40000 = 0x7bff)
    MIN / MAX             a NaN operand is ignored and the other one is stored; -0 comes back as +0

Mip selection and resinfo probe (mip_probe.py, mip_runner.cpp)
--------------------------------------------------------------

mip_runner keeps one 1 MiB image buffer for the whole run. g4_fill writes a 16x16 R32_FLOAT image
with 5 levels through image_store_mip (the texel at level m, x, y holds m * 1000 + y * 16 + x), so the
layout of the levels does not matter. The other kernels build the T# and S# from each case and run
image_gather4_l, image_gather4_lz, image_sample_l (point xy filter, u = v = 0.53) or
image_get_resinfo (2D array, 2D, 3D).

    python3 mip_probe.py gen k.s in
    clang-18 -x assembler -target amdgcn-amd-amdhsa -mcpu=gfx1036 -c k.s -o k.o && ld.lld-18 -shared k.o -o k.co
    g++ -O2 -D__HIP_PLATFORM_AMD__ -o mip_runner mip_runner.cpp -lamdhip64
    ./mip_runner k.co g4_fill in/fill.in - g4_g4l in/samp.in out/g4l.out g4_g4lz in/samp.in out/g4lz.out \
        g4_sl in/samp.in out/sl.out g4_rinfo2da in/rinfo2da.in out/r2da.out g4_rinfo2d in/rinfo2d.in out/r2d.out \
        g4_rinfo3d in/rinfo3d.in out/r3d.out
    python3 mip_probe.py check in out

On gfx1036 check prints 0 differences for every group. The rules it checks are in docs/BLOCKERS.md
("gather4_l footprint ignores sampler bias/clamp" and "image_get_resinfo array layers are not
minified"): LOD_BIAS applies to _l, the S# clamp follows the bias, an exact .5 LOD rounds up, and a 2D
array's layer word is not minified.

f16 output modifier probe (omod_probe.py)
-----------------------------------------

One kernel per f16-result VALU op and MODE (IEEE 0/1 x f16/f64 denorm mode 0..3, set through the
kernel descriptor): each runs the op as _e64 without an omod and with mul:2, mul:4 and div:2 on the
same inputs (8192 cases from om.in).

    python3 omod_probe.py gen k.s om.in > names.txt
    clang-18 -x assembler -target amdgcn-amd-amdhsa -mcpu=gfx1036 -c k.s -o k.o && ld.lld-18 -shared k.o -o k.co
    g++ -O2 -D__HIP_PLATFORM_AMD__ -o verify_runner verify_runner.cpp -lamdhip64
    for n in $(cat names.txt); do ./verify_runner k.co $n om.in out/$n.out; done
    python3 omod_probe.py check out

On gfx1036 check prints 0 differences: the omod acts only with IEEE 0 and f16 output denormals
flushed (denorm mode 0 or 1); a +-0 or denormal pre-omod result then becomes +0, and a scaled value
below 2^-14 becomes a zero of its sign.
