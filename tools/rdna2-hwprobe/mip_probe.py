import itertools, math, struct, sys

W = H = 16
LEVELS = 5
FMT_F32 = 22


def tw1(tmin=0, fmt=FMT_F32, w=W):
    return (tmin << 8) | (fmt << 20) | (((w - 1) & 3) << 30)


def tw2(w=W, h=H):
    return ((w - 1) >> 2) | ((h - 1) << 14) | (1 << 31)


def tw3(base, last, typ=9):
    return 4 | (5 << 3) | (6 << 6) | (7 << 9) | (base << 12) | (last << 16) | (typ << 28)


def tw5(max_mip=LEVELS - 1):
    return max_mip << 4


def sw(minlod, maxlod, bias, mipf, xyf=0):
    w0 = 0
    w1 = (minlod & 0xFFF) | ((maxlod & 0xFFF) << 12)
    w2 = (bias & 0x3FFF) | (xyf << 20) | (xyf << 22) | (mipf << 26)
    return [w0, w1, w2, 0]


def fx(v, signed=False):
    r = int(round(v * 256))
    return r & 0x3FFF if signed else r


HEAD = """
.text
.globl {n}
.p2align 8
.type {n},@function
{n}:
  s_load_dwordx4 s[4:7], s[0:1], 0x0
  s_load_dwordx2 s[52:53], s[0:1], 0x10
  s_waitcnt lgkmcnt(0)
  s_lshl_b32 s8, s2, 6
  s_load_dwordx16 s[20:35], s[4:5], s8
  s_waitcnt lgkmcnt(0)
  s_lshr_b64 s[36:37], s[52:53], 8
  s_mov_b32 s40, s36
  s_and_b32 s41, s37, 0xff
  s_or_b32 s41, s41, s23
  s_mov_b32 s42, s24
  s_mov_b32 s43, s25
  s_mov_b32 s44, s26
  s_mov_b32 s45, s27
  s_mov_b32 s46, 0
  s_mov_b32 s47, 0
  s_mov_b32 s48, s28
  s_mov_b32 s49, s29
  s_mov_b32 s50, s30
  s_mov_b32 s51, s31
  v_mov_b32 v1, s20
  v_mov_b32 v2, s21
  v_mov_b32 v3, s22
  v_mov_b32 v4, s32
  v_mov_b32 v10, 0x7fc0dead
  v_mov_b32 v11, 0x7fc0dead
  v_mov_b32 v12, 0x7fc0dead
  v_mov_b32 v13, 0x7fc0dead
"""

TAIL = """  s_waitcnt vmcnt(0)
  s_lshl_b32 s9, s2, 4
  v_mov_b32 v20, s9
  v_cmp_eq_u32 vcc_lo, 0, v0
  s_and_saveexec_b32 s38, vcc_lo
  global_store_dwordx4 v20, v[10:13], s[6:7]
  s_waitcnt vmcnt(0)
  s_endpgm
"""


def desc(n):
    return f"""
.rodata
.p2align 6
.amdhsa_kernel {n}
  .amdhsa_user_sgpr_kernarg_segment_ptr 1
  .amdhsa_system_sgpr_workgroup_id_x 1
  .amdhsa_next_free_vgpr 32
  .amdhsa_next_free_sgpr 64
  .amdhsa_wavefront_size32 1
  .amdhsa_kernarg_size 24
  .amdhsa_ieee_mode 0
  .amdhsa_dx10_clamp 1
.end_amdhsa_kernel
"""


OPS = {
    "g4l": "image_gather4_l v[10:13], v[1:3], s[40:47], s[48:51] dmask:0x1 dim:SQ_RSRC_IMG_2D",
    "sl": "image_sample_l v10, v[1:3], s[40:47], s[48:51] dmask:0x1 dim:SQ_RSRC_IMG_2D",
    "g4lz": "image_gather4_lz v[10:13], v[1:2], s[40:47], s[48:51] dmask:0x1 dim:SQ_RSRC_IMG_2D",
    "rinfo2da": "image_get_resinfo v[10:13], v4, s[40:47] dmask:0xf dim:SQ_RSRC_IMG_2D_ARRAY unorm",
    "rinfo2d": "image_get_resinfo v[10:13], v4, s[40:47] dmask:0xf dim:SQ_RSRC_IMG_2D unorm",
    "rinfo3d": "image_get_resinfo v[10:13], v4, s[40:47] dmask:0xf dim:SQ_RSRC_IMG_3D unorm",
}


def fill_kernel():
    n = "g4_fill"
    s = HEAD.format(n=n)
    s += f"""  v_mov_b32 v5, s2
  v_lshrrev_b32 v6, 4, v0
  v_and_b32 v7, 15, v0
  s_mov_b32 s10, 0
{n}_loop:
  v_add_nc_u32 v8, s10, v6
  v_mov_b32 v1, v7
  v_mov_b32 v2, v8
  v_mov_b32 v3, v5
  v_lshlrev_b32 v9, 4, v8
  v_add_nc_u32 v9, v9, v7
  v_mul_u32_u24 v14, 1000, v5
  v_add_nc_u32 v9, v9, v14
  v_cvt_f32_u32 v9, v9
  v_lshrrev_b32 v15, v5, 16
  v_cmp_gt_u32 vcc_lo, v15, v7
  v_cmp_gt_u32 s11, v15, v8
  s_and_b32 s11, vcc_lo, s11
  s_and_saveexec_b32 s12, s11
  image_store_mip v9, v[1:3], s[40:47] dmask:0x1 dim:SQ_RSRC_IMG_2D unorm
  s_mov_b32 exec_lo, s12
  s_add_u32 s10, s10, 2
  s_cmp_lt_u32 s10, 16
  s_cbranch_scc1 {n}_loop
  s_waitcnt vmcnt(0)
  s_waitcnt_vscnt null, 0x0
  s_endpgm
"""
    return s + desc(n)


def kernel(n, ins):
    return HEAD.format(n=n) + f"  {ins}\n" + TAIL + desc(n)


def meta(names):
    rows = []
    for n in names:
        rows.append(f"""  - .name: {n}
    .symbol: {n}.kd
    .kernarg_segment_size: 24
    .group_segment_fixed_size: 0
    .private_segment_fixed_size: 0
    .kernarg_segment_align: 8
    .wavefront_size: 32
    .sgpr_count: 64
    .vgpr_count: 32
    .max_flat_workgroup_size: 32
    .args:
      - {{ .size: 8, .offset: 0, .value_kind: global_buffer, .address_space: global }}
      - {{ .size: 8, .offset: 8, .value_kind: global_buffer, .address_space: global }}
      - {{ .size: 8, .offset: 16, .value_kind: global_buffer, .address_space: global }}""")
    return ".amdgpu_metadata\n---\namdhsa.version: [ 1, 1 ]\namdhsa.kernels:\n" + "\n".join(rows) + "\n...\n.end_amdgpu_metadata\n"


def fb(x):
    return struct.unpack("<I", struct.pack("<f", x))[0]


LODS = [-1.0, 0.0, 0.25, 0.49, 0.5, 0.51, 0.75, 1.0, 1.25, 1.49, 1.5, 1.51, 2.0, 2.5, 3.0, 3.5, 3.99, 4.0, 4.5, 6.0]
BIAS = [0.0, 1.0, 0.5, -0.75, 3.0, -1.0, 0.25, -0.5, 1.5]
SCLAMP = [(0, 15.996), (0, 1), (2, 15.996), (1, 2), (0.5, 15.996), (0, 1.5), (5, 15.996), (0, 0), (1.25, 2.75)]
TLEV = [(0, 4), (1, 4), (1, 3), (2, 2), (0, 2)]
TMIN = [0.0, 1.0, 1.5]
MIPF = [0, 1, 2]


def sampling_cases():
    rows = []
    for lod, bias, (mn, mx), (b, l), tmin, mf in itertools.product(LODS, BIAS, SCLAMP, TLEV, TMIN, MIPF):
        s = sw(fx(mn), fx(mx), fx(bias, True), mf)
        rows.append([fb(0.53), fb(0.53), fb(lod), tw1(fx(tmin)), tw2(), tw3(b, l), 0, tw5()] + s + [0, 0, 0, 0])
    return rows


def resinfo_cases(typ):
    rows = []
    for layers in (1, 2, 3, 7, 12, 16, 100, 256):
        for b, l in ((0, 4), (1, 4), (2, 4), (3, 4), (2, 3), (4, 4)):
            for mip in range(0, 7):
                rows.append([0, 0, 0, tw1(), tw2(), tw3(b, l, typ), layers - 1, tw5()] + [0, 0, 0, 0] + [mip, layers, b, l])
    return rows


def gen(asm, ind):
    names = ["g4_fill"] + [f"g4_{k}" for k in OPS]
    src = [fill_kernel()] + [kernel(f"g4_{k}", v) for k, v in OPS.items()]
    open(asm, "w").write("".join(src) + meta(names))
    fill = [[0, 0, 0, tw1(), tw2(), tw3(0, 4), 0, tw5()] + [0] * 8 for _ in range(LEVELS)]
    sets = {"fill": fill, "samp": sampling_cases(), "rinfo2da": resinfo_cases(13), "rinfo2d": resinfo_cases(9), "rinfo3d": resinfo_cases(10)}
    for k, rows in sets.items():
        with open(f"{ind}/{k}.in", "wb") as f:
            for r in rows:
                f.write(struct.pack("<16I", *[x & 0xFFFFFFFF for x in r]))
        print(k, len(rows))




def f32(u):
    return struct.unpack("<f", struct.pack("<I", u))[0]


def q8(x):
    return round(x * 256) / 256


def footprint(m, u=0.53):
    s = 16 >> m
    i = math.floor(u * s - 0.5)
    t = lambda x, y: m * 1000 + (y % s) * 16 + (x % s)
    return [t(i, i + 1), t(i + 1, i + 1), t(i + 1, i), t(i, i)]


def texel(m, u=0.53):
    s = 16 >> m
    i = math.floor(u * s)
    return m * 1000 + i * 16 + i


def lod_rule(c):
    lod, bias, (mn, mx), (b, l), tmin, mf = c
    mn, mx, bias, tmin = q8(mn), q8(mx), q8(bias), q8(tmin)
    rel = max(min(max(lod + bias, mn), mx), tmin - b)
    if mf == 0:
        return max(b, min(l, max(b, math.floor(tmin)))), rel
    return max(b, min(l, b + math.floor(rel + 0.5))), rel


def gather_lz_rule(c):
    lod, bias, sc, (b, l), tmin, mf = c
    return None if b < math.floor(q8(tmin)) else b


def resinfo_rule(typ, layers, b, l, mip):
    lvl = b + mip
    if lvl > l:
        return (0, 0, 0, l - b + 1)
    sh = lambda x: max(1, x >> lvl)
    z = layers if typ == "2da" else sh(layers) if typ == "3d" else 0
    return (sh(W), sh(H), z, l - b + 1)


def rows(path, size, fmt):
    b = open(path, "rb").read()
    return [struct.unpack_from(fmt, b, size * k) for k in range(len(b) // size)]


def check(ind, outd):
    cases = list(itertools.product(LODS, BIAS, SCLAMP, TLEV, TMIN, MIPF))
    g4l, g4lz, sl = rows(f"{outd}/g4l.out", 16, "<4I"), rows(f"{outd}/g4lz.out", 16, "<4I"), rows(f"{outd}/sl.out", 16, "<4I")
    bad = {"gather4_l": 0, "gather4_lz": 0, "sample_l": 0}
    for k, c in enumerate(cases):
        m, rel = lod_rule(c)
        if [f32(x) for x in g4l[k]] != footprint(m):
            bad["gather4_l"] += 1
        z = gather_lz_rule(c)
        if [f32(x) for x in g4lz[k]] != ([0.0] * 4 if z is None else footprint(z)):
            bad["gather4_lz"] += 1
        if c[5] != 2 and f32(sl[k][0]) != texel(m):
            bad["sample_l"] += 1
    for typ in ("2da", "2d", "3d"):
        bad["resinfo_" + typ] = 0
        ins, outs = rows(f"{ind}/rinfo{typ}.in", 64, "<16I"), rows(f"{outd}/r{typ}.out", 16, "<4I")
        for i, o in zip(ins, outs):
            if resinfo_rule(typ, i[13], i[14], i[15], i[12]) != tuple(o):
                bad["resinfo_" + typ] += 1
    for k, v in bad.items():
        print("%-14s %d results differ from the gfx1036 rules" % (k, v))
    return sum(bad.values())


def main():
    if sys.argv[1] == "gen":
        gen(sys.argv[2], sys.argv[3])
    else:
        sys.exit(1 if check(sys.argv[2], sys.argv[3]) else 0)


main()
