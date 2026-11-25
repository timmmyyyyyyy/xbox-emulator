import math, random, struct, sys

OPS = {
    "v_add_f16": "v1, v2", "v_sub_f16": "v1, v2", "v_subrev_f16": "v1, v2", "v_mul_f16": "v1, v2",
    "v_max_f16": "v1, v2", "v_min_f16": "v1, v2", "v_ldexp_f16": "v1, v4", "v_fmac_f16": "v1, v2",
    "v_fma_f16": "v1, v2, v3", "v_div_fixup_f16": "v1, v2, v3", "v_min3_f16": "v1, v2, v3",
    "v_max3_f16": "v1, v2, v3", "v_med3_f16": "v1, v2, v3",
    "v_rcp_f16": "v1", "v_sqrt_f16": "v1", "v_rsq_f16": "v1", "v_log_f16": "v1", "v_exp_f16": "v1",
    "v_frexp_mant_f16": "v1", "v_floor_f16": "v1", "v_ceil_f16": "v1", "v_trunc_f16": "v1",
    "v_rndne_f16": "v1", "v_fract_f16": "v1", "v_sin_f16": "v1", "v_cos_f16": "v1",
    "v_cvt_f16_f32": "v4", "v_cvt_f16_u16": "v4", "v_cvt_f16_i16": "v4",
}
OMOD = [("", 1.0), (" mul:2", 2.0), (" mul:4", 4.0), (" div:2", 0.5)]
MODES = [(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (1, 1), (1, 2), (1, 3)]


def name(op, ieee, dn):
    return f"om_{op}_i{ieee}d{dn}"


def kernel(n, op, args, ieee, dn):
    body = ""
    for k, (om, _) in enumerate(OMOD):
        body += f"  v_mov_b32 v{10 + k}, s22\n"
        body += f"  {op}_e64 v{10 + k}, {args}{om}\n"
    return f"""
.text
.globl {n}
.p2align 8
.type {n},@function
{n}:
  s_load_dwordx4 s[4:7], s[0:1], 0x0
  s_waitcnt lgkmcnt(0)
  s_lshl_b32 s8, s2, 4
  s_load_dwordx4 s[20:23], s[4:5], s8
  s_waitcnt lgkmcnt(0)
  v_mov_b32 v1, s20
  v_mov_b32 v2, s21
  v_mov_b32 v3, s22
  v_mov_b32 v4, s23
{body}  v_mov_b32 v20, s8
  v_cmp_eq_u32 vcc_lo, 0, v0
  s_and_saveexec_b32 s9, vcc_lo
  global_store_dwordx4 v20, v[10:13], s[6:7]
  s_waitcnt vmcnt(0)
  s_endpgm
.rodata
.p2align 6
.amdhsa_kernel {n}
  .amdhsa_user_sgpr_kernarg_segment_ptr 1
  .amdhsa_system_sgpr_workgroup_id_x 1
  .amdhsa_next_free_vgpr 32
  .amdhsa_next_free_sgpr 40
  .amdhsa_wavefront_size32 1
  .amdhsa_kernarg_size 16
  .amdhsa_ieee_mode {ieee}
  .amdhsa_dx10_clamp 1
  .amdhsa_float_denorm_mode_32 3
  .amdhsa_float_denorm_mode_16_64 {dn}
.end_amdhsa_kernel
"""


def meta(names):
    rows = []
    for n in names:
        rows.append(f"""  - .name: {n}
    .symbol: {n}.kd
    .kernarg_segment_size: 16
    .group_segment_fixed_size: 0
    .private_segment_fixed_size: 0
    .kernarg_segment_align: 8
    .wavefront_size: 32
    .sgpr_count: 40
    .vgpr_count: 32
    .max_flat_workgroup_size: 32
    .args:
      - {{ .size: 8, .offset: 0, .value_kind: global_buffer, .address_space: global }}
      - {{ .size: 8, .offset: 8, .value_kind: global_buffer, .address_space: global }}""")
    return ".amdgpu_metadata\n---\namdhsa.version: [ 1, 1 ]\namdhsa.kernels:\n" + "\n".join(rows) + "\n...\n.end_amdgpu_metadata\n"


def h16(r):
    k = r.randrange(5)
    if k == 0:
        return r.choice([0, 0x8000, 0x3C00, 0xBC00, 0x0001, 0x03FF, 0x0400, 0x7BFF, 0x7C00, 0xFC00, 0x7E00, 0x3800, 0x4000, 0x2E66])
    if k == 1:
        return (r.randint(10, 20) << 10) | r.getrandbits(10) | (r.getrandbits(1) << 15)
    return r.getrandbits(16)


def gen(asm, inp):
    src, names = [], []
    for op, args in OPS.items():
        for ieee, dn in MODES:
            names.append(name(op, ieee, dn))
            src.append(kernel(names[-1], op, args, ieee, dn))
    open(asm, "w").write("".join(src) + meta(names))
    r = random.Random(7)
    with open(inp, "wb") as f:
        for _ in range(8192):
            a, b, c = h16(r), h16(r), h16(r)
            if r.random() < 0.5:
                d = struct.unpack("<I", struct.pack("<f", r.choice([1.5, 0.1, 3.0e-5, 70000.0, 2.0e-8, -12.25, 1000.1]) * r.uniform(0.5, 2)))[0]
            else:
                d = (r.getrandbits(16) << 16) | (r.randrange(-20, 20) & 0xFFFF)
            f.write(struct.pack("<4I", a | (r.getrandbits(16) << 16), b, c, d))
    print("\n".join(names))


def h2f(h):
    return struct.unpack("<e", struct.pack("<H", h))[0]


def f2h(x):
    try:
        return struct.unpack("<H", struct.pack("<e", x))[0]
    except OverflowError:
        return 0x7C00 if x > 0 else 0xFC00


def check(outd):
    bad = scaled = kept = 0
    for op in OPS:
        for ieee, dn in MODES:
            b = open(f"{outd}/{name(op, ieee, dn)}.out", "rb").read()
            for k in range(len(b) // 16):
                r = [x & 0xFFFF for x in struct.unpack_from("<4I", b, 16 * k)]
                base = h2f(r[0])
                if math.isnan(base) or math.isinf(base) or base == 0:
                    continue
                for j in (1, 2, 3):
                    s = OMOD[j][1]
                    if ieee != 0 or dn & 2:
                        want = r[0]
                    elif r[0] & 0x7C00 == 0:
                        want = 0
                    elif abs(base * s) < 2.0 ** -14:
                        want = 0x8000 if base < 0 else 0
                    else:
                        want = f2h(base * s)
                    if r[j] != want:
                        bad += 1
                        print("differs:", op, f"ieee{ieee} f16 denorm {dn}", OMOD[j][0].strip(), hex(r[0]), hex(r[j]), "want", hex(want))
                    elif want != r[0]:
                        scaled += 1
                    else:
                        kept += 1
    print(f"{scaled} results scaled by the omod, {kept} unchanged, {bad} differ from the gfx1036 rule")
    return bad


if __name__ == "__main__":
    if sys.argv[1] == "gen":
        gen(sys.argv[2], sys.argv[3])
    else:
        sys.exit(1 if check(sys.argv[2]) else 0)
