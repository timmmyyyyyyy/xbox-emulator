//! RDNA2 → HLSL translator (stage 3 of the shader pipeline).
//!
//! Model: one HLSL invocation == one RDNA2 *lane*. Every VGPR/SGPR is a `uint` holding the
//! raw 32-bit pattern (floats via asfloat/asuint), so no type inference is needed. Lane
//! masks (exec, vcc, compare results in SGPRs) are represented by their bit for *this*
//! lane (0/1). Descriptor-table pointers in user SGPRs are tracked symbolically: an
//! `s_load_dwordxN` from a never-written SGPR is a descriptor fetch from a root table, and
//! the way the descriptor is consumed (s_buffer_load → CBV, MIMG resource → SRV, MIMG
//! sampler → sampler) classifies the table. Slots map 1:1 to HLSL registers because the
//! title's root signature (UE layout) uses one SRV/CBV/sampler table per stage from
//! register 0.
//!
//! Scope: forward-only control flow (no loops yet). Anything not understood returns `Err`
//! naming the instruction — the caller must fall back; nothing is guessed.

use crate::meta::SigElement;
use crate::rdna2::{disassemble, Enc, Insn, Opnd};
use crate::{ShaderExtract, Stage};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

mod mode;
mod ds_ext;
mod lds64;
mod gds;
mod mtbuf;
mod fcmp;
mod icmp;
mod rootmem;
mod mesh;
mod amp;
mod global;
mod scratch;
mod centroid;
mod durango;
mod mixed;
mod mimg_d16;
mod atomic;
mod bufref;
mod user_data;
mod smem_bounds;
mod tess;
mod static_sampler;
pub use smem_bounds::{CBFIT_CBUFFER, CBFIT_RAW};
pub use tess::{passthrough_hull, Partitioning, PassthroughHull, PASSTHROUGH_VS};
pub mod infer;
use bufref::BufRef;
use mimg_d16::{mimg_data_regs, Lane};

pub struct Translated {
    pub hlsl: String,
    pub entry: &'static str,
    /// FXC/D3DCompile target profile.
    pub profile: &'static str,
    pub mip_uavs: Vec<u32>,
    pub mip_srvs: Vec<u32>,
    pub needs_gmem: bool,
    pub needs_global: bool,
    pub uses_typed_atomic64: bool,
    pub user_data_base: Option<usize>,
    pub cb_limits: BTreeMap<u32, u64>,
    pub fmask: Vec<(u32, u32, u32)>,
    pub gather_lod_blind: Vec<u32>,
    pub unknowns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct InputInterp(pub Vec<(String, String)>);

impl InputInterp {
    fn for_output(&self, e: &SigElement) -> Option<&str> {
        let s = semantic(e);
        let m = self.0.iter().find(|(k, _)| *k == s).map(|(_, m)| m.as_str())?;
        (m != "TB_CEN_Q" && (e.comp_type == 3 || m == "nointerpolation")).then_some(m)
    }

    pub fn pc_centroid(mut self) -> Self {
        for (_, m) in &mut self.0 {
            if m == "TB_CEN_Q" {
                *m = "centroid".into();
            }
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    Table(u8),
    Desc { table: u8, off: u32, k: u32 },
    /// Dword `k` of one of several descriptors (control-flow join of different `s_load`s,
    /// e.g. a loop that picks texture i of N). `set` indexes `Emitter::multi_sets`; which
    /// candidate is live is tracked at run time in a `tb_dsel<reg>` variable.
    Multi { set: u16, k: u32 },
    /// A PC-relative pointer into the shader's own code section (`s_getpc_b64` + offsets),
    /// used to read constant tables embedded after the program.
    Code,
}

/// An image op whose texture and/or sampler register holds one of several descriptors.
/// Each candidate is `(descriptor id, resolved resource)`; the id is what the run-time
/// selector `tb_dsel<reg>` holds.
#[derive(Clone, Debug)]
struct MSel {
    tex_reg: u16,
    samp_reg: u16,
    tex: Vec<(u32, Res)>,
    samp: Vec<(u32, Res)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Cb(u32),
    Tex(u32, u32), // slot, dim
    Samp(u32),
    Uav(u32, u32), // slot, dim
    Buf(u32, bool), // slot, is_uav (structured buffer)
    TBuf(u32, bool), // slot, is_uav (typed buffer)
    CbDyn(u32),      // constant buffer read through MUBUF (per-lane offset)
    CodeTbl,         // s_load from the code section (embedded constant table)
}

const XYZW: [&str; 4] = ["x", "y", "z", "w"];

/// RDNA2 cube-map helpers. v_cube{ma,sc,tc,id}_f32 follow the ISA definition (major axis ×2,
/// face-local S/T, face index 0..5 = +X,-X,+Y,-Y,+Z,-Z). Hardware cube sampling takes
/// (s, t, face) with s,t = sc/|ma| + 1.5 (so each is in [1, 2]); `tb_cube_dir` inverts that
/// to the direction vector HLSL TextureCube sampling expects.
pub fn f16_exact(v: f32) -> bool {
    let b = v.to_bits() & 0x7fff_ffff;
    if b == 0 {
        return true;
    }
    if b >= 0x7f80_0000 {
        return b == 0x7f80_0000;
    }
    let e = (b >> 23) as i32 - 127;
    if !(-24..=15).contains(&e) {
        return false;
    }
    let keep = if e >= -14 { 10 } else { 10 - (-14 - e) };
    let m = b & 0x7f_ffff;
    m & ((1u32 << (23 - keep)) - 1) == 0
}

pub fn fma16_operands_exact(ops: &[Opnd]) -> bool {
    ops.iter().all(|o| match o {
        Opnd::ConstF32(x) => f16_exact(*x),
        _ => true,
    })
}

pub(crate) const CUBE_HELPERS: &str = r#"uint tb_perm_byte(uint hi, uint lo, uint sel) { if (sel >= 13u) return 0xffu; if (sel == 12u) return 0u; if (sel == 11u) return (hi >> 31) * 0xffu; if (sel == 10u) return ((hi >> 15) & 1u) * 0xffu; if (sel == 9u) return (lo >> 31) * 0xffu; if (sel == 8u) return ((lo >> 15) & 1u) * 0xffu; return sel >= 4u ? ((hi >> ((sel - 4u) * 8u)) & 0xffu) : ((lo >> (sel * 8u)) & 0xffu); }
uint tb_perm(uint hi, uint lo, uint sel) { return tb_perm_byte(hi, lo, sel & 0xffu) | (tb_perm_byte(hi, lo, (sel >> 8) & 0xffu) << 8) | (tb_perm_byte(hi, lo, (sel >> 16) & 0xffu) << 16) | (tb_perm_byte(hi, lo, sel >> 24) << 24); }
bool tb_cmp_class(uint b, uint mask) { uint e = (b >> 23) & 0xffu; uint m = b & 0x7fffffu; bool neg = (b >> 31) != 0u; uint c; if (e == 0xffu) { c = (m != 0u) ? (((m & 0x400000u) != 0u) ? 1u : 0u) : (neg ? 2u : 9u); } else if (e == 0u) { c = (m == 0u) ? (neg ? 5u : 6u) : (neg ? 4u : 7u); } else { c = neg ? 3u : 8u; } return ((mask >> c) & 1u) != 0u; }uint tb_bfe_i32(uint a, uint b) { uint o = b & 31u, w = (b >> 16) & 127u; if (w == 0u) return 0u; if (w + o >= 32u) return asuint(asint(a) >> o); return asuint(asint(a << (32u - w - o)) >> (32u - w)); }uint tb_mulhi_u32(uint a, uint b) { uint a0 = a & 0xffffu, a1 = a >> 16, b0 = b & 0xffffu, b1 = b >> 16; uint p00 = a0 * b0, p01 = a0 * b1, p10 = a1 * b0, p11 = a1 * b1; uint mid = (p00 >> 16) + (p01 & 0xffffu) + (p10 & 0xffffu); return p11 + (p01 >> 16) + (p10 >> 16) + (mid >> 16); }
bool tb_cmp_class16(uint b, uint mask) { uint e = (b >> 10) & 0x1fu; uint m = b & 0x3ffu; bool neg = ((b >> 15) & 1u) != 0u; uint c; if (e == 0x1fu) { c = (m != 0u) ? (((m & 0x200u) != 0u) ? 1u : 0u) : (neg ? 2u : 9u); } else if (e == 0u) { c = (m == 0u) ? (neg ? 5u : 6u) : (neg ? 4u : 7u); } else { c = neg ? 3u : 8u; } return ((mask >> c) & 1u) != 0u; }
uint tb_mulhi_i32(uint a, uint b) { uint h = tb_mulhi_u32(a, b); if (asint(a) < 0) h -= b; if (asint(b) < 0) h -= a; return h; }
float tb_cubema(float x, float y, float z) { float ax = abs(x), ay = abs(y), az = abs(z); if (az >= ax && az >= ay) return 2.0 * z; if (ay >= ax) return 2.0 * y; return 2.0 * x; }
float tb_cubesc(float x, float y, float z) { float ax = abs(x), ay = abs(y), az = abs(z); if (az >= ax && az >= ay) return z < 0.0 ? -x : x; if (ay >= ax) return x; return x < 0.0 ? z : -z; }
float tb_cubetc(float x, float y, float z) { float ax = abs(x), ay = abs(y), az = abs(z); if (az >= ax && az >= ay) return -y; if (ay >= ax) return y < 0.0 ? -z : z; return -y; }
float tb_cubeid(float x, float y, float z) { float ax = abs(x), ay = abs(y), az = abs(z); if (az >= ax && az >= ay) return z < 0.0 ? 5.0 : 4.0; if (ay >= ax) return y < 0.0 ? 3.0 : 2.0; return x < 0.0 ? 1.0 : 0.0; }
float3 tb_cube_dir(float s, float t, float face) { float u = (s - 1.5) * 2.0, v = (t - 1.5) * 2.0; int f = (int)round(face); if (f == 0) return float3(1.0, -v, -u); if (f == 1) return float3(-1.0, -v, u); if (f == 2) return float3(u, 1.0, v); if (f == 3) return float3(u, -1.0, -v); if (f == 4) return float3(u, -v, 1.0); return float3(-u, -v, -1.0); }
int tb_nrm(float v, uint k) { uint b = asuint(v); uint a = b & 0x7fffffffu; if (a < 0x30000000u) return 0; uint sh = 150u - (a >> 23); uint m = (a & 0x7fffffu) | 0x800000u; uint lo = m * k; uint hi = tb_mulhi_u32(m, k); uint q, rh, rl, hh, hl; if (sh >= 32u) { q = hi >> (sh - 32u); rh = hi & ((1u << (sh - 32u)) - 1u); rl = lo; } else { q = (lo >> sh) | (hi << (32u - sh)); rh = 0u; rl = lo & ((1u << sh) - 1u); } if (sh >= 33u) { hh = 1u << (sh - 33u); hl = 0u; } else { hh = 0u; hl = 1u << (sh - 1u); } bool gt = rh > hh || (rh == hh && rl > hl); bool eq = rh == hh && rl == hl; if (gt || (eq && (q & 1u) != 0u)) q += 1u; int r = (int)q; return (b >> 31) != 0u ? -r : r; }
float tb_rev(float x) { return (abs(x) < 0.5) ? x : x - round(x); }
float tb_ldexp32(float x, int e) { int k = clamp(e, -330, 330); return (float)((double)x * asdouble(0u, (uint)(k + 1023) << 20)); }
float tb_rto(double s) { float f = (float)s; if ((double)f != s) { uint u = asuint(f); if (abs((double)f) > abs(s)) u -= 1u; f = asfloat(u | 1u); } return f; }
float tb_fma16(float a, float b, float c) { precise float p = a * b; precise float s = p + c; if ((asuint(s) & 0x7fffffffu) >= 0x7f800000u) return s; precise float bb = s - p; precise float e = (p - (s - bb)) + (c - bb); uint u = asuint(s); if (e != 0.0) { if ((e > 0.0) != (s > 0.0)) u -= 1u; u |= 1u; } return asfloat(u); }
float tb_fma_rto(float a, float b, float c) { precise double p = (double)a * (double)b; precise double q = (double)c; precise double s = p + q; if (!(abs(s) < 1.0e300)) return (float)s; precise double bb = s - p; precise double e = (p - (s - bb)) + (q - bb); float f = (float)s; uint u = asuint(f); if ((double)f != s) { if (abs((double)f) > abs(s)) u -= 1u; u |= 1u; } else if (e != 0.0) { if ((e > 0.0) != (s > 0.0)) u -= 1u; u |= 1u; } return asfloat(u); }
float tb_mull(float a, float b) { return (a == 0.0 || b == 0.0) ? 0.0 : a * b; }
#ifdef TB_FAST_FMA
float tb_fma(float a, float b, float c) { precise float r = mad(a, b, c); uint m = 0x7fffffffu; uint xa = asuint(a) & m, xb = asuint(b) & m, xc = asuint(c) & m; if (max(max(xa, xb), xc) > 0x7f800000u) return asfloat(((xa > 0x7f800000u) ? asuint(a) : ((xb > 0x7f800000u) ? asuint(b) : asuint(c))) | 0x400000u); return ((asuint(r) & m) > 0x7f800000u) ? asfloat(0x7fc00000u) : r; }
#elif defined(TB_HYBRID_FMA)
float tb_fma(float a, float b, float c) { precise float r0 = mad(a, b, c); uint m0 = 0x7fffffffu; uint ya = asuint(a) & m0, yb = asuint(b) & m0, yc = asuint(c) & m0, yr = asuint(r0) & m0; if ((ya == 0u || (ya >= 0x00800000u && ya < 0x7f800000u)) && (yb == 0u || (yb >= 0x00800000u && yb < 0x7f800000u)) && (yc == 0u || (yc >= 0x00800000u && yc < 0x7f800000u)) && (ya == 0u || yb == 0u || (ya >> 23) + (yb >> 23) >= 129u) && yr >= 0x01000000u && yr < 0x7f000000u) return r0; precise double p = (double)a * (double)b; precise double dc = (double)c; precise double s = p + dc; precise double bb = s - p; precise double e = (p - (s - bb)) + (dc - bb); uint lo, hi; asuint(s, lo, hi); uint fin = (uint)(abs(s) < 1.0e300); uint adj = fin & (uint)(e != 0.0) & (~lo & 1u); uint dn = adj & (uint)((e > 0.0) != (s > 0.0)); precise float r = (float)asdouble(lo + adj - 2u * dn, hi - (dn & (uint)(lo == 0u))); uint m = 0x7fffffffu; float q = ((asuint(r) & m) > 0x7f800000u) ? asfloat(0x7fc00000u) : r; float sp = (max(max(asuint(a) & m, asuint(b) & m), asuint(c) & m) > 0x7f800000u) ? asfloat((((asuint(a) & m) > 0x7f800000u) ? asuint(a) : (((asuint(b) & m) > 0x7f800000u) ? asuint(b) : asuint(c))) | 0x400000u) : q; return ((fin | (uint)(abs(p) < 1.0e300)) != 0u) ? r : sp; }
#else
float tb_fma(float a, float b, float c) { precise double p = (double)a * (double)b; precise double dc = (double)c; precise double s = p + dc; precise double bb = s - p; precise double e = (p - (s - bb)) + (dc - bb); uint lo, hi; asuint(s, lo, hi); uint fin = (uint)(abs(s) < 1.0e300); uint adj = fin & (uint)(e != 0.0) & (~lo & 1u); uint dn = adj & (uint)((e > 0.0) != (s > 0.0)); precise float r = (float)asdouble(lo + adj - 2u * dn, hi - (dn & (uint)(lo == 0u))); uint m = 0x7fffffffu; float q = ((asuint(r) & m) > 0x7f800000u) ? asfloat(0x7fc00000u) : r; float sp = (max(max(asuint(a) & m, asuint(b) & m), asuint(c) & m) > 0x7f800000u) ? asfloat((((asuint(a) & m) > 0x7f800000u) ? asuint(a) : (((asuint(b) & m) > 0x7f800000u) ? asuint(b) : asuint(c))) | 0x400000u) : q; return ((fin | (uint)(abs(p) < 1.0e300)) != 0u) ? r : sp; }
#endif
bool tb_nan(float x) { return (asuint(x) & 0x7fffffffu) > 0x7f800000u; }
float tb_qn(float x) { return tb_nan(x) ? asfloat(0x7fc00000u) : x; }
#if defined(__HLSL_VERSION) && __HLSL_VERSION >= 2021
#define TB_SEL(c, a, b) select((c), (a), (b))
#else
#define TB_SEL(c, a, b) ((c) ? (a) : (b))
#endif
float tb_u2f(uint a) { precise float r = (float)(a & 0xffff0000u) + (float)(a & 0xffffu); return r; }
float tb_log(float x) { uint b = asuint(x); return tb_qn((b == 0x7f800000u) ? x : TB_SEL(b - 0x00800000u < 0x7f000000u && (b & 0x7fffffu) == 0u, (float)((int)(b >> 23) - 127), log2(x))); }
float tb_fsub(float a, float b) { precise float r = a - b; uint ua = asuint(a), ub = asuint(b), m = 0x7fffffffu; return ((ua & m) > 0x7f800000u || (ub & m) > 0x7f800000u || ((ua & m) == 0x7f800000u && ua == ub)) ? asfloat(0x7fc00000u) : r; }
float tb_hz(float r, uint s) { uint b = asuint(r); return asfloat((b & 0x7fffffffu) == 0u ? s : b); }
float tb_pz(float r) { uint b = asuint(r); return asfloat((b & 0x7fffffffu) == 0u ? 0u : b); }
float tb_pzf(float r, float a, float b) { uint m = 0x7fffffffu; return ((asuint(a) & m) == 0u || (asuint(b) & m) == 0u) ? tb_pz(r) : r; }
float tb_hadd(float a, float b) { uint ua = asuint(a), ub = asuint(b); return tb_hz(a + b, ((ua | ub) & 0x7fffffffu) == 0u ? (ua & ub & 0x80000000u) : 0u); }
float tb_hsub(float a, float b) { uint ua = asuint(a), ub = asuint(b); return tb_hz(a - b, ((ua | ub) & 0x7fffffffu) == 0u ? (ua & ~ub & 0x80000000u) : 0u); }
float tb_hmul(float a, float b) { return tb_hz(a * b, (asuint(a) ^ asuint(b)) & 0x80000000u); }
float tb_fract(float x) { if ((asuint(x) & 0x7fffffffu) >= 0x7f800000u) return asfloat(0x7fc00000u); precise float f = x - floor(x); return min(f, asfloat(0x3f7fffffu)); }
bool tb_isinf32(float x) { return abs(x) == asfloat(0x7f800000u); }
int tb_exp32(float x) { return (int)((asuint(x) >> 23) & 0xffu); }
bool tb_sub32(float x) { return x != 0.0 && abs(x) < asfloat(0x00800000u); }
float tb_div_fixup32(float q, float d, float n) { uint db = asuint(d), nb = asuint(n); uint so = (db ^ nb) & 0x80000000u; if (tb_nan(n)) return asfloat(nb | 0x00400000u); if (tb_nan(d)) return asfloat(db | 0x00400000u); if ((d == 0.0 && n == 0.0) || (tb_isinf32(d) && tb_isinf32(n))) return asfloat(0x7fc00000u); if (d == 0.0 || tb_isinf32(n)) return asfloat(so | 0x7f800000u); if (tb_isinf32(d) || n == 0.0) return asfloat(so); if (tb_exp32(n) - tb_exp32(d) < -150) return asfloat(so); if (tb_nan(q)) return asfloat(so | 0x7f800000u); return asfloat((asuint(q) & 0x7fffffffu) | so); }
float tb_div_scale32(float x, float d, float n, out uint vcc) { vcc = 0u; if (d == 0.0 || n == 0.0) { vcc = 1u; return asfloat(0x7fc00000u); } int en = tb_exp32(n), ed = tb_exp32(d); if (en - ed >= 96) { vcc = 1u; return (x == d) ? tb_ldexp32(x, 64) : x; } if (tb_sub32(d)) return tb_ldexp32(x, 64); bool tr = abs(1.0 / (double)d) < asdouble(0u, 0x38100000u); bool tq = abs((double)n / (double)d) < asdouble(0u, 0x38100000u); if (tr && tq) { vcc = 1u; return (x == d) ? tb_ldexp32(x, -64) : x; } if (tr) return tb_ldexp32(x, -64); if (tq) { vcc = 1u; return (x == n) ? tb_ldexp32(x, 64) : x; } if (en <= 23) return tb_ldexp32(x, 64); return x; }
float tb_div_fmas32(float a, float b, float c, uint vcc) { precise float r = tb_fma(a, b, c); if ((vcc & 1u) == 0u) return r; float up = asfloat(0x5f800000u), dn = asfloat(0x1f800000u); if (tb_exp32(c) > 127) { if (!tb_sub32(r)) return r * up; bool sa = abs(a) <= abs(b); return tb_fma((sa ? a : b) * up, sa ? b : a, c * up); } precise float q = r * dn; if (r == 0.0 || !(abs(r) < asfloat(0x20800000u))) return q; precise float df = r - q * up; if (abs(df) != asfloat(0x14800000u)) return q; precise float p = a * b; precise float pe = tb_fma(a, b, -p); precise float s1 = p + c; precise float bb = s1 - p; precise float s1e = (p - (s1 - bb)) + (c - bb); precise float e = (s1 - r) + (s1e + pe); if (e == 0.0 || ((e > 0.0) != (df > 0.0))) return q; return q + ((df > 0.0) ? asfloat(1u) : -asfloat(1u)); }
#ifdef TB_FAST_F2H
uint tb_f2h(float x) { uint b = asuint(x); return (b & 0x7fffffffu) > 0x7f800000u ? (((b >> 16) & 0x8000u) | 0x7e00u | ((b >> 13) & 0x3ffu)) : (uint)asuint16((float16_t)x); }
#else
uint tb_f2h(float x) { uint b = asuint(x); uint s = (b >> 16) & 0x8000u; uint a = b & 0x7fffffffu; if (a > 0x7f800000u) return s | 0x7e00u | ((a >> 13) & 0x3ffu); if (a >= 0x477ff000u) return s | 0x7c00u; if (a >= 0x38800000u) { uint r = a - 0x38000000u; return s | ((r + 0xfffu + ((r >> 13) & 1u)) >> 13); } if (a < 0x33000000u) return s; uint e = a >> 23; uint m = (a & 0x7fffffu) | 0x800000u; uint sh = 126u - e; uint q = m >> sh; uint rem = m & ((1u << sh) - 1u); uint hf = 1u << (sh - 1u); if (rem > hf || (rem == hf && (q & 1u) != 0u)) q += 1u; return s | q; }
#endif
float tb_rne(float x) { float a = abs(x); if (!(a < 8388608.0)) return x; float r = floor(a); float f = a - r; if (f > 0.5 || (f == 0.5 && (((uint)r) & 1u) != 0u)) r += 1.0; return asfloat(asuint(r) | (asuint(x) & 0x80000000u)); }
float tb_flr(float x) { return TB_SEL(asuint(x) - 0x80000001u < 0x7fffffu, -1.0, floor(x)); }
float tb_floor(float x) { uint b = asuint(x); return TB_SEL((b & 0x7fffffffu) > 0x7f800000u, asfloat(b | 0x400000u), tb_flr(x)); }
float tb_ceil(float x) { uint b = asuint(x); return TB_SEL((b & 0x7fffffffu) > 0x7f800000u, asfloat(b | 0x400000u), TB_SEL(b - 1u < 0x7fffffu, 1.0, ceil(x))); }
float tb_trunc(float x) { uint b = asuint(x); return TB_SEL((b & 0x7fffffffu) > 0x7f800000u, asfloat(b | 0x400000u), trunc(x)); }
uint tb_cvt_i(float x) { if (tb_nan(x)) return 0u; if (x >= 2147483648.0) return 0x7fffffffu; if (x <= -2147483648.0) return 0x80000000u; return asuint((int)x); }
uint tb_cvt_flr_i(float x) { uint b = asuint(x); if ((b & 0x7fffffffu) > 0x7f800000u) return TB_SEL((b & 0x80000000u) != 0u, 0x80000000u, 0x7fffffffu); return tb_cvt_i(tb_flr(x)); }
uint tb_cvt_rpi_i(float x) { uint b = asuint(x); if ((b & 0x7fffffffu) > 0x7f800000u) return TB_SEL((b & 0x80000000u) != 0u, 0x80000000u, 0x7fffffffu); precise float r = floor(x); precise float d = x - r; return tb_cvt_i(TB_SEL(d >= 0.5, r + 1.0, r)); }
uint tb_cvt_u(float x) { if (tb_nan(x) || x <= 0.0) return 0u; if (x >= 4294967296.0) return 0xffffffffu; return (uint)x; }
#if defined(__HLSL_VERSION) && __HLSL_VERSION >= 2021
#define TB_WV(e, d) select((exec_lo & 1u) != 0u, (uint)(e), (d))
#else
#define TB_WV(e, d) (((exec_lo & 1u) != 0u) ? (uint)(e) : (d))
#endif
uint tb_f16rtz(float x) { uint h = f32tof16(x); float b = f16tof32(h); if (abs(b) > abs(x) && (h & 0x7fffu) != 0u) h = (h & 0x8000u) | ((h & 0x7fffu) - 1u); return h; }
float tb_rtfmt(float x, uint k, uint c) { if (k == 0u) return x; uint kk = k > 2u ? k - 2u : k; float h = f16tof32(tb_f16rtz(x)); if (kk == 2u && h <= 65504.0) h = min(h, c == 2u ? 64512.0 : 65024.0); return h; }
float tb_psclamp(float x, uint k, uint c) { if (k == 3u) return isnan(x) ? 65504.0 : clamp(x, -65504.0, 65504.0); if (k == 4u) { float m = c == 2u ? 64512.0 : 65024.0; return isnan(x) ? m : min(x, m); } return x; }
#ifndef TB_RTFMT0
#define TB_RTFMT0 0u
#endif
#ifndef TB_RTFMT1
#define TB_RTFMT1 0u
#endif
#ifndef TB_RTFMT2
#define TB_RTFMT2 0u
#endif
#ifndef TB_RTFMT3
#define TB_RTFMT3 0u
#endif
#ifndef TB_RTFMT4
#define TB_RTFMT4 0u
#endif
#ifndef TB_RTFMT5
#define TB_RTFMT5 0u
#endif
#ifndef TB_RTFMT6
#define TB_RTFMT6 0u
#endif
#ifndef TB_RTFMT7
#define TB_RTFMT7 0u
#endif
"#;

pub(crate) fn small_float_store_fixup(addr: &str, v: &str) -> String {
    let mut s = String::new();
    s.push_str("\n#if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6\n");
    s.push_str(&format!("if (abs({v}.x) > 64512.0 || abs({v}.y) > 64512.0 || abs({v}.z) > 64512.0 || abs({v}.w) > 64512.0) {{ for (uint tbk = 0u; tbk < 3u; tbk++) {{ float4 tbr = {addr}; float tbm = tbk == 0u ? 65504.0 : (tbk == 1u ? 65024.0 : 64512.0); bool tbo = false; "));
    for ch in ["x", "y", "z", "w"] {
        s.push_str(&format!("if (isinf(tbr.{ch}) && !isinf({v}.{ch})) {{ {v}.{ch} = clamp({v}.{ch}, -tbm, tbm); tbo = true; }} "));
    }
    s.push_str(&format!("if (!tbo) break; {addr} = {v}; }} }}"));
    s.push_str("\n#endif\n");
    s
}

fn name(o: &Opnd) -> Result<String, String> {
    Ok(match o {
        Opnd::Sgpr(n) => format!("s{n}"),
        Opnd::Vgpr(n) => format!("v{n}"),
        Opnd::VccLo => "vcc_lo".into(),
        Opnd::VccHi => "vcc_hi".into(),
        Opnd::ExecLo => "exec_lo".into(),
        Opnd::ExecHi => "exec_hi".into(),
        Opnd::M0 => "m0".into(),
        Opnd::Scc => "scc".into(),
        Opnd::Null => "nul".into(),
        Opnd::Temp(n) => format!("tb_s{n}"),
        other => return Err(format!("operand {other:?} is not assignable")),
    })
}

/// Operand as a `uint` rvalue expression.
fn r(o: &Opnd) -> Result<String, String> {
    Ok(match o {
        Opnd::Null => "0u".into(),
        Opnd::ConstInt(v) => format!("{:#x}u", *v as i32 as u32),
        Opnd::ConstF32(f) => format!("{:#x}u", f.to_bits()),
        Opnd::Literal(v) => format!("{v:#x}u"),
        Opnd::Raw(c) => return Err(format!("unmodelled operand code {c}")),
        other => name(other)?,
    })
}

fn f(o: &Opnd) -> Result<String, String> {
    Ok(format!("asfloat({})", r(o)?))
}

fn sat_int(e: &str, bits: u32) -> String {
    let h = 1u32 << (bits - 1);
    format!("(min(asuint(max({e}, -{h})) + {h:#x}u, {:#x}u) ^ {h:#x}u)", (h << 1) - 1)
}

fn int24_clamped(t: usize, m: &str, a: &str, b: &str, c: &str) -> (String, String) {
    let signed = m.ends_with("_i24");
    let field = |x: &str| if signed { format!("asuint(asint({x} << 8) >> 8)") } else { format!("({x} & 0xffffffu)") };
    let mulhi = if signed { "tb_mulhi_i32" } else { "tb_mulhi_u32" };
    let mut setup = format!(
        "uint t{t}a = {}, t{t}b = {}; uint t{t}l = t{t}a * t{t}b; uint t{t}h = {mulhi}(t{t}a, t{t}b);",
        field(a),
        field(b)
    );
    if m.starts_with("v_mad") {
        let sign = if signed { format!(" + ((asint({c}) < 0) ? 0xffffffffu : 0u)") } else { String::new() };
        setup.push_str(&format!(" uint t{t}s = t{t}l + {c}; t{t}h += ((t{t}s < t{t}l) ? 1u : 0u){sign}; t{t}l = t{t}s;"));
    }
    let e = if signed {
        format!("((t{t}h == asuint(asint(t{t}l) >> 31)) ? t{t}l : ((asint(t{t}h) < 0) ? 0x80000000u : 0x7fffffffu))")
    } else {
        format!("((t{t}h != 0u) ? 0xffffffffu : t{t}l)")
    };
    (setup, e)
}

fn f16_inline(x: f32) -> u32 {
    if (x - 0.159_154_94).abs() < 1e-7 {
        return 0x3118;
    }
    let b = x.to_bits();
    ((b >> 16) & 0x8000) | ((((b >> 23) & 0xff) - 112) << 10)
}

fn f16_inline_val(x: f32, hi: bool) -> String {
    match (hi, f16_inline(x)) {
        (true, _) => "0.0".into(),
        (false, 0x3118) => "f16tof32(0x3118u)".into(),
        _ => format!("({x:?})"),
    }
}

fn pk_int16(m: &str, ins: &Insn) -> Result<Option<String>, String> {
    let (nsrc, signed) = match m {
        "v_pk_mad_i16" => (3, true),
        "v_pk_mad_u16" => (3, false),
        "v_pk_add_i16" | "v_pk_sub_i16" | "v_pk_max_i16" | "v_pk_min_i16" | "v_pk_ashrrev_i16" => (2, true),
        "v_pk_mul_lo_u16" | "v_pk_add_u16" | "v_pk_sub_u16" | "v_pk_max_u16" | "v_pk_min_u16" | "v_pk_lshlrev_b16" | "v_pk_lshrrev_b16" => (2, false),
        _ => return Ok(None),
    };
    let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
    if (w1 >> 29) & 7 != 0 || (w0 >> 8) & 7 != 0 {
        return Err(format!("{m}: NEG/NEG_HI on an integer packed op"));
    }
    let clamp = (w0 >> 15) & 1 != 0;
    if clamp && !matches!(m, "v_pk_add_i16" | "v_pk_sub_i16" | "v_pk_add_u16" | "v_pk_sub_u16" | "v_pk_mad_u16" | "v_pk_mad_i16" | "v_pk_max_i16" | "v_pk_min_i16" | "v_pk_max_u16" | "v_pk_min_u16") {
        return Err(format!("{m}: CLAMP on an integer packed op without a defined saturation"));
    }
    let src: Vec<&Opnd> = (0..nsrc).map(|i| ins.src.get(i).ok_or_else(|| format!("{m}: missing source {i}"))).collect::<Result<_, _>>()?;
    let sel = [(w0 >> 11) & 7, ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2)];
    let mut halves = Vec::new();
    for h in sel {
        let mut u = Vec::new();
        let mut i = Vec::new();
        for (k, o) in src.iter().enumerate() {
            let x = match o {
                Opnd::ConstF32(f) => format!("{:#x}u", f.to_bits()),
                _ => r(o)?,
            };
            if (h >> k) & 1 != 0 {
                u.push(format!("(({x}) >> 16)"));
                i.push(format!("(asint({x}) >> 16)"));
            } else {
                u.push(format!("(({x}) & 0xffffu)"));
                i.push(format!("(asint(({x}) << 16) >> 16)"));
            }
        }
        let v = if signed { &i } else { &u };
        let e = match m {
            "v_pk_lshlrev_b16" => format!("({} << ({} & 15u))", u[1], u[0]),
            "v_pk_lshrrev_b16" => format!("({} >> ({} & 15u))", u[1], u[0]),
            "v_pk_ashrrev_i16" => format!("asuint({} >> (int)({} & 15u))", i[1], u[0]),
            "v_pk_mul_lo_u16" => format!("({} * {})", v[0], v[1]),
            "v_pk_mad_u16" if clamp => format!("min({} * {} + {}, 0xffffu)", v[0], v[1], v[2]),
            "v_pk_mad_u16" => format!("({} * {} + {})", v[0], v[1], v[2]),
            "v_pk_mad_i16" if clamp => sat_int(&format!("{} * {} + {}", v[0], v[1], v[2]), 16),
            "v_pk_mad_i16" => format!("asuint({} * {} + {})", v[0], v[1], v[2]),
            "v_pk_add_u16" if clamp => format!("min({} + {}, 0xffffu)", v[0], v[1]),
            "v_pk_add_u16" => format!("({} + {})", v[0], v[1]),
            "v_pk_sub_u16" if clamp => format!("asuint(max((int){} - (int){}, 0))", v[0], v[1]),
            "v_pk_sub_u16" => format!("({} - {})", v[0], v[1]),
            "v_pk_add_i16" if clamp => sat_int(&format!("{} + {}", v[0], v[1]), 16),
            "v_pk_add_i16" => format!("asuint({} + {})", v[0], v[1]),
            "v_pk_sub_i16" if clamp => sat_int(&format!("{} - {}", v[0], v[1]), 16),
            "v_pk_sub_i16" => format!("asuint({} - {})", v[0], v[1]),
            "v_pk_max_i16" => format!("asuint(max({}, {}))", v[0], v[1]),
            "v_pk_min_i16" => format!("asuint(min({}, {}))", v[0], v[1]),
            "v_pk_max_u16" => format!("max({}, {})", v[0], v[1]),
            _ => format!("min({}, {})", v[0], v[1]),
        };
        halves.push(format!("({e} & 0xffffu)"));
    }
    Ok(Some(format!("({} | ({} << 16))", halves[0], halves[1])))
}

/// Scalar register number of an operand (SGPRs, plus vcc_lo/vcc_hi = 106/107, which SMEM
/// loads can target directly: `s_buffer_load_dwordx2 vcc_lo, ...`).
fn sreg_num(o: &Opnd) -> Option<u8> {
    match o {
        Opnd::Sgpr(n) => Some(*n),
        Opnd::VccLo => Some(106),
        Opnd::VccHi => Some(107),
        _ => None,
    }
}

fn sreg_opnd(n: u8) -> Opnd {
    match n {
        106 => Opnd::VccLo,
        107 => Opnd::VccHi,
        n => Opnd::Sgpr(n),
    }
}

pub(super) fn i32_result_of_64(m: &str) -> bool {
    m.starts_with("s_") && m.contains("_i32_") && (m.ends_with("_b64") || m.ends_with("_i64"))
}

/// High half of a 64-bit scalar operand.
fn hi(o: &Opnd) -> Opnd {
    match o {
        Opnd::Sgpr(n) => Opnd::Sgpr(n + 1),
        Opnd::VccLo => Opnd::VccHi,
        Opnd::ExecLo => Opnd::ExecHi,
        Opnd::ConstInt(v) => Opnd::ConstInt(if *v < 0 { -1 } else { 0 }),
        Opnd::Null => Opnd::Null,
        _ => Opnd::ConstInt(0),
    }
}

/// Name of the SGPR used as an SMEM dynamic offset (SOFFSET field).
fn ssrc_name(code: u32) -> Result<String, String> {
    match code {
        0..=105 => Ok(format!("s{code}")),
        106 => Ok("vcc_lo".into()),
        107 => Ok("vcc_hi".into()),
        124 => Ok("m0".into()),
        _ => Err(format!("unsupported SOFFSET register code {code}")),
    }
}

fn ps_clamp_lines(osig: &[SigElement]) -> String {
    let targets: Vec<(usize, u32, usize)> = osig
        .iter()
        .enumerate()
        .filter(|(_, e)| e.sysval == 64 && !matches!(e.comp_type, 1 | 2) && e.index <= 7)
        .map(|(ei, e)| (ei, e.index, sig_type(e).1))
        .collect();
    ps_clamp_epilogue(&targets)
}

pub fn ps_clamp_epilogue(targets: &[(usize, u32, usize)]) -> String {
    let mut out = String::new();
    for &(ei, index, comps) in targets {
        for (c, x) in XYZW.iter().enumerate().take(comps.min(4)) {
            out.push_str(&format!("    o.o{ei}.{x} = tb_psclamp(o.o{ei}.{x}, TB_RTFMT{index}, {c}u);\n"));
        }
    }
    out
}

pub fn ps_helpers() -> &'static str {
    CUBE_HELPERS
}

fn sig_type(e: &SigElement) -> (String, usize) {
    // Component count = number of mask bits (elements can be packed into a shared register,
    // e.g. TEXCOORD4.xy + TEXCOORD5.zw, so the highest bit is not the count).
    let comps = (e.mask.count_ones() as usize).max(1);
    let base = match e.comp_type {
        1 => "uint",
        2 => "int",
        _ => "float",
    };
    (if comps == 1 { base.to_string() } else { format!("{base}{comps}") }, comps)
}

/// Map a hardware (register, component) to (signature element index, component within
/// that element). RDNA2 attributes / parameter exports are whole registers; a register may
/// hold several packed signature elements.
fn sig_lookup(sig: &[SigElement], register: u32, chan: usize) -> Option<(usize, usize)> {
    sig.iter().enumerate().find_map(|(idx, e)| {
        if e.sysval == 0 && e.register == register && (e.mask >> chan) & 1 != 0 {
            Some((idx, (e.mask & ((1u8 << chan) - 1)).count_ones() as usize))
        } else {
            None
        }
    })
}

/// Distinct registers of the given elements, in signature order.
fn distinct_regs<'s>(it: impl Iterator<Item = &'s SigElement>) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::new();
    for e in it {
        if !v.contains(&e.register) {
            v.push(e.register);
        }
    }
    v
}

fn semantic(e: &SigElement) -> String {
    format!("{}{}", e.name, e.index)
}

fn ps_input_modifier(ex: &ShaderExtract, ei: usize, e: &SigElement) -> &'static str {
    let cons_depth = || {
        ex.output_sig.iter().any(|o| {
            let s = semantic(o);
            s.starts_with("SV_DepthLessEqual") || s.starts_with("SV_DepthGreaterEqual")
        })
    };
    if e.sysval == 1 && cons_depth() {
        "linear noperspective centroid "
    } else if e.comp_type != 3 {
        "nointerpolation "
    } else if e.name.contains("_centroid") {
        centroid::qualifier(ex, ei)
    } else {
        ""
    }
}

pub fn ps_input_interp(ex: &ShaderExtract) -> InputInterp {
    if ex.stage != Stage::Pixel {
        return InputInterp::default();
    }
    let modifiers = ex.input_sig.iter().enumerate().filter(|(_, e)| e.sysval != 1).filter_map(|(ei, e)| {
        let m = ps_input_modifier(ex, ei, e).trim_end();
        let default = if e.comp_type == 3 { "" } else { "nointerpolation" };
        (m != default).then(|| (semantic(e), m.to_string()))
    });
    InputInterp(modifiers.collect())
}

struct Emitter<'a> {
    ex: &'a ShaderExtract,
    insns: Vec<Insn>,
    res: HashMap<usize, Vec<Res>>, // per-instruction resolved resources
    cb_size: BTreeMap<u32, u32>,   // slot -> dwords
    vs_lat: HashMap<usize, [smem_bounds::Lat; 3]>,
    cfg: Option<(Vec<std::ops::Range<usize>>, Vec<Vec<usize>>)>,
    sampler_literals: Option<HashMap<usize, [u32; 4]>>,
    uses_smem_bounds: bool,
    cb_lim: HashMap<u32, Option<u64>>,
    tex: BTreeMap<u32, u32>,       // slot -> dim
    fmask: BTreeMap<u32, u32>,
    fmask_loc: BTreeSet<(u32, u32, u32)>,
    gather_ssamp: HashMap<usize, Option<(u32, u32)>>,
    gather_blind: BTreeSet<u32>,
    samp: BTreeSet<u32>,
    samp_cmp: BTreeSet<u32>,
    uav: BTreeMap<u32, u32>,       // slot -> dim
    sbuf: BTreeMap<(bool, u32), u32>, // (is_uav, slot) -> element dwords used
    fssm_strides: BTreeMap<(bool, u32), u32>,
    tbuf: BTreeSet<(bool, u32)>,      // typed buffers (is_uav, slot)
    tbuf_uint: BTreeSet<u32>,         // typed UAV slots whose element must be uint (atomics)
    /// Inferred integer element type of typed buffers: (is_uav, slot) -> 1 uint, 2 sint.
    tbuf_ty: BTreeMap<(bool, u32), u8>,
    /// Inferred integer texel type of textures read with texel fetches / written with image
    /// stores: (is_uav, slot) -> 1 uint, 2 sint. Absent = float (the default).
    img_ty: BTreeMap<(bool, u32), u8>,
    /// Pixel-shader colour-export epilogues (see `find_export_epilogs`): pack instruction
    /// index -> capture id, and export instruction index -> the capture ids feeding its
    /// two packed operands (usize::MAX = operand not written).
    epilog_pack: HashMap<usize, usize>,
    epilog_exp: HashMap<usize, (usize, usize)>,
    /// Index of the instruction being emitted.
    cur_idx: usize,
    /// Structured buffers read with scalar loads (`s_buffer_load` through an SRV/UAV
    /// descriptor): they need the byte-offset helper.
    sbuf_flat: BTreeSet<(bool, u32)>,
    is_cs: bool,
    /// Nesting depth of emitted HLSL loops at the current instruction.
    loop_depth: u32,
    is_gs: bool,
    /// Compute shader uses LDS (groupshared memory).
    uses_lds: bool,
    lds_w_pending: bool,
    lds_r_pending: bool,
    lds_nested: bool,
    lds64_nested: bool,
    lds64_n: Option<u32>,
    brace_depth: i32,
    /// Shader reads constant tables embedded in its code section.
    uses_code: bool,
    vreg_array: bool,
    d16: Option<(String, bool)>,
    /// Interned candidate lists `(table, offset)` for `Sym::Multi`.
    multi_sets: Vec<Vec<(u8, u32)>>,
    /// Image ops whose texture and/or sampler is one of several descriptors.
    /// UAV texture slots used with image atomics (declared `<uint>`).
    uav_uint: BTreeSet<u32>,
    uav_u64: BTreeSet<u32>,
    tbuf_u64: BTreeSet<u32>,
    sbuf_u64: BTreeSet<u32>,
    /// The shader uses wave intrinsics (needs shader model 6: DXC only).
    uses_wave: bool,
    wave64: bool,
    msel: HashMap<usize, MSel>,
    /// Buffer ops (MUBUF, s_buffer_load) whose descriptor register holds one of several
    /// descriptors: (register key, candidates as (descriptor id, resource)).
    bsel: HashMap<usize, (u16, Vec<(u32, Res)>)>,
    /// Descriptor loads `(base register, descriptor id)` and base-dword moves `(dst, src)`
    /// per instruction, for maintaining the run-time `tb_dsel<reg>` selectors.
    dsel_loads: HashMap<usize, (u16, u32)>,
    dsel_moves: HashMap<usize, (u16, u16)>,
    /// Registers read before written (entry ABI), from `live_in`.
    live: Vec<String>,
    /// Pixel shader needs its position within the 2x2 quad (ds_swizzle).
    uses_quad: bool,
    vs_lds_out: bool,
    regs: BTreeSet<String>,
    body: String,
    tmp: usize,
    /// PS attribute index -> input register (registers the shader actually reads).
    ps_attrs: Vec<u32>,
    /// VS parameter-export index -> output register.
    out_params: Vec<u32>,
    prim_params: Vec<u32>,
    user_base_override: Option<usize>,
    durango: Option<Vec<(u32, u32)>>,
    rm_mode: std::cell::Cell<Option<bool>>,
    rm_loads: BTreeMap<usize, u32>,
    rm_used: BTreeSet<usize>,
    bl: Vec<(u8, u32)>,
    bl_loads: HashMap<usize, (u32, u8)>,
    bl_const: HashMap<u32, u32>,
    uses_f64: bool,
    uses_d16: bool,
    uses_trans16: bool,
    uses_trans32: bool,
    uav_mips: BTreeSet<u32>,
    coherent_read: BTreeSet<u32>,
    uav_written: BTreeSet<u32>,
    tex_mips: BTreeSet<u32>,
    sm6_minor: u32,
    hw_regs: BTreeSet<u32>,
    uav_force: BTreeSet<u32>,
    bl_uav: BTreeSet<u32>,
    bl_srv: BTreeSet<u32>,
    uses_bvh: bool,
    uses_global: bool,
    uses_scratch: bool,
    mode_use: mode::ModeUse,
    init_mode: u32,
    out_interp: InputInterp,
    user_data: bool,
    unknowns: BTreeSet<String>,
}

/// SGPR spills through vector lanes.
///
/// When a shader runs out of scalar registers the console compiler parks scalars (observed:
/// texture/sampler descriptor dwords) in individual lanes of an otherwise unused VGPR with
/// `v_writelane_b32 vN, sX, <lane>` and brings them back with `v_readlane_b32 sY, vN, <lane>`
/// (VOP3 0x361 / 0x360; fixture `fortnite_lobby_desc.dxbc`). The translator models one lane
/// per VGPR, so each (VGPR, constant lane) slot is lowered to a function-scope translator
/// temporary and the two instructions become plain `s_mov_b32`. The descriptor dataflow
/// tracks those temporaries like registers (keys 1000+), so descriptors survive a spill.
///
/// Refused (explicit error): a non-constant lane index, a read of a slot that was never
/// written, or a spill VGPR that is also used as an ordinary vector.
/// Synthetic opcode of the wave-reduction instruction produced by `lower_wave_reductions`.
const OP_WAVE_REDUCE: u32 = 0x7ff0;

const BINDLESS: u32 = 0x8000_0000;

const OP_WAVE_SCAN: u32 = 0x7ff1;

pub const TB_MIP_SPACE: u32 = 4_000_000;
pub const TB_GMEM_SPACE: u32 = 4_000_001;
pub const TB_BLALIAS_SPACE: u32 = 4_200_000;
pub const BL_ALIASES: u32 = 6;

pub fn bl_alias_space(k: u32, space: u32) -> u32 {
    TB_BLALIAS_SPACE + (k - 1) * 100_000 + space
}

pub fn dg_vertex_id_on() -> bool {
    static M: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("TOMBOX_XS_DG_VERTEXID").map(|v| v.trim() != "0").unwrap_or(true))
}

pub fn msaa_dim_on() -> bool {
    static M: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("TOMBOX_XS_MSAA_DIM").map(|v| v.trim() != "0").unwrap_or(true))
}

pub fn psv_image_dim_ms(kind: u32, da_dim: u32, ms: bool) -> u32 {
    if ms && kind == 3 && da_dim == 1 {
        return 6;
    }
    psv_image_dim(kind, da_dim)
}

pub fn psv_image_dim(kind: u32, da_dim: u32) -> u32 {
    match (kind, da_dim) {
        (1, 1) => 0,
        (4, 1) => 2,
        (5, 1) => 3,
        (6, 5) => 4,
        _ => da_dim,
    }
}

fn bl_name(table: u8, key: usize) -> String {
    match key >> 16 {
        0 => format!("tb_bl{table}_{}", key & 0xffff),
        k => format!("tb_bl{table}_{}_a{k}", key & 0xffff),
    }
}

const BVH_HELPERS: &str = r#"ByteAddressBuffer tb_gmem : register(t0, spaceTB_GMEM_SPACE);
cbuffer TbGmem : register(b0, spaceTB_GMEM_SPACE) { uint4 tb_gmem_base; };
uint tb_gmoff(uint64_t va) { return (uint)(va - ((((uint64_t)tb_gmem_base.y) << 32) | (uint64_t)tb_gmem_base.x)); }
float3 tb_gm3(uint o) { return asfloat(tb_gmem.Load3(o)); }
void tb_bvh_swap(inout float da, inout uint ia, inout float db, inout uint ib) { if (db < da) { float tf = da; da = db; db = tf; uint ti = ia; ia = ib; ib = ti; } }
uint4 tb_bvh_intersect(uint4 d, uint64_t node, float tmax, float3 o, float3 dir, float3 inv) {
    uint64_t base = (((uint64_t)d.x) << 8) | (((uint64_t)(d.y & 0xffu)) << 40);
    uint ty = (uint)node & 7u;
    uint off = tb_gmoff(base + ((node >> 3) << 6));
    float inf = asfloat(0x7f800000u);
    if (ty == 4u || ty == 5u) {
        inv = clamp(inv, -asfloat(0x7f7fffffu), asfloat(0x7f7fffffu));
        uint4 ch = tb_gmem.Load4(off);
        uint id[4] = { ch.x, ch.y, ch.z, ch.w };
        float dist[4];
        [unroll] for (uint i = 0u; i < 4u; i++) {
            float3 mn, mx;
            if (ty == 5u) {
                mn = tb_gm3(off + 16u + 24u * i);
                mx = tb_gm3(off + 28u + 24u * i);
            } else {
                uint3 w = tb_gmem.Load3(off + 16u + 12u * i);
                mn = float3(f16tof32(w.x), f16tof32(w.x >> 16), f16tof32(w.y));
                mx = float3(f16tof32(w.y >> 16), f16tof32(w.z), f16tof32(w.z >> 16));
            }
            float3 t0 = (mn - o) * inv;
            float3 t1 = (mx - o) * inv;
            float lo = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
            float hi = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
            bool hit = id[i] != 0xffffffffu && (asuint(mn.x) & 0x7fffffffu) <= 0x7f800000u && hi >= max(lo, 0.0) && lo < tmax;
            dist[i] = hit ? lo : inf;
            id[i] = hit ? id[i] : 0xffffffffu;
        }
        if ((d.y >> 31) != 0u) {
            tb_bvh_swap(dist[0], id[0], dist[1], id[1]);
            tb_bvh_swap(dist[2], id[2], dist[3], id[3]);
            tb_bvh_swap(dist[0], id[0], dist[2], id[2]);
            tb_bvh_swap(dist[1], id[1], dist[3], id[3]);
            tb_bvh_swap(dist[1], id[1], dist[2], id[2]);
        }
        return uint4(id[0], id[1], id[2], id[3]);
    }
    if (ty < 4u) {
        float3 v0 = tb_gm3(off), v1 = tb_gm3(off + 12u), v2 = tb_gm3(off + 24u);
        uint tri = tb_gmem.Load(off + 48u);
        float3 ad = abs(dir);
        uint kz = (ad.x >= ad.y && ad.x >= ad.z) ? 0u : (ad.y >= ad.z ? 1u : 2u);
        uint kx = kz == 2u ? 0u : kz + 1u;
        uint ky = kx == 2u ? 0u : kx + 1u;
        if (dir[kz] < 0.0) { uint tk = kx; kx = ky; ky = tk; }
        float sz = 1.0 / dir[kz];
        float sx = dir[kx] * sz, sy = dir[ky] * sz;
        float3 A = v0 - o, B = v1 - o, C = v2 - o;
        float ax = A[kx] - sx * A[kz], ay = A[ky] - sy * A[kz];
        float bx = B[kx] - sx * B[kz], by = B[ky] - sy * B[kz];
        float cx = C[kx] - sx * C[kz], cy = C[ky] - sy * C[kz];
        float U = cx * by - cy * bx;
        float V = ax * cy - ay * cx;
        float W = bx * ay - by * ax;
        bool miss = ((U < 0.0 || V < 0.0 || W < 0.0) && (U > 0.0 || V > 0.0 || W > 0.0));
        float det = U + (V + W);
        float t = (U * (sz * A[kz]) + V * (sz * B[kz])) + W * (sz * C[kz]);
        miss = miss || V == 0.0 || det == 0.0 || (det < 0.0 ? -t : t) < 0.0;
        if ((d.w >> 24) & 1u)
            return miss ? uint4(asuint(inf), asuint(1.0), 0u, 0u) : uint4(asuint(t), asuint(det), asuint(V), asuint(W));
        return miss ? uint4(asuint(inf), asuint(1.0), tri, 0u) : uint4(asuint(t), asuint(det), tri, 1u);
    }
    return uint4(0xffffffffu, 0xffffffffu, 0xffffffffu, 0xffffffffu);
}
"#;

const F64_HELPERS: &str = r#"static const uint tb_2opi[51] = { 0xA2F983u, 0x6E4E44u, 0x1529FCu, 0x2757D1u, 0xF534DDu, 0xC0DB62u, 0x95993Cu, 0x439041u, 0xFE5163u, 0xABDEBBu, 0xC561B7u, 0x246E3Au, 0x424DD2u, 0xE00649u, 0x2EEA09u, 0xD1921Cu, 0xFE1DEBu, 0x1CB129u, 0xA73EE8u, 0x8235F5u, 0x2EBB44u, 0x84E99Cu, 0x7026B4u, 0x5F7E41u, 0x3991D6u, 0x398353u, 0x39F49Cu, 0x845F8Bu, 0xBDF928u, 0x3B1FF8u, 0x97FFDEu, 0x05980Fu, 0xEF2F11u, 0x8B5A0Au, 0x6D1F6Du, 0x367ECFu, 0x27CB09u, 0xB74F46u, 0x3F669Eu, 0x5FEA2Du, 0x7527BAu, 0xC7EBE5u, 0xF17B3Du, 0x0739F7u, 0x8A5292u, 0xEA6BFBu, 0x5FB11Fu, 0x8D5D08u, 0x560330u, 0x46FC7Bu, 0x6BABF0u };
uint tb_hi64(double x) { uint l, h; asuint(x, l, h); return h; }
uint tb_lo64(double x) { uint l, h; asuint(x, l, h); return l; }
bool tb_isinf64(double x) { return abs(x) == asdouble(0u, 0x7ff00000u); }
double tb_ldexp64(double x, int e) { e = clamp(e, -2200, 2200); for (int i = 0; i < 3; i++) { int k = clamp(e, -1022, 1023); precise double y = x * asdouble(0u, (uint)(k + 1023) << 20); x = y; e -= k; } return x; }
double tb_trunc64(double x) { uint l, h; asuint(x, l, h); int e = (int)((h >> 20) & 0x7ffu) - 1023; if (e >= 52) return x; if (e < 0) return asdouble(0u, h & 0x80000000u); if (e < 20) return asdouble(0u, h & ~(0xfffffu >> (uint)e)); return asdouble(l & ~(0xffffffffu >> (uint)(e - 20)), h); }
double tb_floor64(double x) { double t = tb_trunc64(x); return (t > x) ? t - 1.0L : t; }
double tb_ceil64(double x) { double t = tb_trunc64(x); return (t < x) ? t + 1.0L : t; }
double tb_rndne64(double x) { if ((tb_hi64(x) & 0x7ff00000u) == 0x7ff00000u || abs(x) >= 4503599627370496.0L) return x; double t = tb_floor64(x + 0.5L); if (t - x == 0.5L && ((int64_t)t & 1) != 0) t -= 1.0L; return t == 0.0L ? asdouble(0u, tb_hi64(x) & 0x80000000u) : t; }
double tb_fract64(double x) { if ((tb_hi64(x) & 0x7ff00000u) == 0x7ff00000u) return asdouble(0u, 0x7ff80000u); precise double f = x - tb_floor64(x); return f == 0.0L ? asdouble(0u, 0u) : min(f, asdouble(0xffffffffu, 0x3fefffffu)); }
int tb_frexp_exp64(double x) { uint h = tb_hi64(x); uint e = (h >> 20) & 0x7ffu; if (e == 0x7ffu || x == 0.0L) return 0; if (e == 0u) return (int)((tb_hi64(x * 18014398509481984.0L) >> 20) & 0x7ffu) - 1022 - 54; return (int)e - 1022; }
double tb_frexp_mant64(double x) { uint h = tb_hi64(x); uint e = (h >> 20) & 0x7ffu; if (e == 0x7ffu || x == 0.0L) return x; if (e == 0u) x *= 18014398509481984.0L; return asdouble(tb_lo64(x), (tb_hi64(x) & 0x800fffffu) | 0x3fe00000u); }
double tb_sqrt64(double x) { if (x != x || x == 0.0L || tb_isinf64(x) && x > 0.0L) return x; if (x < 0.0L) return asdouble(0u, 0xfff80000u); int k = 0; if (((tb_hi64(x) >> 20) & 0x7ffu) == 0u) { x *= 18014398509481984.0L; k = -27; } int m = ((int)((tb_hi64(x) >> 20) & 0x7ffu) - 1023) >> 1; double xs = tb_ldexp64(x, -2 * m); double y = (double)sqrt((float)xs); y = 0.5L * (y + xs / y); y = 0.5L * (y + xs / y); y = y + fma(-y, y, xs) * (0.5L / y); return tb_ldexp64(y, m + k); }
bool tb_isnan64(double x) { uint h = tb_hi64(x) & 0x7fffffffu; return h > 0x7ff00000u || (h == 0x7ff00000u && tb_lo64(x) != 0u); }
int tb_exp64(double x) { return (int)((tb_hi64(x) >> 20) & 0x7ffu); }
bool tb_sub64(double x) { return x != 0.0L && abs(x) < asdouble(0u, 0x00100000u); }
double tb_div_fixup64(double q, double d, double n) { uint dh = tb_hi64(d), nh = tb_hi64(n); uint so = (dh ^ nh) & 0x80000000u; if (tb_isnan64(n)) return asdouble(tb_lo64(n), nh | 0x80000u); if (tb_isnan64(d)) return asdouble(tb_lo64(d), dh | 0x80000u); if ((d == 0.0L && n == 0.0L) || (tb_isinf64(d) && tb_isinf64(n))) return asdouble(0u, 0xfff80000u); if (d == 0.0L || tb_isinf64(n)) return asdouble(0u, so | 0x7ff00000u); if (tb_isinf64(d) || n == 0.0L) return asdouble(0u, so); if (tb_exp64(n) - tb_exp64(d) < -1075) return asdouble(0u, so); if (tb_isnan64(q)) return asdouble(0u, so | 0x7ff00000u); return asdouble(tb_lo64(q), (tb_hi64(q) & 0x7fffffffu) | so); }
double tb_div_scale64(double x, double d, double n, out uint vcc) { vcc = 0u; if (d == 0.0L || n == 0.0L) { vcc = 1u; return asdouble(0u, 0x7ff80000u); } int en = tb_exp64(n), ed = tb_exp64(d); if (en - ed >= 768) { vcc = 1u; return (x == d) ? tb_ldexp64(x, 128) : x; } if (tb_sub64(d)) return tb_ldexp64(x, 128); bool tr = tb_sub64(1.0L / d), tq = tb_sub64(n / d); if (tr && tq) { vcc = 1u; return (x == d) ? tb_ldexp64(x, -128) : x; } if (tr) return tb_ldexp64(x, -128); if (tq) { vcc = 1u; return (x == n) ? tb_ldexp64(x, 128) : x; } if (en <= 53) return tb_ldexp64(x, 128); return x; }
double tb_div_fmas64(double a, double b, double c, uint vcc) { precise double r = fma(a, b, c); if (tb_isnan64(r) && !tb_isnan64(a) && !tb_isnan64(b) && !tb_isnan64(c)) return asdouble(0u, 0x7ff80000u); if ((vcc & 1u) == 0u) return r; double up = asdouble(0u, 0x47f00000u), dn = asdouble(0u, 0x37f00000u); if (tb_exp64(c) > 1023) { if (!tb_sub64(r)) return r * up; bool sa = abs(a) <= abs(b); return fma((sa ? a : b) * up, sa ? b : a, c * up); } precise double q = r * dn; if (r == 0.0L || !(abs(r) < asdouble(0u, 0x08100000u))) return q; precise double df = r - q * up; if (abs(df) != asdouble(0u, 0x04c00000u)) return q; precise double p = a * b; precise double pe = fma(a, b, -p); precise double s1 = p + c; precise double bb = s1 - p; precise double s1e = (p - (s1 - bb)) + (c - bb); precise double e = (s1 - r) + (s1e + pe); if (e == 0.0L || ((e > 0.0L) != (df > 0.0L))) return q; return q + ((df > 0.0L) ? asdouble(1u, 0u) : -asdouble(1u, 0u)); }
double tb_trig_preop64(double x, uint seg) { int e = (int)((tb_hi64(x) >> 20) & 0x7ffu); int sh = (int)(seg & 31u) * 53; if (e > 1077) sh += e - 1077; if (sh >= 1120) return 0.0L; uint w = (uint)sh / 24u, o = (uint)sh % 24u; uint64_t th = (((uint64_t)tb_2opi[w]) << 40) | (((uint64_t)tb_2opi[w + 1u]) << 16) | (uint64_t)(tb_2opi[w + 2u] >> 8); uint64_t tl = (((uint64_t)(tb_2opi[w + 2u] & 0xffu)) << 56) | (((uint64_t)tb_2opi[w + 3u]) << 32); uint64_t m = ((o == 0u) ? th : ((th << o) | (tl >> (64u - o)))) >> 11; int sc = -53 - sh; if (e >= 1968) sc += 128; uint mh = (uint)(m >> 32), ml = (uint)m; if ((mh | ml) == 0u) return 0.0L; int msb = (mh != 0u) ? 32 + (int)firstbithigh(mh) : (int)firstbithigh(ml); int ex = msb + sc; uint64_t b; if (ex >= -1022) { b = (((uint64_t)(uint)(ex + 1023)) << 52) | ((m << (uint)(52 - msb)) & ((((uint64_t)0xfffffu) << 32) | (uint64_t)0xffffffffu)); } else { int s = -1074 - sc; if (s <= 0) { b = m << (uint)(-s); } else if (s >= 64) { b = 0; } else { b = m >> (uint)s; uint64_t rem = m & ((((uint64_t)1) << (uint)s) - 1); uint64_t hf = ((uint64_t)1) << (uint)(s - 1); if (rem > hf || (rem == hf && (b & 1) != 0)) b += 1; } } return asdouble((uint)b, (uint)(b >> 32)); }
bool tb_cmp_class64(uint lo, uint hi, uint mask) { uint e = (hi >> 20) & 0x7ffu; bool mz = ((hi & 0xfffffu) | lo) == 0u; bool neg = (hi >> 31) != 0u; uint c; if (e == 0x7ffu) { c = !mz ? (((hi & 0x80000u) != 0u) ? 1u : 0u) : (neg ? 2u : 9u); } else if (e == 0u) { c = mz ? (neg ? 5u : 6u) : (neg ? 4u : 7u); } else { c = neg ? 3u : 8u; } return ((mask >> c) & 1u) != 0u; }
"#;

fn hi64(o: &Opnd) -> Opnd {
    match o {
        Opnd::Vgpr(n) => Opnd::Vgpr(n + 1),
        other => hi(other),
    }
}

fn half16(o: &Opnd, hi: bool) -> Result<String, String> {
    if !hi {
        return Ok(format!("(({}) & 0xffffu)", r(o)?));
    }
    if matches!(o, Opnd::ConstInt(_) | Opnd::ConstF32(_) | Opnd::Literal(_)) {
        return Err(format!("op_sel high half of constant {o:?} unsupported"));
    }
    Ok(format!("(({}) >> 16)", r(o)?))
}

fn d64(o: &Opnd) -> Result<String, String> {
    Ok(match o {
        Opnd::ConstF32(x) => {
            if (*x - 0.159_154_94).abs() < 1e-7 {
                "0.15915494309189532L".to_string()
            } else {
                format!("({:?}L)", *x as f64)
            }
        }
        Opnd::ConstInt(k) => format!("asdouble({:#x}u, {:#x}u)", *k as u32, if *k < 0 { u32::MAX } else { 0 }),
        Opnd::Literal(k) => format!("asdouble(0u, {k:#x}u)"),
        _ => format!("asdouble({}, {})", r(o)?, r(&hi64(o))?),
    })
}

fn wave_scan_op(red: &str) -> Option<(&'static str, u8)> {
    Some(match red {
        "v_add_f32" => ("WavePrefixSum", 1),
        "v_mul_f32" => ("WavePrefixProduct", 1),
        "v_add_nc_u32" => ("WavePrefixSum", 0),
        _ => return None,
    })
}

/// Whole-wave reductions (HLSL `WaveActiveMin` / `WaveActiveMax`; observed in
/// CocFlattenMainCS, 4f9670de0be37288). The compiled form is:
///
/// ```text
///   s_nand_saveexec_b64 sX, 0        ; save exec, enable every lane
///   v_mov_b32     vI, <identity>
///   v_cndmask_b32 vR, vI, vIn, sX    ; inactive lanes contribute the identity
///   v_min_f32     vR, <dpp>, vR      ; DPP row/quad permutes (src0 = 233 / 250)
///   ...  vop3<0x378>, v_min_f32, v_readlane_b32 (lane 32), v_min_f32 ...
///   v_readlane_b32 sRes, vR, 0       ; the result, a scalar
///   s_mov_b64     exec_lo, sX        ; restore exec
/// ```
///
/// A per-lane translation cannot express the cross-lane data movement, but the net effect
/// is exactly a wave reduction, so the region is replaced by: `sX = exec` (exec itself is
/// left alone), the identity move, and one synthetic instruction
/// `sRes = WaveActive{Min,Max}(lane active ? vIn : identity)`. Needs shader model 6 (DXC).
///
/// Inferred, not documented: operand codes 233/250 as DPP with a trailing control dword and
/// SOP1 0x29 as nand-saveexec are taken from the public GFX10 encoding and are consistent
/// with this shader's instruction stream; the meaning of VOP3 0x378 is not needed.
/// Anything in the region that does not fit this shape is refused.
fn lower_wave_reductions(insns: &mut [Insn]) -> Result<(), String> {
    let is_dpp = |i: &Insn| i.enc == Enc::Vop2 && matches!(i.src.first(), Some(Opnd::Raw(233 | 234 | 250)));
    let mut at = 0;
    'red: while let Some(first) = (at..insns.len()).find(|&i| is_dpp(&insns[i])) {
        let red = insns[first].mnemonic.clone();
        if wave_reduce_op(&red).is_none() {
            at = first + 1;
            continue 'red;
        }
        let Some(Opnd::Vgpr(vr)) = insns[first].dst.first().copied() else { at = first + 1; continue 'red; };
        // Prologue: nand_saveexec, identity move, cndmask.
        if first < 2 {
            at = first + 1;
            continue 'red;
        }
        let cm = first - 1;
        let c = &insns[cm];
        let (sel_ident, vin, msk) = match (c.mnemonic.as_str(), c.dst.first(), c.src.first(), c.src.get(1), c.src.get(2)) {
            ("v_cndmask_b32", Some(Opnd::Vgpr(d)), Some(i), Some(Opnd::Vgpr(x)), Some(Opnd::Sgpr(m))) if *d == vr => (*i, *x, *m),
            _ => { at = first + 1; continue 'red; },
        };
        let (a, ident) = match sel_ident {
            Opnd::ConstInt(k) => (cm - 1, k as u32),
            Opnd::ConstF32(f) => (cm - 1, f.to_bits()),
            Opnd::Literal(k) => (cm - 1, k),
            Opnd::Vgpr(ident_reg) => {
                if cm < 2 {
                    at = first + 1;
            continue 'red;
                }
                let mv = cm - 1;
                match (insns[mv].mnemonic.as_str(), insns[mv].dst.first(), insns[mv].src.first()) {
                    ("v_mov_b32", Some(Opnd::Vgpr(v)), Some(Opnd::Literal(k))) if *v == ident_reg => (mv - 1, *k),
                    _ => { at = first + 1; continue 'red; },
                }
            }
            _ => { at = first + 1; continue 'red; },
        };
        let saved = match (&insns[a].enc, insns[a].op, insns[a].dst.first(), insns[a].src.first()) {
            (Enc::Sop1, 0x29, Some(Opnd::Sgpr(x)), Some(Opnd::ConstInt(0))) => *x,
            (Enc::Sop1, 0x25, Some(Opnd::Sgpr(x)), Some(Opnd::ConstInt(-1))) => *x,
            _ => { at = first + 1; continue 'red; },
        };
        if msk != saved {
            at = first + 1;
            continue 'red;
        }
        // Body up to the exec restore.
        let mut end = None;
        let mut last_read = None;
        for i in first..insns.len() {
            let x = &insns[i];
            if x.mnemonic == "s_mov_b64" && x.dst.first() == Some(&Opnd::ExecLo) && x.src.first() == Some(&Opnd::Sgpr(saved)) {
                end = Some(i);
                break;
            }
            let ok = (x.enc == Enc::Vop2 && x.mnemonic == red && x.dst.first() == Some(&Opnd::Vgpr(vr)))
                || (x.enc == Enc::Vop3 && x.op == 0x378 && x.src.first() == Some(&Opnd::Vgpr(vr)))
                || (x.enc == Enc::Vop3 && x.op == 0x360 && x.src.first() == Some(&Opnd::Vgpr(vr)));
            if !ok {
                at = first + 1;
            continue 'red;
            }
            if x.enc == Enc::Vop3 && x.op == 0x360 {
                last_read = Some(i);
            }
        }
        let (Some(end), Some(last)) = (end, last_read) else { at = first + 1; continue 'red; };
        if insns[last].src.get(1) != Some(&Opnd::ConstInt(0)) {
            at = first + 1;
            continue 'red;
        }
        let res = match insns[last].dst.first() {
            Some(Opnd::Vgpr(n)) | Some(Opnd::Sgpr(n)) => *n,
            _ => { at = first + 1; continue 'red; },
        };
        // Rewrite.
        insns[a].mnemonic = "s_mov_b64".into();
        insns[a].op = 4;
        insns[a].src = vec![Opnd::ExecLo];
        for i in cm..end {
            let x = &mut insns[i];
            x.enc = Enc::Vop1;
            x.op = 0;
            x.mnemonic = "v_nop".into();
            x.dst = vec![Opnd::Vgpr(vr)];
            x.src = vec![];
            x.aux = Default::default();
        }
        let x = &mut insns[last];
        x.enc = Enc::Vop3;
        x.op = OP_WAVE_REDUCE;
        x.mnemonic = format!("tb_wave_{red}");
        x.dst = vec![Opnd::Sgpr(res)];
        x.src = vec![Opnd::Vgpr(vin), Opnd::Sgpr(saved)];
        x.aux.imm = ident as i64;
        at = end + 1;
    }
    Ok(())
}

fn resolve_far_jumps(insns: &mut [Insn]) {
    let literal = |o: Option<&Opnd>| match o {
        Some(Opnd::Literal(v)) => Some(*v as u64),
        Some(Opnd::ConstInt(v)) if *v >= 0 => Some(*v as u64),
        _ => None,
    };
    for i in 0..insns.len().saturating_sub(3) {
        let (Some(Opnd::Sgpr(lo)), true) = (insns[i].dst.first().copied(), insns[i].mnemonic == "s_getpc_b64") else { continue };
        let (add, addc, set) = (&insns[i + 1], &insns[i + 2], &insns[i + 3]);
        let Some(k) = (add.mnemonic == "s_add_u32"
            && add.dst.first() == Some(&Opnd::Sgpr(lo))
            && add.src.first() == Some(&Opnd::Sgpr(lo)))
        .then(|| literal(add.src.get(1)))
        .flatten() else { continue };
        let carries = addc.mnemonic == "s_addc_u32"
            && addc.dst.first() == Some(&Opnd::Sgpr(lo + 1))
            && addc.src.first() == Some(&Opnd::Sgpr(lo + 1))
            && literal(addc.src.get(1)) == Some(0);
        let jumps = set.mnemonic == "s_setpc_b64" && set.src.first() == Some(&Opnd::Sgpr(lo));
        if !carries || !jumps || k % 4 != 0 {
            continue;
        }
        let target = insns[i].offset as i64 + 4 + k as i64;
        let from = insns[i + 3].offset as i64 + 4;
        let branch = &mut insns[i + 3];
        branch.enc = Enc::Sopp;
        branch.op = 2;
        branch.mnemonic = "s_branch".to_string();
        branch.dst.clear();
        branch.src.clear();
        branch.aux.imm = (target - from) / 4;
    }
}

fn lower_wave_scans(insns: &mut [Insn]) -> Result<(), String> {
    let is_dpp = |i: &Insn| i.enc == Enc::Vop2 && matches!(i.src.first(), Some(Opnd::Raw(233 | 234 | 250)));
    let mut first = 0;
    while first < insns.len() {
        if !is_dpp(&insns[first]) || first < 3 {
            first += 1;
            continue;
        }
        let red = insns[first].mnemonic.clone();
        let (Some(Opnd::Vgpr(vs)), Some(_)) = (insns[first].dst.first().copied(), wave_scan_op(&red)) else {
            first += 1;
            continue;
        };
        let mv = first - 1;
        let vr = match (insns[mv].mnemonic.as_str(), insns[mv].dst.first(), insns[mv].src.first()) {
            ("v_mov_b32", Some(Opnd::Vgpr(d)), Some(Opnd::Vgpr(x))) if *d == vs => *x,
            _ => {
                first += 1;
                continue;
            }
        };
        let cm = mv - 1;
        let (ident, msk) = match (insns[cm].mnemonic.as_str(), insns[cm].dst.first(), insns[cm].src.first(), insns[cm].src.get(2)) {
            ("v_cndmask_b32", Some(Opnd::Vgpr(d)), Some(i), Some(Opnd::Sgpr(m))) if *d == vr => (*i, *m),
            _ => {
                first += 1;
                continue;
            }
        };
        let a = match ident {
            Opnd::ConstInt(_) | Opnd::ConstF32(_) | Opnd::Literal(_) => cm.checked_sub(1),
            Opnd::Vgpr(iv) if cm >= 2 && insns[cm - 1].mnemonic == "v_mov_b32" && insns[cm - 1].dst.first() == Some(&Opnd::Vgpr(iv)) => Some(cm - 2),
            _ => None,
        };
        let Some(a) = a else {
            first += 1;
            continue;
        };
        let saved = match (&insns[a].enc, insns[a].op, insns[a].dst.first(), insns[a].src.first()) {
            (Enc::Sop1, 0x29, Some(Opnd::Sgpr(x)), Some(Opnd::ConstInt(0))) | (Enc::Sop1, 0x25, Some(Opnd::Sgpr(x)), Some(Opnd::ConstInt(-1))) if *x == msk => *x,
            _ => {
                first += 1;
                continue;
            }
        };
        let off = insns[first].offset;
        let mut end = None;
        let mut exclusive = false;
        for i in first..insns.len() {
            let x = &insns[i];
            if x.mnemonic == "s_mov_b64" && x.dst.first() == Some(&Opnd::ExecLo) && x.src.first() == Some(&Opnd::Sgpr(saved)) {
                end = Some(i);
                break;
            }
            if exclusive {
                return Err(format!("wave scan at {off:#x}: {} after the exclusive adjustment", x.mnemonic));
            }
            let ok = (matches!(x.enc, Enc::Vop2 | Enc::Vop3) && x.mnemonic == red && x.dst.first() == Some(&Opnd::Vgpr(vs)))
                || (x.enc == Enc::Sop1 && x.mnemonic == "s_mov_b32" && matches!(x.dst.first(), Some(Opnd::ExecLo | Opnd::ExecHi)))
                || (x.enc == Enc::Vop3 && matches!(x.op, 0x360 | 0x377 | 0x378))
                || matches!(x.mnemonic.as_str(), "s_nop" | "s_waitcnt");
            if x.mnemonic == "v_sub_nc_u32" && red == "v_add_nc_u32" && x.dst.first() == Some(&Opnd::Vgpr(vs)) && x.src.first() == Some(&Opnd::Vgpr(vs)) && x.src.get(1) == Some(&Opnd::Vgpr(vr)) {
                exclusive = true;
                continue;
            }
            if !ok {
                return Err(format!("wave scan at {off:#x}: unexpected {} at {:#x}", x.mnemonic, x.offset));
            }
        }
        let Some(end) = end else { return Err(format!("wave scan at {off:#x}: no exec restore")) };
        insns[a].mnemonic = "s_mov_b64".into();
        insns[a].op = 4;
        insns[a].src = vec![Opnd::ExecLo];
        let x = &mut insns[mv];
        x.enc = Enc::Vop3;
        x.op = OP_WAVE_SCAN;
        x.mnemonic = format!("tb_scan_{red}");
        x.dst = vec![Opnd::Vgpr(vs)];
        x.src = vec![Opnd::Vgpr(vr)];
        x.aux = Default::default();
        x.aux.imm2 = (!exclusive) as _;
        for i in first..end {
            let x = &mut insns[i];
            x.enc = Enc::Vop1;
            x.op = 0;
            x.mnemonic = "v_nop".into();
            x.dst = vec![Opnd::Vgpr(vs)];
            x.src = vec![];
            x.aux = Default::default();
        }
        first = end + 1;
    }
    Ok(())
}

fn wave_reduce_op(red: &str) -> Option<(&'static str, u8)> {
    Some(match red {
        "v_min_f32" => ("WaveActiveMin", 1),
        "v_max_f32" => ("WaveActiveMax", 1),
        "v_add_f32" => ("WaveActiveSum", 1),
        "v_mul_f32" => ("WaveActiveProduct", 1),
        "v_add_nc_u32" => ("WaveActiveSum", 0),
        "v_min_u32" => ("WaveActiveMin", 0),
        "v_max_u32" => ("WaveActiveMax", 0),
        "v_min_i32" => ("WaveActiveMin", 2),
        "v_max_i32" => ("WaveActiveMax", 2),
        "v_and_b32" => ("WaveActiveBitAnd", 0),
        "v_or_b32" => ("WaveActiveBitOr", 0),
        "v_xor_b32" => ("WaveActiveBitXor", 0),
        _ => return None,
    })
}

/// Translator temporaries from this index up are lane-spill slots (declared up front).
const LANE_TEMP_BASE: u16 = 10_000;

fn readlane_dst(o: &Opnd) -> Option<Opnd> {
    match *o {
        Opnd::Vgpr(n) | Opnd::Sgpr(n) if n <= 105 => Some(Opnd::Sgpr(n)),
        Opnd::Vgpr(106) | Opnd::VccLo => Some(Opnd::VccLo),
        Opnd::Vgpr(107) | Opnd::VccHi => Some(Opnd::VccHi),
        Opnd::Vgpr(124) | Opnd::M0 => Some(Opnd::M0),
        Opnd::Vgpr(125) | Opnd::Null => Some(Opnd::Null),
        Opnd::Vgpr(126) | Opnd::ExecLo => Some(Opnd::ExecLo),
        Opnd::Vgpr(127) | Opnd::ExecHi => Some(Opnd::ExecHi),
        _ => None,
    }
}

fn lower_lane_spills(insns: &mut [Insn]) -> Result<(), String> {
    const READLANE: u32 = 0x360;
    const WRITELANE: u32 = 0x361;
    let is_lane = |i: &Insn| i.enc == Enc::Vop3 && (i.op == READLANE || i.op == WRITELANE);
    if !insns.iter().any(is_lane) {
        return Ok(());
    }
    let mut vector_regs: BTreeSet<u8> = BTreeSet::new();
    for ins in insns.iter() {
        if ins.mnemonic == "v_nop" {
            continue;
        }
        let ops: Vec<&Opnd> = if is_lane(ins) { if ins.op == READLANE { Vec::new() } else { ins.src.iter().collect() } } else { ins.dst.iter().chain(ins.src.iter()).collect() };
        for o in ops {
            if let Opnd::Vgpr(v) = o {
                vector_regs.insert(*v);
            }
        }
    }
    let spill_lane = |i: &Insn| match (i.op, i.dst.first(), i.src.first(), i.src.get(1)) {
        (WRITELANE, Some(Opnd::Vgpr(v)), _, Some(Opnd::ConstInt(l))) => !vector_regs.contains(v) && (0..64).contains(l),
        (READLANE, _, Some(Opnd::Vgpr(v)), _) => !vector_regs.contains(v),
        _ => false,
    };
    let written: BTreeSet<u8> = insns.iter().filter(|i| is_lane(i) && i.op == WRITELANE && spill_lane(i)).filter_map(|i| match i.dst.first() { Some(Opnd::Vgpr(v)) => Some(*v), _ => None }).collect();
    let mut slots: BTreeMap<(u8, i64), Opnd> = BTreeMap::new();
    let mut spill_vregs: BTreeSet<u8> = BTreeSet::new();
    for ins in insns.iter_mut() {
        if !is_lane(ins) || !spill_lane(ins) {
            continue;
        }
        if ins.op == READLANE && !matches!(ins.src.first(), Some(Opnd::Vgpr(v)) if written.contains(v)) {
            continue;
        }
        let at = ins.offset;
        let lane = match ins.src.get(1) {
            Some(Opnd::ConstInt(l)) if (0..64).contains(l) => *l,
            other => return Err(format!("lane op at {at:#x}: non-constant lane operand {other:?}")),
        };
        if ins.op == WRITELANE {
            let Some(Opnd::Vgpr(v)) = ins.dst.first().cloned() else {
                return Err(format!("v_writelane at {at:#x}: destination is not a VGPR"));
            };
            let next = slots.len();
            let slot = match slots.get(&(v, lane)) {
                Some(s) => *s,
                None => {
                    let s = Opnd::Temp(LANE_TEMP_BASE + next as u16);
                    slots.insert((v, lane), s);
                    s
                }
            };
            spill_vregs.insert(v);
            let value = ins.src.first().cloned().ok_or_else(|| format!("v_writelane at {at:#x}: no source"))?;
            ins.dst = vec![slot];
            ins.src = vec![value];
        } else {
            // The destination field of v_readlane names an SGPR (decoded generically as a VGPR).
            let Some(d) = ins.dst.first().and_then(readlane_dst) else {
                return Err(format!("v_readlane at {at:#x}: unexpected destination {:?}", ins.dst.first()));
            };
            let Some(Opnd::Vgpr(v)) = ins.src.first().cloned() else {
                return Err(format!("v_readlane at {at:#x}: source is not a VGPR"));
            };
            let Some(&slot) = slots.get(&(v, lane)) else {
                return Err(format!("v_readlane at {at:#x}: v{v} lane {lane} read before any v_writelane"));
            };
            if d == Opnd::Null {
                ins.enc = Enc::Vop1;
                ins.op = 0x00;
                ins.mnemonic = "v_nop".into();
                ins.dst.clear();
                ins.src.clear();
                continue;
            }
            ins.dst = vec![d];
            ins.src = vec![slot];
        }
        ins.enc = Enc::Sop1;
        ins.op = 0x03;
        ins.mnemonic = "s_mov_b32".into();
    }
    for ins in insns.iter() {
        if ins.mnemonic == "v_nop" {
            continue; // the two header words at the start of every program decode as v_nop
        }
        let general_read = ins.enc == Enc::Vop3 && ins.op == READLANE;
        for o in ins.dst.iter().skip(general_read as usize).chain(ins.src.iter()) {
            if let Opnd::Vgpr(v) = o {
                if spill_vregs.contains(v) {
                    return Err(format!("v{v} is used both as a lane spill area and as a vector at {:#x}", ins.offset));
                }
            }
        }
    }
    Ok(())
}

fn gather_lod_fields(w1: u32, w2: u32) -> Result<(f32, f32, f32), String> {
    let (lo, hi) = (w1 & 0xfff, (w1 >> 12) & 0xfff);
    let bias = (((w2 & 0x3fff) << 18) as i32) >> 18;
    let sec = (w2 >> 14) & 0x3f;
    let mip = (w2 >> 26) & 3;
    if sec != 0 {
        return Err(format!("S# LOD_BIAS_SEC {sec:#x} is scaled by the T# perf_mod, unsupported"));
    }
    if !matches!(mip, 1 | 2) {
        return Err(format!("S# MIP_FILTER {mip} unsupported"));
    }
    if lo & 0xff != 0 || (hi != 0xfff && hi & 0xff != 0) {
        return Err(format!("fractional S# MIN_LOD {lo:#x} / MAX_LOD {hi:#x} unsupported: the PC sampler cannot be steered onto the footprint mip"));
    }
    if lo > hi {
        return Err(format!("S# MIN_LOD {lo:#x} above MAX_LOD {hi:#x} unsupported"));
    }
    Ok((bias as f32 / 256.0, lo as f32 / 256.0, hi as f32 / 256.0))
}

fn dpp_vgpr(ins: &Insn) -> Option<u8> {
    match (ins.enc, ins.src.first(), ins.raw.get(1)) {
        (Enc::Vop1 | Enc::Vop2 | Enc::Vopc, Some(Opnd::Raw(233 | 234 | 250)), Some(w)) => Some((w & 0xff) as u8),
        _ => None,
    }
}

fn dst16(m: &str) -> bool {
    if m.starts_with("v_cmp") || m.starts_with("v_pk_") || m.starts_with("v_fma_mix") || m.starts_with("v_cvt_pk") || m.starts_with("v_interp") {
        return false;
    }
    if matches!(m, "v_cvt_f32_f16" | "v_pack_b32_f16" | "v_sad_u16" | "v_mad_u32_u16" | "v_mad_i32_i16" | "v_readlane_b32" | "v_writelane_b32") {
        return false;
    }
    m == "v_cvt_f16_f32" || [ "_f16", "_u16", "_i16", "_b16"].iter().any(|k| m.ends_with(k))
}

pub fn translate(ex: &ShaderExtract) -> Result<Translated, String> {
    translate_linked(ex, None)
}

pub fn translate_linked(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>) -> Result<Translated, String> {
    if ex.root_sig.is_none() && !ex.has_root_sig && std::env::var_os("TOMBOX_FSSM_ROOT_SIG").is_some_and(|v| v != "0") && matches!(crate::fssm_bind::parse(&ex.fssm), Ok(Some(_))) {
        return translate_linked_with_fssm_root(ex, feeds_ps);
    }
    if ex.root_sig.is_none() && std::env::var_os("TOMBOX_INFER_ROOT_SIG").is_some() {
        if let Ok(sig) = infer::infer_root_signature(ex) {
            let mut inferred = ex.clone();
            inferred.root_sig = Some(sig);
            return translate_linked(&inferred, feeds_ps);
        }
    }
    translate_linked_with(ex, feeds_ps, false, None)
}

pub fn translate_with_pgm_rsrc1(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>, rsrc1: u32) -> Result<Translated, String> {
    translate_linked_with(ex, feeds_ps, false, Some(rsrc1))
}

pub fn translate_user_data(ex: &ShaderExtract) -> Result<Translated, String> {
    translate_linked_with(ex, None, true, None)
}

pub fn translate_linked_user_data(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>) -> Result<Translated, String> {
    translate_linked_with(ex, feeds_ps, true, None)
}

pub fn translate_with_inference(ex: &ShaderExtract) -> Result<Translated, String> {
    if ex.root_sig.is_some() || ex.has_root_sig {
        return translate(ex);
    }
    let mut inferred = ex.clone();
    if let Ok(sig) = infer::infer_root_signature(ex) {
        inferred.root_sig = Some(sig);
    }
    translate(&inferred)
}

fn fssm_buffer_strides(ex: &ShaderExtract) -> BTreeMap<(bool, u32), u32> {
    let mut out: BTreeMap<(bool, u32), u32> = BTreeMap::new();
    let Ok(Some(b)) = crate::fssm_bind::parse(&ex.fssm) else { return out };
    let mut conflict: BTreeSet<(bool, u32)> = BTreeSet::new();
    for u in &b.uses {
        let is_uav = match u.class {
            crate::fssm_bind::Class::Srv => false,
            crate::fssm_bind::Class::Uav => true,
            _ => continue,
        };
        if u.stride < 4 || u.stride % 4 != 0 || u.register > 0xffff {
            continue;
        }
        let key = (is_uav, u.register);
        if *out.entry(key).or_insert(u.stride) != u.stride {
            conflict.insert(key);
        }
    }
    for k in conflict {
        out.remove(&k);
    }
    out
}

pub fn translate_with_fssm_root(ex: &ShaderExtract) -> Result<Translated, String> {
    translate_linked_with_fssm_root(ex, None)
}

pub fn translate_linked_with_fssm_root(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>) -> Result<Translated, String> {
    if ex.root_sig.is_some() || ex.has_root_sig {
        return translate_linked_with(ex, feeds_ps, false, None);
    }
    let b = crate::fssm_bind::parse(&ex.fssm)?.ok_or("no root signature and no FSSM binding table")?;
    let mut with = ex.clone();
    with.root_sig = Some(b.root_signature());
    match b.user_base(ex.stage)? {
        Some(base) => translate_with(&with, &out_interp_for(ex, feeds_ps), Some(base), BTreeSet::new(), false, mode::container_pgm_rsrc1(ex)),
        None => translate_linked_with(&with, feeds_ps, false, None),
    }
}

fn out_interp_for(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>) -> InputInterp {
    match ex.stage {
        Stage::Vertex | Stage::Geometry | Stage::Mesh | Stage::Domain => feeds_ps.cloned().unwrap_or_default(),
        _ => InputInterp::default(),
    }
}

fn fssm_user_base(ex: &ShaderExtract) -> Option<usize> {
    let rs = ex.root_sig.as_ref()?;
    let b = crate::fssm_bind::parse(&ex.fssm).ok().flatten()?;
    if !b.disagreements(rs).is_empty() {
        return None;
    }
    b.user_base(ex.stage).ok().flatten()
}

fn translate_linked_with(ex: &ShaderExtract, feeds_ps: Option<&InputInterp>, user_data: bool, rsrc1: Option<u32>) -> Result<Translated, String> {
    let rsrc1 = rsrc1.or_else(|| mode::container_pgm_rsrc1(ex));
    let out_interp = out_interp_for(ex, feeds_ps);
    let base = fssm_user_base(ex);
    match translate_with(ex, &out_interp, base, BTreeSet::new(), user_data, rsrc1) {
        Err(e) if base.is_none() && disassemble(&ex.isa).iter().take(16).any(|i| i.mnemonic == "s_swappc_b64") => translate_with(ex, &out_interp, Some(8), BTreeSet::new(), user_data, rsrc1).map_err(|e2| if e2.contains("not a buffer descriptor") || e2.contains("not a root table") { e } else { e2 }),
        r => r,
    }
}

fn translate_with(ex: &ShaderExtract, out_interp: &InputInterp, user_base_override: Option<usize>, uav_force: BTreeSet<u32>, user_data: bool, rsrc1: Option<u32>) -> Result<Translated, String> {
    let (profile, is_ps) = match ex.stage {
        Stage::Vertex => ("vs_5_0", false),
        Stage::Pixel => ("ps_5_0", true),
        Stage::Compute => ("cs_5_0", false),
        Stage::Geometry => ("gs_5_0", false),
        Stage::Mesh => ("ms_6_5", false),
        Stage::Amplification => ("as_6_5", false),
        Stage::Domain => ("ds_5_0", false),
        other => return Err(format!("stage {other:?} not supported yet")),
    };
    let mut insns = disassemble(&ex.isa);
    resolve_far_jumps(&mut insns);
    let wave64 = mesh::wave_size(&insns) == 64;
    lower_wave_scans(&mut insns)?;
    lower_wave_reductions(&mut insns)?;
    lower_lane_spills(&mut insns)?;
    for i in insns.iter_mut() {
        if i.enc == Enc::Sopp && matches!(i.op, 0x1b | 0x1e) {
            i.op = 1;
        }
    }
    let mut em = Emitter {
        loop_depth: 0,
        ex,
        insns,
        res: HashMap::new(),
        cb_size: BTreeMap::new(),
        vs_lat: HashMap::new(),
        cfg: None,
        sampler_literals: None,
        uses_smem_bounds: false,
        cb_lim: HashMap::new(),
        tex: BTreeMap::new(),
        fmask: BTreeMap::new(),
        fmask_loc: BTreeSet::new(),
        gather_ssamp: HashMap::new(),
        gather_blind: BTreeSet::new(),
        samp: BTreeSet::new(),
        samp_cmp: BTreeSet::new(),
        uav: BTreeMap::new(),
        sbuf: BTreeMap::new(),
        fssm_strides: fssm_buffer_strides(ex),
        tbuf: BTreeSet::new(),
        tbuf_uint: BTreeSet::new(),
        tbuf_ty: BTreeMap::new(),
        img_ty: BTreeMap::new(),
        epilog_pack: HashMap::new(),
        epilog_exp: HashMap::new(),
        cur_idx: 0,
        sbuf_flat: BTreeSet::new(),
        is_cs: matches!(ex.stage, Stage::Compute | Stage::Mesh | Stage::Amplification),
        is_gs: ex.stage == Stage::Geometry,
        uses_lds: false,
        lds_w_pending: false,
        lds_r_pending: false,
        lds_nested: false,
        lds64_nested: false,
        lds64_n: None,
        brace_depth: 0,
        uses_code: false,
        vreg_array: false,
        d16: None,
        multi_sets: Vec::new(),
        uav_uint: BTreeSet::new(),
        uav_u64: BTreeSet::new(),
        tbuf_u64: BTreeSet::new(),
        sbuf_u64: BTreeSet::new(),
        uses_wave: false,
        wave64,
        msel: HashMap::new(),
        bsel: HashMap::new(),
        dsel_loads: HashMap::new(),
        dsel_moves: HashMap::new(),
        live: Vec::new(),
        uses_quad: false,
        vs_lds_out: false,
        regs: BTreeSet::new(),
        body: String::new(),
        tmp: 0,
        // PS attribute slot k = the k-th input *register* that carries an interpolant and is
        // read. A register counts as read if any element packed into it is read — including
        // a system value sharing it (observed: PARTICLE_RANDOM unread + SV_IsFrontFace read
        // in the same register still occupies an attribute slot; d26fd88ff8712f9e).
        ps_attrs: distinct_regs(ex.input_sig.iter().filter(|e| {
            e.sysval == 0 && ex.input_sig.iter().any(|o| o.register == e.register && o.rw_mask != 0)
        })),
        out_params: distinct_regs(ex.output_sig.iter().filter(|e| e.sysval == 0)),
        prim_params: distinct_regs(ex.prim_sig.iter().filter(|e| e.sysval == 0)),
        user_base_override,
        durango: durango::layout(ex)?,
        rm_mode: std::cell::Cell::new(None),
        rm_loads: BTreeMap::new(),
        rm_used: BTreeSet::new(),
        bl: Vec::new(),
        bl_loads: HashMap::new(),
        bl_const: HashMap::new(),
        uses_f64: false,
        uses_d16: false,
        uses_trans16: false,
        uses_trans32: false,
        uav_mips: BTreeSet::new(),
        coherent_read: BTreeSet::new(),
        uav_written: BTreeSet::new(),
        tex_mips: BTreeSet::new(),
        sm6_minor: 0,
        hw_regs: BTreeSet::new(),
        uav_force,
        bl_uav: BTreeSet::new(),
        bl_srv: BTreeSet::new(),
        uses_bvh: false,
        uses_global: false,
        uses_scratch: false,
        mode_use: mode::ModeUse::default(),
        init_mode: rsrc1.map_or(mode::MODE_DEFAULT, mode::mode_from_pgm_rsrc1),
        out_interp: out_interp.clone(),
        user_data,
        unknowns: BTreeSet::new(),
    };
    em.vreg_array = em.insns.iter().any(|i| i.mnemonic.starts_with("v_movrel") || i.mnemonic == "v_swaprel_b32");
    em.mode_use = mode::analyze(&em.insns, em.init_mode as u8);
    if rsrc1.is_none() {
        if let Some(i) = em.insns.iter().find(|i| mode::f16_omod(i)) {
            em.unknowns.insert(format!(
                "f16 output modifier at {:#x}: it acts only with MODE IEEE 0 and f16 denormals flushed, the initial MODE (PGM_RSRC1 FLOAT_MODE, DX10_CLAMP, IEEE_MODE) is not located in this container, MODE {:#x} is assumed",
                i.offset,
                mode::MODE_DEFAULT
            ));
        }
    }
    em.plan_lds64();
    if em.mode_use.sp || em.mode_use.dp {
        em.hw_regs.insert(1);
    }
    // Lane-spill slots beyond the tracked range are function-scope temporaries.
    for ins in &em.insns {
        for o in ins.dst.iter().chain(ins.src.iter()) {
            if let Opnd::Temp(n) = o {
                if *n >= LANE_TEMP_BASE {
                    em.regs.insert(format!("tb_s{n}"));
                }
            }
        }
    }
    if em.is_ds() {
        em.ds_check()?;
    }
    em.resolve_resources()?;
    let clash: BTreeSet<u32> = em.bl_uav.intersection(&em.bl_srv).copied().filter(|s| !em.uav_force.contains(s)).collect();
    if !clash.is_empty() {
        let mut force = em.uav_force.clone();
        force.extend(clash);
        return translate_with(ex, out_interp, user_base_override, force, user_data, rsrc1);
    }
    if is_ps {
        em.find_export_epilogs();
    }
    em.infer_tbuf_types();
    let live_in = em.live_in();
    em.live = live_in.clone();
    em.emit_body(is_ps)?;
    let mut hlsl = em.finish(is_ps)?;
    hlsl.insert_str(0, &format!("// entry-live registers (read before written): {}
", live_in.join(" ")));
    // ps_5_0 forces UAV slots to start after the render-target outputs (FXC X4509);
    // SM5.1 has no such rule, and D3D12 accepts SM5.1 bytecode.
    let profile = if is_ps && !em.uav.is_empty() { "ps_5_1" } else { profile };
    let profile = if hlsl.contains(", space") {
        match profile {
            "vs_5_0" => "vs_5_1",
            "ps_5_0" => "ps_5_1",
            "cs_5_0" => "cs_5_1",
            "gs_5_0" => "gs_5_1",
            p => p,
        }
    } else {
        profile
    };
    let profile = match (em.sm6_minor, profile.split('_').next()) {
        (0, _) => profile,
        (2, Some("ps")) => "ps_6_2",
        (6, Some("ps")) => "ps_6_6",
        (6, Some("vs")) => "vs_6_6",
        (6, Some("cs")) => "cs_6_6",
        (6, Some("gs")) => "gs_6_6",
        (7, Some("ps")) => "ps_6_7",
        (7, Some("vs")) => "vs_6_7",
        (7, Some("cs")) => "cs_6_7",
        (7, Some("gs")) => "gs_6_7",
        (_, Some("ps")) => "ps_6_8",
        (_, Some("vs")) => "vs_6_8",
        (_, Some("cs")) => "cs_6_8",
        (_, Some("gs")) => "gs_6_8",
        (1..=5, Some("ms")) => "ms_6_5",
        (6, Some("ms")) => "ms_6_6",
        (7, Some("ms")) => "ms_6_7",
        (_, Some("ms")) => "ms_6_8",
        _ => profile,
    };
    Ok(Translated { hlsl, entry: "main", profile, mip_uavs: em.uav_mips.iter().copied().collect(), mip_srvs: em.tex_mips.iter().copied().collect(), needs_gmem: em.uses_bvh, needs_global: em.uses_global, uses_typed_atomic64: !em.uav_u64.is_empty() || !em.tbuf_u64.is_empty() || !em.sbuf_u64.is_empty(), user_data_base: em.ud_active().then(|| em.user_base()), cb_limits: em.cb_limits(), fmask: em.fmask_loc.iter().copied().collect(), gather_lod_blind: em.gather_blind.iter().copied().collect(), unknowns: em.unknowns.iter().cloned().collect() })
}

impl<'a> Emitter<'a> {
    /// Root-signature range type (0 SRV, 1 UAV, 2 CBV, 3 Sampler) of the descriptor table
    /// whose pointer lives in user SGPR `sgpr` at shader entry. The FSSM register table
    /// maps user SGPR (base + i) to root parameter i, with base 8 for graphics stages and
    /// 4 for compute (observed: PS s8/s9/s10 = params 0/1/2, VS s12 = param 4, CS s5/s7 =
    /// params 1/3). `None` when the root signature is unavailable or the SGPR is not a table.
    fn table_kind(&self, sgpr: u8) -> Option<u32> {
        if let Some(k) = self.rm_table_kind(sgpr) {
            return k;
        }
        // Root descriptor-table pointers arrive in consecutive user SGPRs: from s4 in compute
        // shaders and s8 in the other stages. Vertex shaders compiled to feed a geometry
        // shader ("MainForGS") start with `s_swappc_b64 s12, s12` and receive them from s19
        // 7 registers after the swapped PC register pair instead (observed: `s_swappc_b64
        // s12, s12` -> SRV table s19, CBV s20 in a55400ccab602283; `s_swappc_b64 s4, s4` ->
        // SRV s11, CBV s12 in 4d318a496eebf6d3).
        // Geometry shaders: SRV table in s22, CBV table in s23 (6a6bdf123db8b5b0).
        let (p, k) = self.root_param_at(sgpr, self.user_base())?;
        if k != 0 {
            return None;
        }
        match &self.ex.root_sig.as_ref()?.params.get(p)?.kind {
            crate::meta::RootParamKind::Table(r) if !r.is_empty() && r.iter().all(|x| x.range_type == r[0].range_type) => Some(r[0].range_type),
            crate::meta::RootParamKind::Descriptor { kind, .. } => Some(match kind {
                2 => 2,
                3 => 0,
                _ => 1,
            }),
            _ => None,
        }
    }

    fn table_kind_for(&self, sgpr: u8, writes: bool) -> Option<u32> {
        if let Some(k) = self.table_kind(sgpr) {
            return Some(k);
        }
        let (p, 0) = self.root_param_at(sgpr, self.user_base())? else { return None };
        match &self.ex.root_sig.as_ref()?.params.get(p)?.kind {
            crate::meta::RootParamKind::Table(r) if !r.is_empty() && r.iter().all(|x| x.range_type <= 1) => Some(writes as u32),
            _ => None,
        }
    }

    fn merged_es(&self) -> bool {
        self.live.iter().any(|l| l == "s3")
            && self.insns.iter().take(16).any(|i| matches!(i.mnemonic.as_str(), "s_bfe_u32" | "s_bfe_i32") && i.src.first() == Some(&Opnd::Sgpr(3)))
    }

    fn user_base(&self) -> usize {
        if let Some(b) = self.user_base_override {
            return b;
        }
        let chained = self.insns.iter().take(16).find(|i| i.mnemonic == "s_swappc_b64").and_then(|i| match i.dst.first() {
            Some(Opnd::Sgpr(n)) if *n >= 8 => Some(*n as usize + 7),
            _ => None,
        });
        if self.ex.stage == Stage::Mesh {
            16
        } else if self.is_cs {
            4
        } else if self.ex.stage == Stage::Geometry {
            22
        } else {
            chained.unwrap_or(8)
        }
    }

    fn root_param_at(&self, sgpr: u8, base: usize) -> Option<(usize, u32)> {
        if let Some(r) = self.dg_slot(sgpr) {
            return r;
        }
        let mut at = (sgpr as usize).checked_sub(base)? as u32;
        for (i, p) in self.ex.root_sig.as_ref()?.params.iter().enumerate() {
            let n = match &p.kind {
                crate::meta::RootParamKind::Table(_) => 1,
                crate::meta::RootParamKind::Descriptor { .. } => 2,
                crate::meta::RootParamKind::Constants { num32, .. } => *num32,
            };
            if at < n {
                return Some((i, at));
            }
            at -= n;
        }
        None
    }

    fn static_sgpr_init(&mut self) -> String {
        let mut init = String::new();
        if self.dg().is_some() || self.rootmem() {
            return init;
        }
        let Some(rs) = self.ex.root_sig.as_ref() else { return init };
        let base = self.user_base();
        let mut sgpr = base as u8;
        while let Some((p, k)) = self.root_param_at(sgpr, base) {
            if let Some(param) = rs.params.get(p) {
                if let crate::meta::RootParamKind::Constants { .. } = &param.kind {
                    let r = format!("s{sgpr}");
                    if self.live.contains(&r) {
                        self.rm_used.insert(p);
                        self.regs.insert(r.clone());
                        let _ = writeln!(init, "    {r} = tb_rc{p}_{k};");
                    }
                }
            }
            let Some(next) = sgpr.checked_add(1) else { break };
            sgpr = next;
        }
        init
    }

    fn is_root_desc(&self, sgpr: u8) -> bool {
        let Some((p, 0)) = self.root_param_at(sgpr, self.user_base()) else { return false };
        matches!(self.ex.root_sig.as_ref().and_then(|r| r.params.get(p)).map(|x| &x.kind), Some(crate::meta::RootParamKind::Descriptor { .. }))
    }

    fn bind_id(&self, table: u8, off: u32, stride: u32) -> u32 {
        if let Some(id) = self.rm_bind(table) {
            return id;
        }
        if off & BINDLESS != 0 {
            return off;
        }
        let fallback = off / stride;
        let Some((p, 0)) = self.root_param_at(table, self.user_base()) else { return fallback };
        let Some(param) = self.ex.root_sig.as_ref().and_then(|r| r.params.get(p)) else { return fallback };
        match &param.kind {
            crate::meta::RootParamKind::Descriptor { register, space, .. } => (space << 16) | register,
            crate::meta::RootParamKind::Table(ranges) => {
                let idx = off / stride;
                let mut next = 0u32;
                for r in ranges {
                    let start = if r.offset_in_table == u32::MAX { next } else { r.offset_in_table };
                    next = start.saturating_add(r.num);
                    if idx >= start && (r.num == u32::MAX || idx - start < r.num) {
                        return (r.space << 16) | (r.base_register + idx - start);
                    }
                }
                fallback
            }
            _ => fallback,
        }
    }

    fn sgpr_const_in(insns: &[Insn], start: usize, idx: usize, reg: u8) -> Option<u32> {
        for j in (start..idx).rev() {
            let i = &insns[j];
            let wide = match i.enc {
                Enc::Smem if matches!(i.op, 0..=4 | 8..=12) => 1u16 << (i.op & 7),
                Enc::Smem if matches!(i.op, 0x24 | 0x25) => 2,
                _ if ["_b64", "_u64", "_i64"].iter().any(|suffix| i.mnemonic.ends_with(suffix)) && !i32_result_of_64(&i.mnemonic) => 2,
                _ => 1,
            };
            let hit = i.dst.iter().any(|d| matches!(d, Opnd::Sgpr(r) if (reg as u16) >= *r as u16 && (reg as u16) < *r as u16 + wide));
            if !hit {
                continue;
            }
            return match (i.mnemonic.as_str(), i.src.first()) {
                ("s_movk_i32", _) => Some(i.aux.imm as i32 as u32),
                ("s_mov_b32", Some(Opnd::ConstInt(v))) => Some(*v as u32),
                ("s_mov_b32", Some(Opnd::Literal(v))) => Some(*v),
                _ => None,
            };
        }
        None
    }

    fn gcn_image_dim(&self, table: u8, off: u32, is_uav: bool, dim: u32) -> u32 {
        if !crate::gcn::is_durango(&self.ex.fssm) || !matches!(dim, 1 | 5) {
            return dim;
        }
        let byte = if off & BINDLESS != 0 {
            let n = off & !BINDLESS;
            match (self.bl_const.get(&n), self.bl.get(n as usize)) {
                (Some(c), Some((_, imm))) => c.wrapping_add(*imm),
                _ => return dim,
            }
        } else {
            off
        };
        if self.root_param_at(table, self.user_base()).is_none() {
            return dim;
        }
        let id = self.bind_id(table, byte, 32);
        if id & BINDLESS != 0 {
            return dim;
        }
        let (space, reg) = (id >> 16, id & 0xffff);
        let want = if is_uav { 6 } else { 3 };
        let Some(p) = self.ex.resources.iter().find(|p| p.res_type == want && p.space == space && p.lower <= reg && reg <= p.upper) else { return dim };
        psv_image_dim_ms(p.kind, dim, msaa_dim_on())
    }

    fn decl(&self, used: &mut Vec<(String, u8, usize)>, ty: &str, name: &str, slot: u32, class: char, range_type: u32, stride: u32) -> Result<String, String> {
        if slot & BINDLESS == 0 {
            return Ok(format!("{ty} {name} : register({});\n", Self::reg_of(class, slot)));
        }
        let n = slot & !BINDLESS;
        let (table, _) = *self.bl.get(n as usize).ok_or("bindless: unknown load")?;
        let (p, _) = self.root_param_at(table, self.user_base()).ok_or_else(|| format!("bindless: s{table} is not a root parameter"))?;
        let Some(crate::meta::RootParamKind::Table(ranges)) = self.ex.root_sig.as_ref().and_then(|r| r.params.get(p)).map(|x| &x.kind) else {
            return Err(format!("bindless: s{table} is not a descriptor table"));
        };
        let mut starts = Vec::with_capacity(ranges.len());
        let mut next = 0u32;
        for r in ranges {
            let s = if r.offset_in_table == u32::MAX { next } else { r.offset_in_table };
            next = s.saturating_add(r.num);
            starts.push(s);
        }
        let mut out = String::new();
        let key = match used.iter().find(|u| u.0 == ty && u.1 == table) {
            Some(u) => u.2,
            None => {
                let of_type: Vec<usize> = (0..ranges.len()).filter(|&i| ranges[i].range_type == range_type).collect();
                let unb: Vec<usize> = of_type.iter().copied().filter(|&i| ranges[i].num == u32::MAX).collect();
                let cands = match (unb.len(), of_type.len()) {
                    (0, 1) => of_type,
                    (0, n) => return Err(format!("bindless: table s{table} has {n} bounded ranges of type {range_type} and no unbounded one for {ty}")),
                    _ => unb,
                };
                let bounded = ranges[cands[0]].num != u32::MAX;
                let key = match cands.iter().copied().find(|&i| !bounded && !used.iter().any(|u| u.1 == table && u.2 & 0xffff == i)) {
                    Some(ri) => ri,
                    None => {
                        let ri = cands[0];
                        let k = used.iter().filter(|u| u.1 == table && u.2 & 0xffff == ri).count() + usize::from(bounded);
                        if k > BL_ALIASES as usize {
                            return Err(format!("bindless: more than {} HLSL types share unbounded range {ri} of table s{table}", BL_ALIASES + 1));
                        }
                        ri | (k << 16)
                    }
                };
                used.push((ty.to_string(), table, key));
                let (ri, k) = (key & 0xffff, (key >> 16) as u32);
                let space = if k == 0 { ranges[ri].space } else { bl_alias_space(k, ranges[ri].space) };
                let len = if ranges[ri].num == u32::MAX { String::new() } else { ranges[ri].num.to_string() };
                out.push_str(&format!("{ty} {}[{len}] : register({class}{}, space{space});\n", bl_name(table, key), ranges[ri].base_register));
                key
            }
        };
        out.push_str(&format!("#define {name} {}[tb_bi{n} / {stride}u - {}u]\n", bl_name(table, key), starts[key & 0xffff]));
        Ok(out)
    }

    fn reg_of(kind: char, id: u32) -> String {
        if id >> 16 == 0 {
            format!("{kind}{id}")
        } else {
            format!("{kind}{}, space{}", id & 0xffff, id >> 16)
        }
    }

    /// Classify descriptor loads and bind them to HLSL registers.
    ///
    /// Code is not laid out in execution order (blocks are reached through backward
    /// branches), so the symbolic SGPR state is computed as a forward dataflow fixpoint over
    /// the control-flow graph: state = (known symbolic values, set of written SGPRs), join =
    /// keep only values every predecessor agrees on / union of written sets. A final pass
    /// then walks each block with its entry state and records the resources.
    fn resolve_resources(&mut self) -> Result<(), String> {
        type State = (HashMap<u16, Sym>, BTreeSet<u16>);
        let n = self.insns.len();
        if n == 0 {
            return Ok(());
        }
        let off2idx: HashMap<usize, usize> = self.insns.iter().enumerate().map(|(i, x)| (x.offset, i)).collect();
        // Block leaders and per-instruction branch targets.
        let mut leaders: BTreeSet<usize> = BTreeSet::new();
        leaders.insert(0);
        let mut tgt: HashMap<usize, usize> = HashMap::new();
        for (i, ins) in self.insns.iter().enumerate() {
            if ins.enc != Enc::Sopp {
                continue;
            }
            if Self::branch_cond(ins.op).is_some() {
                let t = ins.offset as i64 + 4 + ins.aux.imm * 4;
                if let Some(&ti) = off2idx.get(&(t as usize)) {
                    tgt.insert(i, ti);
                    leaders.insert(ti);
                }
                leaders.insert(i + 1);
            } else if ins.op == 1 {
                leaders.insert(i + 1);
            }
        }
        let starts: Vec<usize> = leaders.into_iter().filter(|&s| s < n).collect();
        let block_of = |i: usize| starts.partition_point(|&s| s <= i) - 1;
        let ranges: Vec<std::ops::Range<usize>> =
            (0..starts.len()).map(|b| starts[b]..starts.get(b + 1).copied().unwrap_or(n)).collect();
        let succs: Vec<Vec<usize>> = ranges
            .iter()
            .map(|r| {
                let last = r.end - 1;
                let ins = &self.insns[last];
                let mut v = Vec::new();
                let is_sopp = ins.enc == Enc::Sopp;
                if let Some(&t) = tgt.get(&last) {
                    v.push(block_of(t));
                }
                let falls = !(is_sopp && (ins.op == 1 || ins.op == 2));
                if falls && r.end < n {
                    v.push(block_of(r.end));
                }
                v
            })
            .collect();
        self.cfg = Some((ranges.clone(), succs.clone()));
        self.sampler_literals = None;

        // Fixpoint (analysis only: recordings are rolled back afterwards).
        let saved = (
            self.res.clone(),
            self.cb_size.clone(),
            self.tex.clone(),
            self.samp.clone(),
            self.samp_cmp.clone(),
            self.uav.clone(),
            self.sbuf.clone(),
            self.tbuf.clone(),
            self.tbuf_uint.clone(),
            self.tbuf_u64.clone(),
            self.sbuf_u64.clone(),
        );
        let mut first_err: Option<String> = None;
        let mut states: Vec<Option<State>> = vec![None; ranges.len()];
        states[0] = Some((HashMap::new(), BTreeSet::new()));
        let mut work = vec![0usize];
        let mut budget = 20_000usize;
        while let Some(b) = work.pop() {
            if budget == 0 {
                return Err("descriptor dataflow did not converge".into());
            }
            budget -= 1;
            let (mut sym, mut written) = states[b].clone().unwrap();
            if let Err(e) = self.res_block(&mut sym, &mut written, ranges[b].clone()) {
                // Unknown effect: forget everything and treat every SGPR as written. Keep
                // the first failure — it is the root cause if the recording pass fails too.
                first_err.get_or_insert(e);
                sym.clear();
                written = (0u16..=105).collect();
            }
            for &s in &succs[b] {
                let changed = match &mut states[s] {
                    slot @ None => {
                        *slot = Some((sym.clone(), written.clone()));
                        true
                    }
                    Some((osym, owr)) => {
                        let before = (osym.clone(), owr.len());
                        let keys: Vec<u16> = osym.keys().copied().collect();
                        for k in keys {
                            let a = osym[&k];
                            match sym.get(&k).copied() {
                                Some(b) if a == b => {}
                                Some(b) => match self.join_desc(a, b) {
                                    Some(j) => {
                                        osym.insert(k, j);
                                    }
                                    None => {
                                        osym.remove(&k);
                                    }
                                },
                                // Known on one path only. A descriptor stays (see below);
                                // anything else is dropped.
                                None if matches!(a, Sym::Desc { .. } | Sym::Multi { .. } | Sym::Table(_)) => {}
                                None => {
                                    osym.remove(&k);
                                }
                            }
                        }
                        // Optimistic join for descriptors: a register that holds a descriptor
                        // on one incoming path and nothing known on the other is treated as
                        // holding the descriptor. Compiled code loads descriptors inside
                        // blocks that are skipped only when no lane is active
                        // (`s_cbranch_execz`), then uses them after the join; on the skipped
                        // path the result is unused. A later use as a descriptor would be
                        // invalid anyway if the register held something else.
                        for (k, v) in sym.iter() {
                            if matches!(v, Sym::Desc { .. } | Sym::Multi { .. } | Sym::Table(_)) && !osym.contains_key(k) {
                                osym.insert(*k, *v);
                            }
                        }
                        owr.extend(written.iter().copied());
                        before != (osym.clone(), owr.len())
                    }
                };
                if changed && !work.contains(&s) {
                    work.push(s);
                }
            }
        }
        (self.res, self.cb_size, self.tex, self.samp, self.samp_cmp, self.uav, self.sbuf, self.tbuf, self.tbuf_uint, self.tbuf_u64, self.sbuf_u64) = saved;

        // TB_DFDEBUG=<sgpr>: print that register's symbol at every block entry (diagnostics).
        if let Some(reg) = std::env::var("TB_DFDEBUG").ok().and_then(|v| v.parse::<u16>().ok()) {
            for (b, r) in ranges.iter().enumerate() {
                let s = states[b].as_ref().map(|(sym, wr)| (sym.get(&reg).copied(), wr.contains(&reg)));
                eprintln!("block {b} @{:#x}..: s{reg} = {s:?} succs={:?}", self.insns[r.start].offset, succs[b]);
            }
            eprintln!("first analysis error: {first_err:?}");
        }
        self.vs_lat = smem_bounds::lattice(&self.insns, &ranges, &succs);
        self.msel.clear();
        self.bsel.clear();
        self.dsel_loads.clear();
        self.dsel_moves.clear();
        // Recording pass with the converged entry states.
        for (b, r) in ranges.iter().enumerate() {
            if let Some((mut sym, mut written)) = states[b].clone() {
                self.res_block(&mut sym, &mut written, r.clone()).map_err(|e| first_err.clone().unwrap_or(e))?;
            }
        }
        Ok(())
    }

    /// Element type of each typed buffer, inferred from how the shader uses the values it
    /// loads from (or produces for) the buffer. The buffer view's format is not part of the
    /// shader: the hardware delivers floats for float/normalised formats and integers for
    /// integer formats, and the compiled code simply goes on to treat the registers as one
    /// or the other. HLSL needs the element type declared, so it is recovered from the
    /// first typed uses: conversions from / comparisons as / arithmetic on integers or
    /// floats, and use as a memory index. Bitwise logic and moves are followed, not counted.
    /// Slots with no evidence, or more float than integer evidence, stay float.
    fn infer_tbuf_types(&mut self) {
        // 0 float, 1 uint, 2 sint
        fn class(mn: &str) -> Option<u8> {
            let m = mn.strip_suffix("_e64").unwrap_or(mn);
            if let Some(rest) = m.strip_prefix("v_cvt_") {
                let last = rest.rsplit('_').next().unwrap_or("");
                return Some(if last.starts_with('f') { 0 } else if last.starts_with('i') { 2 } else { 1 });
            }
            if m.starts_with("v_cmp") {
                return if m.contains("_f") { Some(0) } else if m.contains("_i") { Some(2) } else if m.contains("_u") { Some(1) } else { None };
            }
            const SINT: [&str; 7] = ["v_ashr", "v_max_i", "v_min_i", "v_bfe_i", "v_mul_i32", "v_mad_i32", "v_mul_hi_i"];
            const UINT: [&str; 14] = [
                "v_lshl", "v_lshr", "v_bfe_u", "v_mul_lo", "v_mul_hi_u", "v_mul_u32", "v_mad_u32", "v_add_nc", "v_sub_nc", "v_subrev_nc", "v_add_co", "v_sub_co",
                "v_max_u", "v_min_u",
            ];
            if SINT.iter().any(|p| m.starts_with(p)) {
                return Some(2);
            }
            if UINT.iter().any(|p| m.starts_with(p)) {
                return Some(1);
            }
            if m.starts_with("v_") && (m.contains("_f32") || m.contains("_f16") || m.contains("_f64")) {
                return Some(0);
            }
            None
        }
        // Bitwise logic with a small constant (`x & 1`, `x & 0xf`, `x ^ 1`) is integer work:
        // the masks applied to float bit patterns are sign / exponent / mantissa masks, all
        // far larger.
        fn small_mask(u: &Insn) -> bool {
            ["v_and_b32", "v_or_b32", "v_xor_b32"].iter().any(|p| u.mnemonic.starts_with(p))
                && u.src.iter().any(|o| match o {
                    Opnd::ConstInt(c) => *c > 0 && *c <= 64,
                    Opnd::Literal(l) => *l > 0 && *l <= 0xffff,
                    _ => false,
                })
        }
        fn follows(mn: &str) -> bool {
            ["v_mov_b32", "v_and_b32", "v_or_b32", "v_xor_b32", "v_cndmask_b32", "v_not_b32"].iter().any(|p| mn.starts_with(p))
        }
        fn half_move(u: &Insn) -> bool {
            let m = u.mnemonic.strip_suffix("_e64").unwrap_or(&u.mnemonic);
            match m {
                "v_mov_b32" | "v_cndmask_b32" | "v_or3_b32" | "v_and_or_b32" | "v_perm_b32"
                | "v_alignbit_b32" | "v_alignbyte_b32" | "v_bfi_b32" => true,
                "v_lshl_or_b32" => matches!(u.src.get(1), Some(Opnd::ConstInt(16))),
                "v_or_b32" => u.src.iter().all(|o| matches!(o, Opnd::Vgpr(_))),
                "v_and_b32" => u.src.iter().any(|o| matches!(o, Opnd::Literal(0xffff | 0xffff_0000))),
                "v_lshrrev_b32" | "v_lshlrev_b32" => matches!(u.src.first(), Some(Opnd::ConstInt(16))),
                _ => false,
            }
        }
        fn result_class(mn: &str, d16: bool) -> Option<u8> {
            match mn.strip_prefix("v_cvt_") {
                Some(rest) if d16 && (rest.starts_with("pkrtz_") || rest.starts_with("pknorm_")) => Some(0),
                Some(rest) => Some(if rest.starts_with('f') { 0 } else if rest.contains("i32") || rest.contains("i16") { 2 } else { 1 }),
                None => class(mn),
            }
        }
        // Keys are (is image, is_uav, slot): typed buffers and textures are told apart.
        let mut votes: BTreeMap<(bool, bool, u32), [u32; 3]> = BTreeMap::new();
        // (stored-to resource, loaded-from resource): a store of registers loaded from another
        // typed resource says both hold the same kind of element.
        let mut links: Vec<((bool, bool, u32), (bool, bool, u32))> = Vec::new();
        // Textures that are sampled or gathered hold filterable (float) data.
        let mut sampled: BTreeSet<u32> = BTreeSet::new();
        let n = self.insns.len();
        // The typed resource an instruction reads or writes, with the data registers.
        let typed = |em: &Self, i: usize| -> Option<((bool, bool, u32), BTreeSet<u8>, bool, bool)> {
            let ins = &em.insns[i];
            let Some(&Opnd::Vgpr(vd)) = ins.dst.first() else { return None };
            match em.res.get(&i).and_then(|v| v.first()).copied()? {
                Res::TBuf(slot, is_uav) => {
                    let comps = match ins.mnemonic.rsplit('_').next() {
                        Some("x") => 1u8,
                        Some("xy") => 2,
                        Some("xyz") => 3,
                        Some("xyzw") => 4,
                        _ => return None,
                    };
                    let regs = (0..comps).map(|k| vd.wrapping_add(k)).collect();
                    Some(((false, is_uav, slot), regs, ins.mnemonic.starts_with("buffer_load"), ins.mnemonic.starts_with("buffer_store")))
                }
                r @ (Res::Tex(..) | Res::Uav(..)) => {
                    let (slot, is_uav) = match r {
                        Res::Tex(s, _) => (s, false),
                        Res::Uav(s, _) => (s, true),
                        _ => unreachable!(),
                    };
                    let comps = mimg_data_regs(ins) as u8;
                    let regs = (0..comps).map(|k| vd.wrapping_add(k)).collect();
                    let load = matches!(ins.mnemonic.as_str(), "image_load" | "image_load_mip");
                    Some(((true, is_uav, slot), regs, load, ins.mnemonic.starts_with("image_store")))
                }
                _ => None,
            }
        };
        for i in 0..n {
            if let Some(Res::Tex(slot, _)) = self.res.get(&i).and_then(|v| v.first()).copied() {
                let m = &self.insns[i].mnemonic;
                if m.starts_with("image_sample") || m.starts_with("image_gather") {
                    sampled.insert(slot);
                }
            }
            let Some((key, regs, is_load, is_store)) = typed(self, i) else { continue };
            let v = votes.entry(key).or_insert([0; 3]);
            let d16 = mimg_d16::mimg_d16(&self.insns[i]) || self.insns[i].mnemonic.contains("_d16_");
            if is_load {
                // Forward: how are the loaded registers (and plain copies / masks of them) used?
                let mut live = regs;
                for j in i + 1..n {
                    if live.is_empty() {
                        break;
                    }
                    let u = &self.insns[j];
                    // The scan is linear: past an unconditional branch (or the program end)
                    // the following code belongs to another path.
                    if u.enc == Enc::Sopp && (u.op == 1 || u.op == 2) {
                        break;
                    }
                    let uses = u.src.iter().any(|o| matches!(o, Opnd::Vgpr(r) if live.contains(r)));
                    let mem_index = matches!(u.enc, Enc::Mubuf | Enc::Mimg | Enc::Ds) && matches!(u.src.first(), Some(Opnd::Vgpr(r)) if live.contains(r));
                    if mem_index && u.enc == Enc::Mimg && (u.mnemonic.starts_with("image_sample") || u.mnemonic.starts_with("image_gather")) {
                        // Sampling coordinates are floats; only texel fetches index by integer.
                        v[0] += 1;
                    } else if mem_index && !(u.mnemonic.contains("store") && matches!(u.enc, Enc::Mubuf)) {
                        v[1] += 1;
                    } else if uses && d16 && half_move(u) {
                    } else if uses && !self.epilog_pack.contains_key(&j) {
                        // (The export tail packs whatever type the output has: no evidence.)
                        if let Some(c) = class(&u.mnemonic) {
                            v[c as usize] += 1;
                        } else if small_mask(u) {
                            v[1] += 1;
                        }
                    }
                    let carry = uses && (follows(&u.mnemonic) || (d16 && half_move(u)));
                    for d in &u.dst {
                        if let Opnd::Vgpr(r) = d {
                            if carry {
                                live.insert(*r);
                            } else if !matches!(u.enc, Enc::Mubuf | Enc::Ds | Enc::Exp) || u.mnemonic.contains("load") || u.mnemonic.contains("read") {
                                live.remove(r);
                            }
                        }
                    }
                }
            } else if is_store {
                let mut work: Vec<(u8, usize, u32)> = regs.into_iter().map(|r| (r, i, 0)).collect();
                while let Some((r, before, depth)) = work.pop() {
                    for j in (0..before).rev() {
                        let d = &self.insns[j];
                        if d.dst.iter().any(|o| matches!(o, Opnd::Vgpr(x) if *x == r)) {
                            if let Some((k2, _, true, _)) = typed(self, j) {
                                links.push((key, k2));
                                break;
                            }
                            if d16 && half_move(d) {
                                if depth < 4 {
                                    for o in &d.src {
                                        if let Opnd::Vgpr(x) = o {
                                            work.push((*x, j, depth + 1));
                                        }
                                    }
                                }
                                break;
                            }
                            if let Some(c) = result_class(&d.mnemonic, d16) {
                                v[c as usize] += 1;
                            }
                            break;
                        }
                    }
                }
            }
        }
        let mut ty: BTreeMap<(bool, bool, u32), u8> = BTreeMap::new();
        for (k, v) in &votes {
            let ints = v[1] + v[2];
            if ints > v[0] {
                ty.insert(*k, if v[2] > 0 { 2 } else { 1 });
            }
        }
        for (to, from) in links {
            if votes.get(&to).map(|v| v.iter().all(|&x| x == 0)).unwrap_or(true) {
                if let Some(&t) = ty.get(&from) {
                    ty.insert(to, t);
                }
            }
        }
        for ((is_img, is_uav, slot), t) in ty {
            if !is_img {
                self.tbuf_ty.insert((is_uav, slot), t);
            } else if is_uav {
                // Textures used with image atomics already have their own uint declaration.
                if !self.uav_uint.contains(&slot) && !self.uav_u64.contains(&slot) {
                    self.img_ty.insert((true, slot), t);
                }
            } else if !sampled.contains(&slot) {
                self.img_ty.insert((false, slot), t);
            }
        }
    }

    /// Recognise the colour-export epilogue of a pixel shader.
    ///
    /// Every colour export in the title's pixel shaders has the same shape, whatever the
    /// HLSL output type:
    ///
    /// ```text
    /// s_nop ; s_nop ; v_cvt_pkrtz_f16_f32 vA, x, y ; v_cvt_pkrtz_f16_f32 vB, z, w ; exp mrtN vA, vB (compressed)
    /// ```
    ///
    /// including shaders whose output is `uint4` (the lighting-channel copy writes an R16_UINT
    /// target this way, where a float-to-half conversion of the integer would always give 0).
    /// The export format belongs to the render target, which the shader compiler does not
    /// know, so this tail can only be a default that the console's pipeline creation replaces
    /// per target format (the two leading no-ops are spare room for it). That replacement is
    /// inferred, not observed. What the shader itself computes is the four unpacked operands
    /// (x, y, z, w), so those are exported directly: as integers for an integer output
    /// element, as full floats otherwise (the render-target write then converts to the
    /// target's own format, as a per-format epilogue would).
    ///
    /// Records the pack instructions whose operands must be captured and, per export, which
    /// captures feed it. An export is left alone unless both packed operands come from pack
    /// instructions directly before it with nothing but no-ops in between and no branch
    /// landing inside the sequence.
    fn find_export_epilogs(&mut self) {
        let mut off2idx: HashMap<usize, usize> = HashMap::new();
        for (i, ins) in self.insns.iter().enumerate() {
            off2idx.insert(ins.offset, i);
        }
        let mut targets: BTreeSet<usize> = BTreeSet::new();
        for ins in &self.insns {
            if ins.enc == Enc::Sopp && Self::branch_cond(ins.op).is_some() {
                let t = ins.offset as i64 + 4 + ins.aux.imm * 4;
                if t >= 0 {
                    if let Some(&ti) = off2idx.get(&(t as usize)) {
                        targets.insert(ti);
                    }
                }
            }
        }
        let mut next_id = 0usize;
        for idx in 0..self.insns.len() {
            let ins = &self.insns[idx];
            if ins.enc != Enc::Exp || ins.aux.imm2 & 1 == 0 || ins.aux.imm > 7 {
                continue;
            }
            let (Some(&Opnd::Vgpr(r0)), Some(&Opnd::Vgpr(r1))) = (ins.src.first(), ins.src.get(1)) else { continue };
            let en = ins.aux.mask;
            let (need0, need1) = (en & 0x3 != 0, en & 0xc != 0);
            let (mut p0, mut p1) = (None, None);
            let mut first = idx;
            for j in (idx.saturating_sub(8)..idx).rev() {
                let u = &self.insns[j];
                if matches!(u.mnemonic.as_str(), "s_nop" | "s_waitcnt") {
                    continue;
                }
                if u.mnemonic != "v_cvt_pkrtz_f16_f32" {
                    break;
                }
                let Some(&Opnd::Vgpr(d)) = u.dst.first() else { break };
                if d == r0 && p0.is_none() {
                    p0 = Some(j);
                    first = j;
                }
                if d == r1 && p1.is_none() {
                    p1 = Some(j);
                    first = j;
                }
            }
            if (need0 && p0.is_none()) || (need1 && p1.is_none()) {
                continue;
            }
            if targets.range(first + 1..=idx).next().is_some() {
                continue;
            }
            let mut id_of = |em: &mut Self, p: Option<usize>, need: bool| -> usize {
                match p {
                    Some(j) if need => *em.epilog_pack.entry(j).or_insert_with(|| {
                        next_id += 1;
                        next_id - 1
                    }),
                    _ => usize::MAX,
                }
            };
            let a = id_of(self, p0, need0);
            let b = id_of(self, p1, need1);
            self.epilog_exp.insert(idx, (a, b));
        }
    }

    /// Declared element size, in dwords, of a structured buffer.
    ///
    /// The stride is a property of the buffer view, not of the shader: on the console the
    /// hardware multiplies the element index by the descriptor's stride. D3D12 instead takes
    /// the stride from the HLSL element type (observed: a translation declaring a 12-byte
    /// element for the 16-byte primitive-data buffer read the wrong rows), so the declared
    /// element has to be the real one. It is not recoverable in general. What the PC build
    /// of the same game shows for 801 buffers bound at the same register: stride 16 for
    /// every buffer the Xbox code touches with extents of 8..28 bytes, 64 for extents of 64.
    /// Hence: extents of 8..28 bytes -> 16-byte elements (larger offsets address following
    /// elements), anything larger rounded up to a multiple of 16. A buffer only ever read or
    /// written one dword at offset 0 keeps a 4-byte element: ScatterCopyCS reads its index
    /// buffer that way and its view really has stride 4 (forcing 16 scrambled the per-object
    /// data it uploads); no 16-byte buffer in the comparison was touched like that.
    /// A heuristic (TECHNICAL_DEBT).
    fn sbuf_words(&self, key: (bool, u32)) -> u32 {
        if let Some(stride) = self.fssm_strides.get(&key) {
            return stride / 4;
        }
        let raw = self.sbuf.get(&key).copied().unwrap_or(1).max(1);
        match raw {
            1 => 1,
            2..=7 => 4,
            _ => raw.div_ceil(4) * 4,
        }
    }

    /// Resource a MUBUF instruction addresses when its descriptor is `slot` of `table`
    /// (also records declaration requirements).
    fn mubuf_res(&mut self, ins: &Insn, slot: u32, table: u8, off: u32) -> Result<Res, String> {
        if ins.enc == Enc::Mtbuf {
            return self.mtbuf_res(ins, slot, table, off);
        }
        let writes = ins.mnemonic.contains("store") || ins.mnemonic.contains("atomic") || self.uav_force.contains(&slot);
        let kind = self.kind_at(table, off, writes)?;
        if slot & BINDLESS != 0 && kind.is_some_and(|k| k <= 1) {
            if kind == Some(1) { self.bl_uav.insert(slot); } else { self.bl_srv.insert(slot); }
        }
        let is_uav = match kind {
            Some(0) => false,
            Some(1) => true,
            Some(2) => {
                // Per-lane indexed read of a constant buffer (e.g. cbuffer arrays indexed by
                // a varying value): size the declaration to the maximum.
                self.cb_size.insert(slot, 4096 * 4);
                return Ok(Res::CbDyn(slot));
            }
            None if self.ex.root_sig.is_none() => {
                return Err(format!(
                    "mubuf at {:#x}: missing root signature for descriptor table in s{table}",
                    ins.offset
                ));
            }
            other => return Err(format!("mubuf at {:#x}: buffer in table of kind {other:?}", ins.offset)),
        };
        if ins.mnemonic.contains("_atomic_") {
            if !is_uav {
                return Err(format!("mubuf at {:#x}: atomic on a non-UAV buffer", ins.offset));
            }
            let x2 = ins.mnemonic.ends_with("_x2");
            // PSV0 tells typed (6) from structured (8) UAVs. Atomics need a uint element, so
            // a typed UAV used atomically is declared RWBuffer<uint>.
            let (reg, space) = (slot & 0xffff, slot >> 16);
            let structured = slot & BINDLESS != 0 || self.ex.resources.iter().any(|p| matches!(p.res_type, 7 | 8 | 9) && p.space == space && p.lower <= reg && reg <= p.upper);
            if structured {
                if x2 {
                    self.sbuf_u64.insert(slot);
                } else if self.sbuf_u64.contains(&slot) {
                    return Err(format!("mubuf at {:#x}: structured buffer slot {slot} mixes 32-bit and 64-bit atomics", ins.offset));
                }
                let e = self.sbuf.entry((true, slot)).or_insert(0);
                *e = (*e).max(ins.aux.imm as u32 / 4 + if x2 { 2 } else { 1 });
                return Ok(Res::Buf(slot, true));
            }
            self.tbuf.insert((true, slot));
            if x2 {
                if self.tbuf_uint.contains(&slot) {
                    return Err(format!("mubuf at {:#x}: typed buffer slot {slot} mixes 32-bit and 64-bit atomics", ins.offset));
                }
                self.tbuf_u64.insert(slot);
            } else {
                if self.tbuf_u64.contains(&slot) {
                    return Err(format!("mubuf at {:#x}: typed buffer slot {slot} mixes 32-bit and 64-bit atomics", ins.offset));
                }
                self.tbuf_uint.insert(slot);
            }
            return Ok(Res::TBuf(slot, true));
        }
        if ins.mnemonic.contains("_format") {
            // Typed buffer (Buffer<T> / RWBuffer<T>), addressed by element index.
            self.tbuf.insert((is_uav, slot));
            return Ok(Res::TBuf(slot, is_uav));
        }
        let subword = matches!(
            ins.mnemonic.as_str(),
            "buffer_load_ubyte" | "buffer_load_sbyte" | "buffer_load_ushort" | "buffer_load_sshort" | "buffer_store_byte" | "buffer_store_short"
                | "buffer_load_ubyte_d16" | "buffer_load_ubyte_d16_hi" | "buffer_load_sbyte_d16" | "buffer_load_sbyte_d16_hi"
                | "buffer_load_short_d16" | "buffer_load_short_d16_hi" | "buffer_store_byte_d16_hi" | "buffer_store_short_d16_hi"
        );
        let ndw = match Self::mubuf_dwords(&ins.mnemonic) {
            Some(n) => n,
            None if subword => 1,
            None => return Err(format!("mubuf op {} unsupported", ins.mnemonic)),
        };
        let end = ins.aux.imm as u32 / 4 + ndw;
        let e = self.sbuf.entry((is_uav, slot)).or_insert(0);
        *e = (*e).max(end);
        Ok(Res::Buf(slot, is_uav))
    }

    /// Candidate descriptors `(table, offset)` of a symbol that is dword 0 of a descriptor.
    fn desc_candidates(&self, s: Option<&Sym>, align: u32) -> Option<Vec<(u8, u32)>> {
        if let Some(c) = self.rm_desc(s) {
            return c;
        }
        match s {
            Some(Sym::Desc { table, off, k: 0 }) => Some(vec![(*table, *off)]),
            Some(Sym::Desc { table, off, k }) if *off & BINDLESS == 0 && (4 * *k) % align == 0 => Some(vec![(*table, *off + 4 * *k)]),
            Some(Sym::Multi { set, k: 0 }) => self.multi_sets.get(*set as usize).cloned(),
            Some(Sym::Table(t)) if self.is_root_desc(*t) => Some(vec![(*t, 0)]),
            _ => None,
        }
    }

    /// Join of two different descriptor symbols at a control-flow merge: the union of
    /// their candidates when both are the same dword of some descriptor, else nothing.
    fn join_desc(&mut self, a: Sym, b: Sym) -> Option<Sym> {
        let parts = |s: Sym, sets: &Vec<Vec<(u8, u32)>>| -> Option<(Vec<(u8, u32)>, u32)> {
            match s {
                Sym::Desc { table, off, k } => Some((vec![(table, off)], k)),
                Sym::Multi { set, k } => sets.get(set as usize).map(|v| (v.clone(), k)),
                _ => None,
            }
        };
        let (mut ca, ka) = parts(a, &self.multi_sets)?;
        let (cb, kb) = parts(b, &self.multi_sets)?;
        if ka != kb {
            return None;
        }
        ca.extend(cb);
        ca.sort();
        ca.dedup();
        if ca.len() > 16 {
            return None; // not a small "one of N" selection
        }
        let set = match self.multi_sets.iter().position(|v| *v == ca) {
            Some(i) => i,
            None => {
                self.multi_sets.push(ca);
                self.multi_sets.len() - 1
            }
        };
        Some(Sym::Multi { set: set as u16, k: ka })
    }

    /// Transfer function for one basic block (also records resources / reports errors).
    fn res_block(
        &mut self,
        sym: &mut HashMap<u16, Sym>,
        written: &mut BTreeSet<u16>,
        range: std::ops::Range<usize>,
    ) -> Result<(), String> {
        let insns = self.insns.clone();
        let block_start = range.start;
        let dbg_reg = std::env::var("TB_DFDEBUG").ok().and_then(|v| v.parse::<u16>().ok());
        let mut dbg_prev: Option<Sym> = dbg_reg.and_then(|r| sym.get(&r).copied());
        for idx in range {
            if let Some(r) = dbg_reg {
                let cur = sym.get(&r).copied();
                if cur != dbg_prev && idx > 0 {
                    eprintln!("  s{r}: {dbg_prev:?} -> {cur:?} by {:#x} {}", insns[idx - 1].offset, insns[idx - 1].mnemonic);
                }
                dbg_prev = cur;
            }
            let ins = &insns[idx];
            // Dataflow key of a scalar location: SGPR number, or 1000+ for a lane-spill slot.
            let sg = |o: &Opnd| match o {
                Opnd::Sgpr(n) => Some(*n as u16),
                Opnd::Temp(n) if *n >= LANE_TEMP_BASE => Some(1000 + (*n - LANE_TEMP_BASE)),
                _ => None,
            };
            match ins.enc {
                Enc::Sop1 if ins.mnemonic == "s_mov_b32" => {
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        match ins.src.first().and_then(sg).and_then(|s| sym.get(&s).cloned()) {
                            Some(v) => {
                                sym.insert(d, v);
                            }
                            None => {
                                // A never-written user SGPR copied before use is still a table pointer.
                                let s = ins.src.first().and_then(sg);
                                match s {
                                    Some(s) if !written.contains(&s) => {
                                        sym.insert(d, Sym::Table(s as u8));
                                    }
                                    _ => {
                                        sym.remove(&d);
                                    }
                                }
                            }
                        }
                        written.insert(d);
                        if let Some(s) = ins.src.first().and_then(sg) {
                            if matches!(sym.get(&d), Some(Sym::Desc { k: 0, .. }) | Some(Sym::Multi { k: 0, .. })) {
                                self.dsel_moves.insert(idx, (d, s));
                            }
                        }
                    }
                }
                Enc::Sop1 if ins.mnemonic == "s_mov_b64" => {
                    // 64-bit moves copy descriptors two dwords at a time
                    // (`s_mov_b64 s44, s84` + `s_mov_b64 s46, s86` moves a 4-dword descriptor).
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        let s = ins.src.first().and_then(sg);
                        for k in 0..2u16 {
                            match s.and_then(|s| sym.get(&(s + k)).cloned()) {
                                Some(v) => {
                                    if k == 0 && matches!(v, Sym::Desc { k: 0, .. } | Sym::Multi { k: 0, .. }) {
                                        self.dsel_moves.insert(idx, (d, s.unwrap_or(d)));
                                    }
                                    sym.insert(d + k, v);
                                }
                                None => match s {
                                    // A never-written user SGPR is a root table pointer (see
                                    // s_mov_b32); a 64-bit copy carries it in the low half.
                                    Some(s) if k == 0 && !written.contains(&s) => {
                                        sym.insert(d, Sym::Table(s as u8));
                                    }
                                    _ => {
                                        sym.remove(&(d + k));
                                    }
                                },
                            }
                            written.insert(d + k);
                        }
                    }
                }
                // s_setreg_* program a hardware mode register; the encoding's register
                // field is not an SGPR destination.
                Enc::Sopk if ins.mnemonic.starts_with("s_setreg") => {}
                // A code pointer taken through a VGPR: `v = index*4 + sCode` followed by
                // `v_readfirstlane sBase, v` and `s_load [sBase]` is a constant-table lookup
                // with a run-time index (ColorDeficiencyMain, cc45bda725a0f774). VGPRs are
                // tracked under keys 2000+ for exactly this.
                Enc::Vop2 | Enc::Vop3
                    if matches!(ins.mnemonic.as_str(), "v_add_co_u32" | "v_add_nc_u32")
                        && ins.src.iter().any(|o| sg(o).map(|s| sym.get(&s) == Some(&Sym::Code)).unwrap_or(false)) =>
                {
                    if let Some(Opnd::Vgpr(v)) = ins.dst.first() {
                        sym.insert(2000 + *v as u16, Sym::Code);
                    }
                }
                Enc::Vop1 if ins.mnemonic == "v_readfirstlane_b32" => {
                    let code = matches!(ins.src.first(), Some(Opnd::Vgpr(v)) if sym.get(&(2000 + *v as u16)) == Some(&Sym::Code));
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        if code {
                            sym.insert(d, Sym::Code);
                        } else {
                            sym.remove(&d);
                        }
                        written.insert(d);
                    }
                }
                Enc::Sop1 if ins.mnemonic == "s_swappc_b64" && (self.durango.is_some() || crate::gcn::is_durango(&self.ex.fssm)) => {}
                Enc::Sop1 if ins.mnemonic == "s_getpc_b64" => {
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        sym.insert(d, Sym::Code);
                        sym.remove(&(d + 1));
                        written.insert(d);
                        written.insert(d + 1);
                    }
                }
                Enc::Sop2 if matches!(ins.mnemonic.as_str(), "s_add_u32" | "s_add_i32") => {
                    // Pointer arithmetic keeps a code pointer a code pointer.
                    let is_code = ins.src.iter().any(|o| sg(o).map(|s| sym.get(&s) == Some(&Sym::Code)).unwrap_or(false));
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        if is_code {
                            sym.insert(d, Sym::Code);
                        } else {
                            sym.remove(&d);
                        }
                        written.insert(d);
                    }
                }
                Enc::Smem if ins.op <= 4 && ins.src.first().and_then(sg).map(|b| sym.get(&b) == Some(&Sym::Code)).unwrap_or(false) => {
                    // Read from an embedded constant table.
                    let n = 1u32 << ins.op;
                    let d = ins.dst.first().and_then(sg).ok_or("s_load: bad dst")?;
                    if (ins.raw.get(1).copied().unwrap_or(0) >> 25) != 0x7d {
                        return Err(format!("s_load at {:#x}: code-table read with dynamic soffset unsupported", ins.offset));
                    }
                    self.uses_code = true;
                    self.res.insert(idx, vec![Res::CodeTbl]);
                    for k in 0..n {
                        sym.remove(&(d + k as u16));
                        written.insert(d + k as u16);
                    }
                }
                Enc::Smem if ins.op <= 4 => {
                    let n = 1u32 << ins.op;
                    let b = ins.src.first().and_then(sg).ok_or("s_load: bad base")?;
                    let d = ins.dst.first().and_then(sg).ok_or("s_load: bad dst")?;
                    let table = match sym.get(&b) {
                        Some(Sym::Table(t)) => *t,
                        None if !written.contains(&b) => b as u8,
                        Some(Sym::Desc { table, off, k }) if self.dg_table_ptr(*table, *off, *k).is_some() => self.dg_table_ptr(*table, *off, *k).unwrap_or(0),
                        other => {
                            return Err(format!(
                                "s_load at {:#x}: base s{b} is not a root table pointer ({other:?})",
                                ins.offset
                            ))
                        }
                    };
                    self.rm_note_load(idx, ins, table)?;
                    let soff = ins.raw.get(1).copied().unwrap_or(0) >> 25;
                    if soff != 0x7d {
                        if soff > 107 {
                            return Err(format!("s_load at {:#x}: soffset {soff} unsupported", ins.offset));
                        }
                        let next = self.bl.len() as u32;
                        let bn = self.bl_loads.entry(idx).or_insert((next, soff as u8)).0;
                        if bn == next {
                            self.bl.push((table, ins.aux.imm as u32));
                        }
                        if let Some(c) = Self::sgpr_const_in(&insns, block_start, idx, soff as u8) {
                            self.bl_const.insert(bn, c);
                        }
                        for k in 0..n {
                            sym.insert(d + k as u16, Sym::Desc { table, off: BINDLESS | bn, k });
                            written.insert(d + k as u16);
                        }
                        continue;
                    }
                    for k in 0..n {
                        sym.insert(d + k as u16, Sym::Desc { table, off: ins.aux.imm as u32, k });
                        self.dsel_loads.insert(idx, (d, ((table as u32) << 24) | ins.aux.imm as u32));
                        written.insert(d + k as u16);
                    }
                }
                Enc::Smem if (0x10..=0x12).contains(&ins.op) => {
                    return Err(format!(
                        "{} at {:#x}: stores through a 64-bit scalar address (SBASE), which is not bound to a resource (no GPU-VA-to-resource map)",
                        ins.mnemonic, ins.offset
                    ));
                }
                Enc::Smem if (0x18..=0x1a).contains(&ins.op) => {
                    let n = 1u32 << (ins.op - 0x18);
                    let b = ins.src.first().and_then(sg).ok_or("s_buffer_store: bad base")?;
                    ins.dst.first().and_then(sreg_num).ok_or("s_buffer_store: bad data")?;
                    let bsym = sym.get(&b).copied().or_else(|| (!written.contains(&b)).then_some(Sym::Table(b as u8)));
                    let cands = self.desc_candidates(bsym.as_ref(), 32).ok_or_else(|| {
                        format!("s_buffer_store at {:#x}: base s{b} is not a buffer descriptor ({:?})", ins.offset, sym.get(&b))
                    })?;
                    if cands.len() != 1 {
                        return Err(format!("s_buffer_store at {:#x}: base s{b} may name {} descriptors", ins.offset, cands.len()));
                    }
                    let (table, off) = cands[0];
                    if self.kind_at(table, off, true)? != Some(1) {
                        return Err(format!("s_buffer_store at {:#x}: descriptor at table s{table} offset {off:#x} is not writable", ins.offset));
                    }
                    let slot = self.bind_id(table, off, 32);
                    let end = if ins.aux.imm < 0 { n } else { ins.aux.imm as u32 / 4 + n };
                    let e = self.sbuf.entry((true, slot)).or_insert(0);
                    *e = (*e).max(end);
                    self.res.insert(idx, vec![Res::Buf(slot, true)]);
                }
                Enc::Smem if !(8..=12).contains(&ins.op) => {
                    if let Some(d) = ins.dst.first().and_then(sg) {
                        for k in 0..2u16 {
                            sym.remove(&(d + k));
                            written.insert(d + k);
                        }
                    }
                }
                Enc::Smem => {
                    let n = 1u32 << (ins.op - 8);
                    let b = ins.src.first().and_then(sg).ok_or("s_buffer_load: bad base")?;
                    let d = ins.dst.first().and_then(sreg_num).map(u16::from).ok_or("s_buffer_load: bad dst")?;
                    // CBV descriptors sit on a 32-byte stride (cb1 is loaded from table offset
                    // 0x20 in MainOnePassPointLightPS).
                    let bsym = sym.get(&b).copied().or_else(|| (!written.contains(&b)).then_some(Sym::Table(b as u8)));
                    let cands = self.desc_candidates(bsym.as_ref(), 32).ok_or_else(|| {
                        if let Some(Sym::Table(root)) = bsym {
                            if self.ex.root_sig.is_none() {
                                return format!(
                                    "s_buffer_load at {:#x}: missing root signature for base s{b} (root s{root})",
                                    ins.offset
                                );
                            }
                        }
                        format!("s_buffer_load at {:#x}: base s{b} is not a buffer descriptor ({:?})", ins.offset, sym.get(&b))
                    })?;
                    // A dynamic SOFFSET register indexes the buffer at run time: size the
                    // cbuffer declaration to the D3D12 maximum (64 KiB = 4096 vec4).
                    let dynamic = (ins.raw.get(1).copied().unwrap_or(0) >> 25) != 0x7d;
                    if !dynamic && ins.aux.imm < 0 {
                        return Err(format!("s_buffer_load at {:#x}: negative offset without SOFFSET", ins.offset));
                    }
                    let end = if dynamic { 4096 * 4 } else { ins.aux.imm as u32 / 4 + n };
                    let mut out: Vec<(u32, Res)> = Vec::new();
                    // A scalar load reads whatever buffer the descriptor names. From the CBV
                    // table that is a constant buffer; from the SRV / UAV table it is a
                    // (structured) buffer addressed by byte offset, e.g. the sky-light
                    // spherical-harmonics buffer read by ReflectionEnvironmentSkyLighting.
                    // Only the unambiguous case is handled; with several candidate
                    // descriptors the load is still treated as a constant buffer (debt).
                    let buffer_kind = if cands.len() == 1 { self.kind_at(cands[0].0, cands[0].1, false)?.filter(|k| *k <= 1) } else { None };
                    for (table, off) in &cands {
                        let slot = self.bind_id(*table, *off, 32);
                        if let Some(k) = buffer_kind {
                            let key = (k == 1, slot);
                            let e = self.sbuf.entry(key).or_insert(0);
                            *e = (*e).max(4);
                            self.sbuf_flat.insert(key);
                            out.push((((*table as u32) << 24) | off, Res::Buf(slot, k == 1)));
                            continue;
                        }
                        let e = self.cb_size.entry(slot).or_insert(0);
                        *e = (*e).max(end);
                        out.push((((*table as u32) << 24) | off, Res::Cb(slot)));
                    }
                    self.res.insert(idx, vec![out[0].1]);
                    if out.len() > 1 {
                        self.bsel.insert(idx, (b, out));
                    }
                    for k in 0..n {
                        sym.remove(&(d + k as u16));
                        written.insert(d + k as u16);
                    }
                }
                // buffer_gl0_inv / buffer_gl1_inv (MUBUF 0x71 / 0x72) invalidate the GPU data caches: no
                // resource, no data, and no PC equivalent.
                Enc::Mubuf if ins.op == 0x71 || ins.op == 0x72 => {}
                Enc::Mubuf | Enc::Mtbuf => {
                    let rs = ins.src.get(1).and_then(sg).ok_or("mubuf: bad srsrc")?;
                    let rsym = sym.get(&rs).copied().or_else(|| (!written.contains(&rs)).then_some(Sym::Table(rs as u8)));
                    let cands = self.desc_candidates(rsym.as_ref(), 32).ok_or_else(|| {
                        format!("mubuf at {:#x}: s{rs} not a buffer descriptor ({:?})", ins.offset, sym.get(&rs))
                    })?;
                    let mut out: Vec<(u32, Res)> = Vec::new();
                    for (table, off) in &cands {
                        let slot = self.bind_id(*table, *off, 32);
                        out.push((((*table as u32) << 24) | off, self.mubuf_res(ins, slot, *table, *off)?));
                    }
                    self.res.insert(idx, vec![out[0].1]);
                    if out.len() > 1 {
                        self.bsel.insert(idx, (rs, out));
                    }
                }
                Enc::Mimg if ins.mnemonic.starts_with("image_bvh") => {
                    for d in &ins.dst {
                        if let Opnd::Vgpr(v) = d {
                            for k in 0..4u16 {
                                sym.remove(&(2000 + *v as u16 + k));
                            }
                        }
                    }
                }
                Enc::Mimg => {
                    let n = ins.src.len();
                    let rs = ins.src.get(n - 2).and_then(sg).ok_or("mimg: bad srsrc")?;
                    let ss = ins.src.get(n - 1).and_then(sg).ok_or("mimg: bad ssamp")?;
                    let da_dim = ins.aux.imm as u32;
                    let tex_c = self.desc_candidates(sym.get(&rs), 32).ok_or_else(|| {
                        format!("mimg at {:#x}: s{rs} not an image descriptor ({:?})", ins.offset, sym.get(&rs))
                    })?;
                    let mut tex_res: Vec<(u32, Res)> = Vec::new();
                    for (ttable, off) in &tex_c {
                        let tslot = self.bind_id(*ttable, *off, 32);
                        let writes = ins.mnemonic.contains("store") || ins.mnemonic.contains("atomic") || self.uav_force.contains(&tslot);
                        let is_uav = self.kind_at(*ttable, *off, writes)? == Some(1);
                        let dim = self.gcn_image_dim(*ttable, *off, is_uav, da_dim);
                        if !is_uav && dim == 1 && msaa_dim_on() && ins.mnemonic == "image_load" && *off & BINDLESS == 0 && *off >= 32 && self.gcn_image_dim(*ttable, *off - 32, false, 1) == 6 {
                            let ms = self.bind_id(*ttable, *off - 32, 32);
                            if ms & BINDLESS == 0 {
                                self.tex.entry(ms).or_insert(6);
                                self.fmask.insert(tslot, ms);
                                if let Some((param, _)) = self.root_param_at(*ttable, self.user_base()) {
                                    self.fmask_loc.insert((param as u32, *off / 32, (*off - 32) / 32));
                                }
                            }
                        }
                        if tslot & BINDLESS != 0 {
                            if is_uav { self.bl_uav.insert(tslot); } else { self.bl_srv.insert(tslot); }
                        }
                        if is_uav && ins.mnemonic.starts_with("image_atomic_") {
                            let op = &ins.mnemonic["image_atomic_".len()..];
                            let is_64 = (op == "cmpswap" && ins.aux.mask == 0xf) || (op != "cmpswap" && ins.aux.mask == 0x3);
                            if is_64 {
                                self.uav_u64.insert(tslot);
                            } else {
                                self.uav_uint.insert(tslot);
                            }
                        }
                        let map = if is_uav { &mut self.uav } else { &mut self.tex };
                        if let Some(prev) = map.insert(tslot, dim) {
                            if prev != dim {
                                return Err(format!("image slot {tslot} used with two dimensions"));
                            }
                        }
                        tex_res.push((((*ttable as u32) << 24) | off, if is_uav { Res::Uav(tslot, dim) } else { Res::Tex(tslot, dim) }));
                    }
                    let mut samp_res: Vec<(u32, Res)> = Vec::new();
                    if ins.mnemonic.starts_with("image_gather4") && ins.mnemonic.split('_').any(|t| t == "l") {
                        let w = |k: u16| u8::try_from(ss + k).ok().and_then(|r| Self::sgpr_const_in(&insns, block_start, idx, r));
                        self.gather_ssamp.insert(idx, w(1).zip(w(2)));
                    }
                    if ins.mnemonic.starts_with("image_sample") || ins.mnemonic.starts_with("image_gather") || ins.mnemonic == "image_get_lod" {
                        let fixed = if sym.get(&ss).is_none() && written.contains(&ss) {
                            match self.fssm_static_sampler(idx, ss)? {
                                Some(slot) => Some(slot),
                                None => self.static_sampler(idx, ss)?,
                            }
                        } else {
                            None
                        };
                        let samp_c = match fixed {
                            Some(slot) => vec![(0xffu8, slot)],
                            None => self.desc_candidates(sym.get(&ss), 16).ok_or_else(|| {
                                format!("mimg at {:#x}: s{ss} not a sampler descriptor ({:?})", ins.offset, sym.get(&ss))
                            })?,
                        };
                        for (stable, off) in &samp_c {
                            let sslot = if fixed.is_some() { *off } else { self.bind_id(*stable, *off, 16) };
                            // Depth-compare variants (opcode bit 3) need a comparison sampler.
                            if ins.op & 8 != 0 {
                                self.samp_cmp.insert(sslot);
                            } else {
                                self.samp.insert(sslot);
                            }
                            samp_res.push((((*stable as u32) << 24) | off, Res::Samp(sslot)));
                        }
                    }
                    let mut v = vec![tex_res[0].1];
                    if let Some(s) = samp_res.first() {
                        v.push(s.1);
                    }
                    if tex_res.len() > 1 || samp_res.len() > 1 {
                        self.msel.insert(idx, MSel { tex_reg: rs, samp_reg: ss, tex: tex_res, samp: samp_res });
                    }
                    self.res.insert(idx, v);
                }

                _ => {
                    for d in &ins.dst {
                        if let Opnd::Vgpr(v) = d {
                            sym.remove(&(2000 + *v as u16));
                        }
                        if let Some(n) = sg(d) {
                            sym.remove(&n);
                            written.insert(n);
                            if ins.mnemonic.ends_with("_b64") && !i32_result_of_64(&ins.mnemonic) {
                                sym.remove(&(n + 1));
                                written.insert(n + 1);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Registers read before any write in program order — i.e. the shader's entry ABI
    /// (interpolation barycentrics, fragment position, user-data SGPRs, thread ids...).
    fn live_in(&self) -> Vec<String> {
        let mut written: BTreeSet<String> = BTreeSet::new();
        let mut live: BTreeSet<(u8, u32)> = BTreeSet::new();
        for ins in &self.insns {
            if ins.mnemonic == "v_nop" {
                continue;
            }
            let dpp_src = dpp_vgpr(ins).map(Opnd::Vgpr);
            for o in ins.src.iter().chain(dpp_src.iter()) {
                if let Opnd::Sgpr(n) | Opnd::Vgpr(n) = o {
                    let nm = name(o).unwrap_or_default();
                    if !written.contains(&nm) {
                        live.insert((if matches!(o, Opnd::Vgpr(_)) { 1 } else { 0 }, *n as u32));
                    }
                }
            }
            if ins.mnemonic.starts_with("image_store") {
                if let Some(Opnd::Vgpr(base)) = ins.dst.first() {
                    for reg in u32::from(*base)..u32::from(*base) + mimg_data_regs(ins) {
                        if !written.contains(&format!("v{reg}")) {
                            live.insert((1, reg));
                        }
                    }
                }
                continue;
            }
            // Multi-register destinations: SMEM loads, MIMG results, 64-bit scalar ops.
            let extra = match ins.enc {
                Enc::Smem if matches!(ins.op, 0x24 | 0x25) => 1,
                Enc::Smem => (1u32 << (ins.op & 7)) - 1,
                Enc::Mimg => mimg_data_regs(ins).saturating_sub(1),
                _ if ins.mnemonic.ends_with("_b64") && !i32_result_of_64(&ins.mnemonic) => 1,
                _ => 0,
            };
            for o in &ins.dst {
                if let Ok(nm) = name(o) {
                    written.insert(nm);
                }
                match o {
                    Opnd::Sgpr(n) => (1..=extra).for_each(|k| { written.insert(format!("s{}", *n as u32 + k)); }),
                    Opnd::Vgpr(n) => (1..=extra).for_each(|k| { written.insert(format!("v{}", *n as u32 + k)); }),
                    _ => {}
                }
            }
        }
        live.iter().map(|(k, n)| format!("{}{}", if *k == 1 { "v" } else { "s" }, n)).collect()
    }

    fn line(&mut self, s: &str) {
        if self.is_cs && (self.lds_w_pending || self.lds_r_pending) && self.brace_depth == 0 && self.loop_depth == 0 {
            let t = s.trim_start();
            let code = t.split("//").next().unwrap_or("");
            if !t.starts_with("//") && !t.starts_with('#') && code.matches('{').count() > code.matches('}').count() {
                self.lds_w_pending = false;
                self.lds_r_pending = false;
                self.body.push_str("    GroupMemoryBarrierWithGroupSync();\n");
            }
        }
        self.body.push_str("    ");
        self.body.push_str(s);
        self.body.push('\n');
        for l in s.lines() {
            let t = l.trim_start();
            if t.starts_with("//") || t.starts_with('#') {
                continue;
            }
            let code = t.split("//").next().unwrap_or("");
            self.brace_depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
        }
    }

    fn lds_order(&mut self, writes: bool, reads: bool) {
        let crossed = (reads && self.lds_w_pending) || (writes && self.lds_r_pending);
        let wave_write = self.one_wave_lds() && writes && self.lds_w_pending;
        let hazard = crossed || wave_write;
        if hazard && self.brace_depth == 0 && self.loop_depth == 0 {
            self.line("GroupMemoryBarrierWithGroupSync();");
            self.lds_w_pending = false;
            self.lds_r_pending = false;
        }
        if writes {
            self.lds_w_pending = true;
        }
        if reads {
            self.lds_r_pending = true;
        }
        if self.brace_depth != 0 || self.loop_depth != 0 {
            self.lds_nested = true;
        }
    }

    fn touch(&mut self, o: &Opnd) {
        if let Opnd::Sgpr(_) | Opnd::Vgpr(_) = o {
            if let Ok(n) = name(o) {
                self.regs.insert(n);
            }
        }
    }

    /// Guarded write to a VGPR (inactive lanes keep their value).
    fn wv(&mut self, dst: &Opnd, expr: &str) -> Result<(), String> {
        self.touch(dst);
        let d = name(dst)?;
        let merged;
        let expr = match &self.d16 {
            Some((dn, hi)) if *dn == d => {
                merged = if *hi { format!("(({d} & 0xffffu) | ((({expr}) & 0xffffu) << 16))") } else { format!("(({d} & 0xffff0000u) | (({expr}) & 0xffffu))") };
                merged.as_str()
            }
            _ => expr,
        };
        const LANE_SENSITIVE: [&str; 12] = ["Wave", "Quad", "ddx", "ddy", "Sample", "Gather", "Load", "Interlocked", "GetDimensions", "Calculate", "Append", "Consume"];
        static BRANCH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let branch = *BRANCH.get_or_init(|| std::env::var("TOMBOX_XLAT_GUARD_BRANCH").map(|v| v == "1").unwrap_or(false));
        if branch || LANE_SENSITIVE.iter().any(|k| expr.contains(k)) || expr.contains(';') || expr.contains('{') {
            self.line(&format!("if (exec_lo & 1u) {d} = {expr};"));
        } else {
            self.line(&format!("{d} = TB_WV({expr}, {d});"));
        }
        Ok(())
    }

    fn ws(&mut self, dst: &Opnd, expr: &str) -> Result<(), String> {
        self.touch(dst);
        let d = name(dst)?;
        match d.as_str() {
            _ => self.line(&format!("{d} = {expr};")),
        }
        Ok(())
    }

    fn quadmask32(&mut self, source: &str) -> String {
        let bits = format!("t{}", self.tmp);
        self.tmp += 1;
        self.line(&format!("uint {bits} = {source};"));
        self.line(&format!("{bits} |= {bits} >> 1;"));
        self.line(&format!("{bits} = ({bits} | ({bits} >> 2)) & 0x11111111u;"));
        self.line(&format!("{bits} = ({bits} | ({bits} >> 3)) & 0x03030303u;"));
        self.line(&format!("{bits} = ({bits} | ({bits} >> 6)) & 0x000f000fu;"));
        self.line(&format!("{bits} = ({bits} | ({bits} >> 12)) & 0xffu;"));
        bits
    }

    /// Condition under which a SOPP branch opcode is taken (None = not a branch).
    fn branch_cond(op: u32) -> Option<&'static str> {
        Some(match op {
            2 => "true",
            4 => "scc == 0u",
            5 => "scc != 0u",
            6 => "(vcc_lo & 1u) == 0u",
            7 => "(vcc_lo & 1u) != 0u",
            8 => "(exec_lo & 1u) == 0u",
            9 => "(exec_lo & 1u) != 0u",
            _ => return None,
        })
    }

    /// Control flow.
    ///
    /// * Forward branches use skip flags: `jT` is set when a branch to instruction T is
    ///   taken, and every instruction before T that lies after such a branch is wrapped in
    ///   `if (!(jT || ...))`. This handles arbitrary forward-only CFGs (if/else, breaks).
    /// * A backward branch at B to header L becomes `do { ... } while (cond)` around
    ///   [L..=B]. Flags for labels inside the body are reset each iteration; flags for
    ///   labels past B (loop exits) terminate the loop and keep skipping until their label.
    ///   Loops must be properly nested, have one back-edge per header, and must not be
    ///   entered from outside except through the header — otherwise `Err`.
    fn emit_body(&mut self, is_ps: bool) -> Result<(), String> {
        let mut off2idx: HashMap<usize, usize> = self.insns.iter().enumerate().map(|(i, x)| (x.offset, i)).collect();
        // A branch to the address just past the last instruction leaves the program (same as
        // reaching the final s_endpgm).
        if let Some(l) = self.insns.last() {
            off2idx.insert(l.offset + l.dwords * 4, self.insns.len());
        }
        let prog_end = self.insns.last().map_or(0, |l| l.offset + l.dwords * 4);
        let mut target: HashMap<usize, usize> = HashMap::new(); // forward: branch idx -> target idx
        let mut back: HashMap<usize, usize> = HashMap::new(); // backward: branch idx B -> header idx L
        let n_insns = self.insns.len();
        for (i, ins) in self.insns.iter().enumerate() {
            if ins.enc == Enc::Sopp && ins.op == 1 && i + 1 < n_insns {
                target.insert(i, n_insns); // early s_endpgm -> end of program
                continue;
            }
            if ins.enc == Enc::Sopp && Self::branch_cond(ins.op).is_some() {
                let t = ins.offset as i64 + 4 + ins.aux.imm * 4;
                let ti = if t >= 0 && t as usize >= prog_end {
                        self.insns.len() // at or beyond the end of the program (s_code_end padding): exit
                    } else {
                        *off2idx
                            .get(&(t as usize))
                            .ok_or_else(|| format!("branch at {:#x} targets mid-instruction {t:#x}", ins.offset))?
                    };
                if ti <= i {
                    back.insert(i, ti);
                } else {
                    target.insert(i, ti);
                }
            }
        }
        // Validate loop structure.
        // Several back-edges to one header = `continue`s. The last one (highest address)
        // closes the loop; each earlier one becomes a forward jump to that closing branch
        // which also forces another iteration (see the `|| j{B}` at the loop bottom).
        let mut heads: HashMap<usize, usize> = HashMap::new(); // L -> B
        for (&b, &l) in &back {
            let e = heads.entry(l).or_insert(b);
            *e = (*e).max(b);
        }
        let continues: Vec<(usize, usize)> =
            back.iter().filter(|(&b, &l)| heads[&l] != b).map(|(&b, &l)| (b, heads[&l])).collect();
        let cont: BTreeSet<usize> = continues.iter().map(|&(b, _)| b).collect();
        for (b, closing) in continues {
            back.remove(&b);
            target.insert(b, closing);
        }
        let mut loops: Vec<(usize, usize)> = heads.iter().map(|(&l, &b)| (l, b)).collect();
        loops.sort_unstable();
        for &(l1, b1) in &loops {
            for &(l2, b2) in &loops {
                let disjoint = b1 < l2 || b2 < l1;
                let nested = (l1 <= l2 && b2 <= b1) || (l2 <= l1 && b1 <= b2);
                if !(disjoint || nested) {
                    // Interleaved loops cannot be expressed as nested do/while: fall back
                    // to the general block dispatcher.
                    return self.emit_body_dispatch(is_ps);
                }
            }
            // A forward branch from before the header into the body (the usual rotated
            // `while`: jump straight to the condition) is handled by the skip flags: the
            // flag stays set on the first iteration, so everything before the label is
            // skipped, and in-body flags are cleared at the bottom of each iteration.
        }
        let plan = if structure_enabled() { plan_structure(&target, &loops, &cont, |b| self.insns[b].enc == Enc::Sopp && Self::branch_cond(self.insns[b].op).is_some(), |b| self.insns[b].enc == Enc::Sopp && self.insns[b].op == 2) } else { Structure::default() };
        for b in plan.regions.keys().chain(plan.breaks.iter()).chain(plan.elses.iter()) {
            target.remove(b);
        }
        let labels: BTreeSet<usize> = target.values().copied().collect();
        for l in &labels {
            self.line(&format!("bool j{l} = false;"));
        }
        if !loops.is_empty() {
            self.line("uint tb_guard = 0u;");
            for &(_, b) in &loops {
                self.line(&format!("bool tb_c{b} = false;"));
            }
        }

        let insns = self.insns.clone();
        let mut open = false;
        let mut cur: Vec<usize> = vec![];
        let mut stack: Vec<(usize, Option<usize>)> = vec![];
        for (i, ins) in insns.iter().enumerate() {
            while let Some(top) = stack.last_mut() {
                if top.1 == Some(i) {
                    if open {
                        self.line("}");
                        open = false;
                    }
                    cur.clear();
                    self.line("} else {");
                    top.1 = None;
                } else if top.0 == i {
                    if open {
                        self.line("}");
                        open = false;
                    }
                    cur.clear();
                    self.line("}");
                    stack.pop();
                } else {
                    break;
                }
            }
            if let Some(&b) = heads.get(&i) {
                if open {
                    self.line("}");
                    open = false;
                }
                cur.clear();
                self.line("do {");
                self.loop_depth += 1;
                self.line(&format!("tb_c{b} = false;"));
            }
            let active: Vec<usize> =
                labels.iter().copied().filter(|&t| t > i && target.iter().any(|(&b, &bt)| bt == t && b < i)).collect();
            if active != cur || (open && heads.contains_key(&i)) {
                if open {
                    self.line("}");
                    open = false;
                }
                if !active.is_empty() {
                    let c: Vec<String> = active.iter().map(|t| format!("j{t}")).collect();
                    self.line(&format!("if (!({})) {{", c.join(" || ")));
                    open = true;
                }
                cur = active;
            }
            for o in ins.src.iter().chain(ins.dst.iter()) {
                self.touch(o);
            }
            self.emit_insn(i, ins, is_ps, target.get(&i).copied())
                .map_err(|e| format!("{e} [{} @ {:#x}]", ins.mnemonic, ins.offset))?;
            if open && target.get(&i).is_some_and(|t| cur.contains(t)) {
                self.line("}");
                open = false;
                cur.clear();
            }
            if plan.breaks.contains(&i) {
                let cond = Self::branch_cond(ins.op).unwrap_or("false");
                self.line(&format!("if ({cond}) break;"));
            }
            if let Some(&(t, els)) = plan.regions.get(&i) {
                if open {
                    self.line("}");
                    open = false;
                }
                cur.clear();
                let cond = Self::branch_cond(ins.op).unwrap_or("false");
                self.line(&format!("if (!({cond})) {{"));
                stack.push((t, els));
            }
            if back.contains_key(&i) {
                // Evaluate the back-edge condition inside the guard (a skipped branch is not taken).
                let cond = Self::branch_cond(ins.op).unwrap_or("false");
                self.line(&format!("tb_c{i} = ({cond});"));
                if labels.contains(&i) {
                    // A `continue` jumped here: iterate again regardless of the condition.
                    self.line(&format!("tb_c{i} = tb_c{i} || j{i};"));
                }
                if open {
                    self.line("}");
                    open = false;
                }
                cur.clear();
                // Clear the flags of labels inside this loop body for the next iteration.
                let head = back[&i];
                for t in labels.iter().filter(|&&t| t > head && t <= i) {
                    self.line(&format!("j{t} = false;"));
                }
                let exits: Vec<String> = labels.iter().filter(|&&t| t > i && (!structure_enabled() || target.iter().any(|(&b, &bt)| bt == t && b >= head && b <= i))).map(|t| format!("j{t}")).collect();
                let exit = if exits.is_empty() { String::new() } else { format!(" && !({})", exits.join(" || ")) };
                self.line(&format!("}} while (tb_c{i}{exit} && ++tb_guard < {}u);", loop_guard(65536)));
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
        }
        if open {
            self.line("}");
        }
        while let Some(top) = stack.pop() {
            if top.1.is_some() {
                self.line("} else {");
            }
            self.line("}");
        }
        Ok(())
    }

    /// General control flow: a `while` + `switch` dispatcher over basic blocks, used when
    /// the CFG is not reducible to nested loops. Each block ends by setting the next block
    /// id (`tb_pc`); the program ends when it reaches the id one past the last instruction.
    /// (Implicit-derivative sampling cannot live inside this loop — FXC rejects it, and the
    /// shader then fails to compile rather than silently changing meaning.)
    fn emit_body_dispatch(&mut self, is_ps: bool) -> Result<(), String> {
        let n = self.insns.len();
        let mut off2idx: HashMap<usize, usize> = self.insns.iter().enumerate().map(|(i, x)| (x.offset, i)).collect();
        if let Some(l) = self.insns.last() {
            off2idx.insert(l.offset + l.dwords * 4, n);
        }
        let prog_end = self.insns.last().map_or(0, |l| l.offset + l.dwords * 4);
        let mut leaders: BTreeSet<usize> = BTreeSet::new();
        leaders.insert(0);
        let mut tgt: HashMap<usize, usize> = HashMap::new();
        for (i, ins) in self.insns.iter().enumerate() {
            if ins.enc != Enc::Sopp {
                continue;
            }
            if Self::branch_cond(ins.op).is_some() {
                let t = ins.offset as i64 + 4 + ins.aux.imm * 4;
                let ti = if t >= 0 && t as usize >= prog_end {
                        self.insns.len() // at or beyond the end of the program (s_code_end padding): exit
                    } else {
                        *off2idx
                            .get(&(t as usize))
                            .ok_or_else(|| format!("branch at {:#x} targets mid-instruction {t:#x}", ins.offset))?
                    };
                tgt.insert(i, ti);
                leaders.insert(ti);
                leaders.insert(i + 1);
            } else if ins.op == 1 {
                leaders.insert(i + 1);
            }
        }
        let starts: Vec<usize> = leaders.into_iter().filter(|&s| s < n).collect();
        self.loop_depth = 0;
        self.line("uint tb_pc = 0u; uint tb_guard = 0u;");
        self.line(&format!("[loop] while (tb_pc != {n}u && tb_guard++ < {}u) {{", loop_guard(1000000)));
        self.loop_depth += 1;
        // Flat `if (tb_sel == K)` tests on a snapshot of the block id (FXC mishandles a
        // `switch` whose selector is assigned inside its cases).
        self.line("uint tb_sel = tb_pc; tb_pc = 0xffffffffu;");
        let insns = self.insns.clone();
        for (b, &start) in starts.iter().enumerate() {
            let end = starts.get(b + 1).copied().unwrap_or(n);
            self.line(&format!("if (tb_sel == {start}u) {{"));
            for i in start..end {
                let ins = &insns[i];
                for o in ins.src.iter().chain(ins.dst.iter()) {
                    self.touch(o);
                }
                let is_ctl = ins.enc == Enc::Sopp && (Self::branch_cond(ins.op).is_some() || ins.op == 1);
                if !is_ctl {
                    self.emit_insn(i, ins, is_ps, None)
                        .map_err(|e| format!("{e} [{} @ {:#x}]", ins.mnemonic, ins.offset))?;
                }
            }
            let last = &insns[end - 1];
            let next = if last.enc == Enc::Sopp && last.op == 1 {
                format!("{n}u")
            } else if let (Some(c), Some(&t)) = (
                if last.enc == Enc::Sopp { Self::branch_cond(last.op) } else { None },
                tgt.get(&(end - 1)),
            ) {
                if last.op == 2 { format!("{t}u") } else { format!("({c}) ? {t}u : {end}u") }
            } else {
                format!("{end}u")
            };
            self.line(&format!("tb_pc = {next}; }}"));
        }
        // No block matched (cannot happen for a well-formed program): stop.
        self.line(&format!("if (tb_pc == 0xffffffffu) tb_pc = {n}u;"));
        self.line("}");
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(())
    }

    /// Float source with VOP3 abs/neg modifiers applied.
    fn fm(&self, ins: &Insn, i: usize) -> Result<String, String> {
        let mut e = f(&ins.src[i])?;
        if ins.enc == Enc::Vop3 {
            if (ins.aux.mask >> i) & 1 != 0 {
                e = format!("abs({e})");
            }
            if (ins.aux.mask >> (3 + i)) & 1 != 0 {
                e = format!("(-{e})");
            }
        }
        Ok(e)
    }

    /// Wrap a float result with VOP3 omod/clamp and convert to uint bits.
    fn emit_f64(&mut self, ins: &Insn, m: &str) -> Result<(), String> {
        self.uses_f64 = true;
        let s = &ins.src;
        let d = ins.dst.first().cloned().ok_or("no dst")?;
        for o in s.iter().chain(ins.dst.iter()) {
            if matches!(o, Opnd::Vgpr(_) | Opnd::Sgpr(_) | Opnd::VccLo | Opnd::ExecLo) {
                self.touch(o);
                self.touch(&hi64(o));
            }
        }
        let vop3 = ins.enc == Enc::Vop3;
        let mods = if !vop3 { 0 } else if m == "v_div_scale_f64" { ins.aux.mask & 0x38 } else { ins.aux.mask };
        let a = |i: usize| -> Result<String, String> {
            let mut e = d64(s.get(i).ok_or("missing operand")?)?;
            if (mods >> i) & 1 != 0 {
                e = format!("abs({e})");
            }
            if (mods >> (3 + i)) & 1 != 0 {
                e = format!("(-{e})");
            }
            Ok(e)
        };
        let res = |e: String| -> String {
            let mut e = e;
            if vop3 {
                e = match ins.aux.imm2 {
                    1 => format!("(({e}) * 2.0L)"),
                    2 => format!("(({e}) * 4.0L)"),
                    3 => format!("(({e}) * 0.5L)"),
                    _ => e,
                };
                if ins.aux.imm != 0 {
                    e = format!("clamp({e}, 0.0L, 1.0L)");
                }
            }
            e
        };
        let sdst = |raw0: u32| -> Result<Opnd, String> {
            match (raw0 >> 8) & 0x7f {
                c @ 0..=105 => Ok(Opnd::Sgpr(c as u8)),
                106 => Ok(Opnd::VccLo),
                125 => Ok(Opnd::Null),
                c => Err(format!("carry-out register code {c} unsupported")),
            }
        };
        if let Some(rest) = m.strip_prefix("v_cmpx_").or_else(|| m.strip_prefix("v_cmp_")) {
            let c = if rest == "class_f64" {
                format!("tb_cmp_class64({}, {}, {})", r(&s[0])?, fcmp::class_src(r(&hi64(&s[0]))?, 0x8000_0000, mods)?, r(&s[1])?)
            } else {
                let cc = rest.strip_suffix("_f64").ok_or("bad f64 compare")?;
                let (x, y) = (a(0)?, a(1)?);
                fcmp::cond(cc, &x, &y, &fcmp::either("f64", s.first().ok_or("missing operand")?, s.get(1).ok_or("missing operand")?)?)?
            };
            if m.starts_with("v_cmpx_") {
                self.line(&format!("exec_lo = (((exec_lo & 1u) != 0u) && {c}) ? 1u : 0u;"));
                return Ok(());
            }
            self.ws(&d, &format!("(((exec_lo & 1u) != 0u) && {c}) ? 1u : 0u"))?;
            let dh = hi(&d);
            if !matches!(dh, Opnd::Null | Opnd::ConstInt(_)) {
                self.touch(&dh);
                self.ws(&dh, "0u")?;
            }
            return Ok(());
        }
        let t = self.tmp;
        self.tmp += 1;
        if let Some(op) = crate::trans32::op64_index(m).filter(|_| !f64_native()) {
            self.uses_trans32 = true;
            let x = d64(s.first().ok_or("missing operand")?)?;
            self.line(&format!("uint t{t}l, t{t}h; asuint({x}, t{t}l, t{t}h);"));
            if mods & 1 != 0 {
                self.line(&format!("t{t}h &= 0x7fffffffu;"));
            }
            if (mods >> 3) & 1 != 0 {
                self.line(&format!("t{t}h ^= 0x80000000u;"));
            }
            self.line(&format!("uint2 t{t}r = tb_t64({op}u, t{t}l, t{t}h);"));
            if vop3 && (ins.aux.imm2 != 0 || ins.aux.imm != 0) {
                let e = res(format!("asdouble(t{t}r.x, t{t}r.y)"));
                self.line(&format!("double t{t}d = {e}; uint t{t}a, t{t}b; asuint(t{t}d, t{t}a, t{t}b);"));
                self.wv(&d, &format!("t{t}a"))?;
                return self.wv(&hi64(&d), &format!("t{t}b"));
            }
            self.wv(&d, &format!("t{t}r.x"))?;
            return self.wv(&hi64(&d), &format!("t{t}r.y"));
        }
        let mut flag: Option<(Opnd, String)> = None;
        let e = match m {
            "v_add_f64" => res(format!("({} + {})", a(0)?, a(1)?)),
            "v_mul_f64" => res(format!("({} * {})", a(0)?, a(1)?)),
            "v_fma_f64" => res(format!("fma({}, {}, {})", a(0)?, a(1)?, a(2)?)),
            "v_min_f64" => res(format!("min({}, {})", a(0)?, a(1)?)),
            "v_max_f64" => res(format!("max({}, {})", a(0)?, a(1)?)),
            "v_ldexp_f64" => res(format!("tb_ldexp64({}, asint({}))", a(0)?, r(&s[1])?)),
            "v_div_fixup_f64" => res(format!("tb_div_fixup64({}, {}, {})", a(0)?, a(1)?, a(2)?)),
            "v_div_fmas_f64" => res(format!("tb_div_fmas64({}, {}, {}, vcc_lo)", a(0)?, a(1)?, a(2)?)),
            "v_div_scale_f64" => {
                let (x, y, z) = (a(0)?, a(1)?, a(2)?);
                self.line(&format!("uint t{t}v; double t{t}q = tb_div_scale64({x}, {y}, {z}, t{t}v);"));
                flag = Some((sdst(ins.raw[0])?, format!("(((exec_lo & 1u) != 0u) ? t{t}v : 0u)")));
                res(format!("t{t}q"))
            }
            "v_trig_preop_f64" => res(format!("tb_trig_preop64({}, {})", a(0)?, r(&s[1])?)),
            "v_trunc_f64" => res(format!("tb_trunc64({})", a(0)?)),
            "v_floor_f64" => res(format!("tb_floor64({})", a(0)?)),
            "v_ceil_f64" => res(format!("tb_ceil64({})", a(0)?)),
            "v_rndne_f64" => res(format!("tb_rndne64({})", a(0)?)),
            "v_fract_f64" => res(format!("tb_fract64({})", a(0)?)),
            "v_rcp_f64" => res(format!("(1.0L / {})", a(0)?)),
            "v_sqrt_f64" => res(format!("tb_sqrt64({})", a(0)?)),
            "v_rsq_f64" => res(format!("(1.0L / tb_sqrt64({}))", a(0)?)),
            "v_frexp_mant_f64" => res(format!("tb_frexp_mant64({})", a(0)?)),
            "v_cvt_f64_f32" => res(format!("(double)({})", self.fm(ins, 0)?)),
            "v_cvt_f64_i32" => res(format!("(double)asint({})", r(&s[0])?)),
            "v_cvt_f64_u32" => res(format!("(double)({})", r(&s[0])?)),
            "v_cvt_f32_f64" => {
                self.line(&format!("double t{t} = {};", a(0)?));
                return self.wv(&d, &format!("asuint({})", self.fres(ins, format!("(float)t{t}"))));
            }
            "v_cvt_i32_f64" => {
                self.line(&format!("double t{t} = {};", a(0)?));
                return self.wv(&d, &format!("(tb_nan(t{t}) ? 0u : asuint((int)clamp(t{t}, -2147483648.0L, 2147483647.0L)))"));
            }
            "v_cvt_u32_f64" => {
                self.line(&format!("double t{t} = {};", a(0)?));
                return self.wv(&d, &format!("(tb_nan(t{t}) ? 0u : (uint)clamp(t{t}, 0.0L, 4294967295.0L))"));
            }
            "v_frexp_exp_i32_f64" => return self.wv(&d, &format!("asuint(tb_frexp_exp64({}))", a(0)?)),
            "v_mad_u64_u32" | "v_mad_i64_i32" => {
                let (x, y) = (r(&s[0])?, r(&s[1])?);
                let (cl, ch) = (r(&s[2])?, r(&hi64(&s[2]))?);
                let signed = m == "v_mad_i64_i32";
                let ph = if signed { format!("tb_mulhi_i32({x}, {y})") } else { format!("tb_mulhi_u32({x}, {y})") };
                self.line(&format!(
                    "uint t{t}pl = {x} * {y}; uint t{t}ph = {ph}; uint t{t}l = t{t}pl + {cl}; uint t{t}c = (t{t}l < t{t}pl) ? 1u : 0u; uint t{t}s = t{t}ph + {ch}; uint t{t}h = t{t}s + t{t}c;"
                ));
                let co = if signed {
                    format!("((((t{t}h ^ ((t{t}ph ^ t{t}h) & ({ch} ^ t{t}h))) >> 31) != 0u) ? 1u : 0u)")
                } else {
                    format!("(((t{t}s < t{t}ph) || (t{t}h < t{t}s)) ? 1u : 0u)")
                };
                let cd = sdst(ins.raw[0])?;
                if !matches!(cd, Opnd::Null) {
                    self.touch(&cd);
                    self.ws(&cd, &format!("((exec_lo & 1u) != 0u) ? {co} : 0u"))?;
                }
                self.wv(&d, &format!("t{t}l"))?;
                return self.wv(&hi64(&d), &format!("t{t}h"));
            }
            other => return Err(format!("f64 op {other} unsupported")),
        };
        self.line(&format!("double t{t} = {e}; uint t{t}l, t{t}h; asuint(t{t}, t{t}l, t{t}h);"));
        self.wv(&d, &format!("t{t}l"))?;
        self.wv(&hi64(&d), &format!("t{t}h"))?;
        if let Some((fd, v)) = flag {
            if !matches!(fd, Opnd::Null) {
                self.touch(&fd);
                self.ws(&fd, &v)?;
            }
        }
        Ok(())
    }

    fn fres(&self, ins: &Insn, e: String) -> String {
        let mut e = e;
        if ins.enc == Enc::Vop3 {
            e = match ins.aux.imm2 {
                1 => format!("(({e}) * 2.0)"),
                2 => format!("(({e}) * 4.0)"),
                3 => format!("(({e}) * 0.5)"),
                _ => e,
            };
            if ins.aux.imm != 0 {
                e = format!("saturate({e})");
            }
        }
        format!("asuint({e})")
    }

    fn cmp_expr(m: &str, a: &Opnd, b: &Opnd) -> Result<String, String> {
        Self::cmp_expr_mod(m, a, b, 0, 0)
    }

    fn cmp_expr_mod(m: &str, a: &Opnd, b: &Opnd, mods: u8, opsel: u8) -> Result<String, String> {
        // m like "v_cmp_lt_f32" / "s_cmp_eq_i32"
        let parts: Vec<&str> = m.split('_').collect();
        let (cc, ty) = (parts[2], parts[3]);
        let fmod = |e: String, i: u8| -> String {
            let e = if (mods >> i) & 1 != 0 { format!("abs({e})") } else { e };
            if (mods >> (3 + i)) & 1 != 0 { format!("(-{e})") } else { e }
        };
        let hs = |i: u8| (opsel >> i) & 1 != 0;
        if matches!(ty, "i16" | "u16") && mods != 0 {
            return Err(format!("{m}: VOP3 abs/neg on an integer compare"));
        }
        let int16 = |o: &Opnd, i: u8| -> Result<String, String> {
            if matches!(o, Opnd::ConstF32(_)) {
                return Err(format!("{m}: inline float constant in a 16-bit integer compare"));
            }
            let x = half16(o, hs(i))?;
            Ok(if ty == "i16" { format!("(asint({x} << 16) >> 16)") } else { x })
        };
        let (x, y) = match ty {
            "f32" => (fmod(f(a)?, 0), fmod(f(b)?, 1)),
            "f16" => {
                let h = |o: &Opnd, i: u8| -> Result<String, String> {
                    Ok(match o {
                        Opnd::ConstF32(_) if !hs(i) => f(o)?,
                        _ => format!("f16tof32{}", half16(o, hs(i))?),
                    })
                };
                (fmod(h(a, 0)?, 0), fmod(h(b, 1)?, 1))
            }
            "i16" | "u16" => (int16(a, 0)?, int16(b, 1)?),
            "i32" => (format!("asint({})", r(a)?), format!("asint({})", r(b)?)),
            "u32" => (r(a)?, r(b)?),
            other => return Err(format!("compare type {other} unsupported")),
        };
        if ty == "f16" && opsel & 3 != 0 {
            let nan = |o: &Opnd, i: u8| -> Result<String, String> {
                if hs(i) { Ok(format!("(({} & 0x7fffu) > 0x7c00u)", half16(o, true)?)) } else { fcmp::nan("f16", o) }
            };
            return fcmp::cond(cc, &x, &y, &format!("({} || {})", nan(a, 0)?, nan(b, 1)?));
        }
        let float = matches!(ty, "f32" | "f16");
        if float {
            return fcmp::cond(cc, &x, &y, &fcmp::either(ty, a, b)?);
        }
        Ok(match cc {
            "f" => "false".into(),
            "lt" => format!("({x} < {y})"),
            "eq" => format!("({x} == {y})"),
            "le" => format!("({x} <= {y})"),
            "gt" => format!("({x} > {y})"),
            "lg" if float => format!("(({x} < {y}) || ({x} > {y}))"),
            "lg" | "ne" => format!("({x} != {y})"),
            "ge" => format!("({x} >= {y})"),
            "o" => format!("({x} == {x} && {y} == {y})"),
            "u" => format!("({x} != {x} || {y} != {y})"),
            "nge" => format!("(!({x} >= {y}))"),
            "nlg" => format!("(!(({x} < {y}) || ({x} > {y})))"),
            "ngt" => format!("(!({x} > {y}))"),
            "nle" => format!("(!({x} <= {y}))"),
            "neq" => format!("(!({x} == {y}))"),
            "nlt" => format!("(!({x} < {y}))"),
            "tru" | "t" => "true".into(),
            other => return Err(format!("compare cond {other} unsupported")),
        })
    }

    fn coherent_key(slot: u32) -> u32 {
        if slot & BINDLESS != 0 { BINDLESS } else { slot }
    }

    fn is_coherent(&self, slot: u32) -> bool {
        let k = Self::coherent_key(slot);
        self.coherent_read.contains(&k) && self.uav_written.contains(&k)
    }

    fn note_coherent(&mut self, idx: usize, ins: &Insn) {
        let w = ins.raw.first().copied().unwrap_or(0);
        let glc = match ins.enc {
            Enc::Mubuf | Enc::Mtbuf => (w >> 14) & 3 != 0,
            Enc::Mimg => (w >> 13) & 1 != 0 || (w >> 7) & 1 != 0,
            _ => return,
        };
        let m = ins.mnemonic.as_str();
        let writes = m.contains("store") || m.contains("_atomic");
        if !writes && !glc {
            return;
        }
        let slots: Vec<u32> = self
            .res
            .get(&idx)
            .into_iter()
            .flatten()
            .filter_map(|r| match *r {
                Res::Buf(slot, true) | Res::TBuf(slot, true) | Res::Uav(slot, _) => Some(Self::coherent_key(slot)),
                _ => None,
            })
            .collect();
        let set = if writes { &mut self.uav_written } else { &mut self.coherent_read };
        set.extend(slots);
    }

    fn emit_insn(&mut self, idx: usize, ins: &Insn, is_ps: bool, br: Option<usize>) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        let s = &ins.src;
        let d = ins.dst.first().cloned();
        self.line(&format!("// {:#06x} {m}", ins.offset));
        self.cur_idx = idx;
        self.note_coherent(idx, ins);
        if let Some(&id) = self.epilog_pack.get(&idx) {
            // Export epilogue: keep the unpacked operands (see `find_export_epilogs`).
            for (k, sfx) in ["a", "b"].iter().enumerate() {
                let o = *s.get(k).ok_or("pack without two operands")?;
                self.touch(&o);
                self.regs.insert(format!("tb_pk{id}{sfx}"));
                self.line(&format!("tb_pk{id}{sfx} = {};", r(&o)?));
            }
        }
        // Run-time descriptor selectors (only for shaders with a "one of N" image op).
        if let Some((reg, cands)) = self.bsel.remove(&idx) {
            // One guarded copy of the op per candidate buffer.
            self.regs.insert(format!("tb_dsel{reg}"));
            let mut first = true;
            for (id, res) in &cands {
                self.line(&format!("{}if (tb_dsel{reg} == {id:#x}u) {{", if first { "" } else { "else " }));
                first = false;
                self.res.insert(idx, vec![*res]);
                self.emit_insn(idx, ins, is_ps, br)?;
                self.line("}");
            }
            self.bsel.insert(idx, (reg, cands));
            return Ok(());
        }
        if !self.msel.is_empty() || !self.bsel.is_empty() {
            if let Some(&(d, id)) = self.dsel_loads.get(&idx) {
                self.regs.insert(format!("tb_dsel{d}"));
                self.line(&format!("tb_dsel{d} = {id:#x}u;"));
            }
            if let Some(&(d, s)) = self.dsel_moves.get(&idx) {
                self.regs.insert(format!("tb_dsel{d}"));
                self.regs.insert(format!("tb_dsel{s}"));
                self.line(&format!("tb_dsel{d} = tb_dsel{s};"));
            }
        }
        if ins.enc == Enc::Mimg {
            if let Some(sel) = self.msel.get(&idx).cloned() {
                // One guarded copy of the op per (texture, sampler) candidate pair.
                self.regs.insert(format!("tb_dsel{}", sel.tex_reg));
                self.regs.insert(format!("tb_dsel{}", sel.samp_reg));
                let samps: Vec<Option<(u32, Res)>> =
                    if sel.samp.is_empty() { vec![None] } else { sel.samp.iter().copied().map(Some).collect() };
                let mut first = true;
                for (tid, tres) in &sel.tex {
                    for s in &samps {
                        let mut cond = vec![];
                        if sel.tex.len() > 1 {
                            cond.push(format!("tb_dsel{} == {tid:#x}u", sel.tex_reg));
                        }
                        let mut v = vec![*tres];
                        if let Some((sid, sres)) = s {
                            if sel.samp.len() > 1 {
                                cond.push(format!("tb_dsel{} == {sid:#x}u", sel.samp_reg));
                            }
                            v.push(*sres);
                        }
                        self.line(&format!("{}if ({}) {{", if first { "" } else { "else " }, cond.join(" && ")));
                        first = false;
                        self.res.insert(idx, v);
                        self.emit_mimg(idx, ins)?;
                        self.line("}");
                    }
                }
                return Ok(());
            }
        }
        match ins.enc {
            Enc::Sopp if ins.op == 0x10 && !self.is_gs && matches!(ins.aux.imm as u32 & 0xf, 1 | 4 | 5 | 7 | 8) => Ok(()),
            Enc::Sopp if ins.op == 0x10 => self.emit_sendmsg(ins),
            Enc::Sopp if matches!(ins.op, 0x24 | 0x25) => {
                if self.reads_hwreg(1) || self.mode_use.sp || self.mode_use.dp {
                    self.hw_regs.insert(1);
                    let (mask, v) = if ins.op == 0x24 { (0xfu32, ins.aux.imm as u32 & 0xf) } else { (0xf0u32, (ins.aux.imm as u32 & 0xf) << 4) };
                    self.line(&format!("tb_hw1 = (tb_hw1 & {:#x}u) | {v:#x}u;", !mask));
                }
                Ok(())
            }
            Enc::Sopp if ins.op == 0x0d => {
                if ins.aux.imm & 1 != 0 {
                    return Err("s_sethalt 1 halts the wave until a debugger resumes it".into());
                }
                Ok(())
            }
            Enc::Sopp if ins.op == 0x11 => Err("s_sendmsghalt halts the wave after the message".into()),
            Enc::Sopp if ins.op == 0x12 => Ok(()),
            Enc::Sopp if ins.op == 0x0a && self.is_ms() => self.mesh_barrier(),
            Enc::Sopp if ins.op == 0x0a && self.is_cs => {
                self.lds_w_pending = false;
                self.lds_r_pending = false;
                self.line("GroupMemoryBarrierWithGroupSync();");
                Ok(())
            }
            Enc::Sopp if ins.op == 0x0e && self.reads_hwreg(29) => {
                self.hw_regs.insert(29);
                self.line(&format!("tb_hw29 = (tb_hw29 + {}u) & 0xfffffu;", 64 * (ins.aux.imm as u32 & 0x7f)));
                Ok(())
            }
            Enc::Sopp if ins.op == 0x0f && self.reads_hwreg(2) => {
                self.hw_regs.insert(2);
                self.line(&format!("tb_hw2 = (tb_hw2 & ~0x18u) | {:#x}u;", (ins.aux.imm as u32 & 3) << 3));
                Ok(())
            }
            Enc::Sopp => {
                let cond = match ins.op {
                    2 => Some("true".to_string()),
                    4 => Some("scc == 0u".into()),
                    5 => Some("scc != 0u".into()),
                    6 => Some("(vcc_lo & 1u) == 0u".into()),
                    7 => Some("(vcc_lo & 1u) != 0u".into()),
                    8 => Some("(exec_lo & 1u) == 0u".into()),
                    9 => Some("(exec_lo & 1u) != 0u".into()),
                    // A non-final s_endpgm ends the program on this path: jump to the end label.
                    1 if br.is_some() => Some("true".to_string()),
                    0 | 1 | 0x03 | 0x0a | 0x0b | 0x0c | 0x0e | 0x0f | 0x13 | 0x14 | 0x15 | 0x16 | 0x17..=0x1a | 0x1f | 0x20 | 0x21 | 0x22 | 0x23 | 0x24 | 0x25 | 0x28 => None,
                    _ => return Err("unsupported SOPP".into()),
                };
                if let (Some(c), Some(t)) = (cond, br) {
                    self.line(&format!("if ({c}) j{t} = true;"));
                }
                Ok(())
            }
            Enc::Vop3 if matches!(ins.op, 0x200 | 0x201 | 0x202 | 0x342 | 0x343 | 0x35a) => self.emit_interp(ins, is_ps),
            Enc::Vop2 | Enc::Vop1 | Enc::Vop3 | Enc::Vopc => {
                self.d16 = None;
                if ins.aux.sdwa.is_none() && dst16(&ins.mnemonic) {
                    if let Some(dn) = ins.dst.first().and_then(|o| name(o).ok()) {
                        let hi = ins.enc == Enc::Vop3 && (ins.raw[0] >> 14) & 1 != 0;
                        self.d16 = Some((dn, hi));
                    }
                }
                let r = self.emit_vec_mode(ins);
                self.d16 = None;
                r
            }
            Enc::Sop2 => {
                let d = d.ok_or("no dst")?;
                let (a, b) = (r(&s[0])?, r(&s[1])?);
                if m == "s_bfe_u64" || m == "s_bfe_i64" {
                    self.touch(&hi(&s[0]));
                    let ah = r(&hi(&s[0]))?;
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!(
                        "uint t{t}o = {b} & 63u; uint t{t}w = min(({b} >> 16) & 127u, 64u); uint t{t}l = (t{t}o == 0u) ? {a} : ((t{t}o >= 32u) ? ({ah} >> (t{t}o - 32u)) : (({a} >> t{t}o) | ({ah} << (32u - t{t}o)))); uint t{t}h = (t{t}o >= 32u) ? 0u : ({ah} >> t{t}o);"
                    ));
                    if m == "s_bfe_i64" {
                        self.line(&format!(
                            "if (t{t}o >= 32u) {{ t{t}l = asuint(asint({ah}) >> (t{t}o - 32u)); t{t}h = asuint(asint({ah}) >> 31); }} else if (t{t}o != 0u) {{ t{t}h = asuint(asint({ah}) >> t{t}o); }}"
                        ));
                    }
                    self.line(&format!(
                        "if (t{t}w < 32u) {{ t{t}l &= (1u << t{t}w) - 1u; t{t}h = 0u; }} else if (t{t}w < 64u) {{ t{t}h &= (1u << (t{t}w - 32u)) - 1u; }}"
                    ));
                    if m == "s_bfe_i64" {
                        self.line(&format!(
                            "if (t{t}w != 0u && t{t}w < 32u) {{ t{t}l = asuint(asint(t{t}l << (32u - t{t}w)) >> (32u - t{t}w)); t{t}h = asuint(asint(t{t}l) >> 31); }} else if (t{t}w > 32u && t{t}w < 64u) {{ t{t}h = asuint(asint(t{t}h << (64u - t{t}w)) >> (64u - t{t}w)); }} else if (t{t}w == 32u) {{ t{t}h = asuint(asint(t{t}l) >> 31); }}"
                        ));
                    }
                    self.ws(&d, &format!("t{t}l"))?;
                    let dh = hi(&d);
                    self.touch(&dh);
                    self.ws(&dh, &format!("t{t}h"))?;
                    self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                    return Ok(());
                }
                if m == "s_lshr_b64" || m == "s_ashr_i64" {
                    self.touch(&hi(&s[0]));
                    let ah = r(&hi(&s[0]))?;
                    let n = format!("({b} & 63u)");
                    let fill = if m == "s_ashr_i64" { format!("asuint(asint({ah}) >> 31)") } else { "0u".to_string() };
                    let top: fn(&str, &str) -> String =
                        if m == "s_ashr_i64" { |x, k| format!("asuint(asint({x}) >> {k})") } else { |x, k| format!("({x} >> {k})") };
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!(
                        "uint t{t}l = ({n} == 0u) ? {a} : (({n} >= 32u) ? {} : (({a} >> ({n} & 31u)) | ({ah} << (32u - ({n} & 31u))))); uint t{t}h = ({n} >= 32u) ? {fill} : {};",
                        top(&ah, &format!("({n} - 32u)")),
                        top(&ah, &format!("({n} & 31u)"))
                    ));
                    self.ws(&d, &format!("t{t}l"))?;
                    let dh = hi(&d);
                    self.touch(&dh);
                    self.ws(&dh, &format!("t{t}h"))?;
                    self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                    return Ok(());
                }
                if m == "s_bfm_b64" || m == "s_cselect_b64" || m == "s_lshl_b64" {
                    let (lo, hi_e) = if m == "s_lshl_b64" {
                        let ah = r(&hi(&s[0]))?;
                        (
                            format!("((({b} & 63u) >= 32u) ? 0u : ({a} << ({b} & 31u)))"),
                            format!(
                                "((({b} & 63u) == 0u) ? {ah} : ((({b} & 63u) >= 32u) ? ({a} << (({b} & 63u) - 32u)) : (({ah} << ({b} & 31u)) | ({a} >> (32u - ({b} & 31u))))))"
                            ),
                        )
                    } else if m == "s_bfm_b64" {
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!(
                            "uint t{t}w = {a} & 63u, t{t}o = {b} & 63u; uint t{t}m = (t{t}w >= 32u) ? 0xffffffffu : ((1u << t{t}w) - 1u); uint t{t}n = (t{t}w > 32u) ? (0xffffffffu >> (64u - t{t}w)) : 0u;"
                        ));
                        (
                            format!("((t{t}o >= 32u) ? 0u : (t{t}m << t{t}o))"),
                            format!("((t{t}o == 0u) ? t{t}n : ((t{t}o >= 32u) ? (t{t}m << (t{t}o - 32u)) : ((t{t}n << t{t}o) | (t{t}m >> (32u - t{t}o)))))"),
                        )
                    } else {
                        (
                            format!("((scc != 0u) ? {a} : {b})"),
                            format!("((scc != 0u) ? {} : {})", r(&hi(&s[0]))?, r(&hi(&s[1]))?),
                        )
                    };
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint t{t}l = {lo}; uint t{t}h = {hi_e};"));
                    self.ws(&d, &format!("t{t}l"))?;
                    let dh = hi(&d);
                    self.touch(&dh);
                    self.ws(&dh, &format!("t{t}h"))?;
                    if m == "s_lshl_b64" {
                        self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                    }
                    return Ok(());
                }
                let is64 = m.ends_with("_b64");
                let bit = |op: &str, a: &str, b: &str| -> Option<String> {
                    Some(match op {
                        "and" => format!("({a} & {b})"),
                        "or" => format!("({a} | {b})"),
                        "xor" => format!("({a} ^ {b})"),
                        "andn2" => format!("({a} & ~{b})"),
                        "orn2" => format!("({a} | ~{b})"),
                        "nand" => format!("(~({a} & {b}))"),
                        "nor" => format!("(~({a} | {b}))"),
                        "xnor" => format!("(~({a} ^ {b}))"),
                        _ => return None,
                    })
                };
                let opname = m.trim_start_matches("s_").trim_end_matches("_b32").trim_end_matches("_b64");
                if let Some(lo) = bit(opname, &a, &b) {
                    if is64 {
                        self.touch(&hi(&s[0]));
                        self.touch(&hi(&s[1]));
                        let (ah, bh) = (r(&hi(&s[0]))?, r(&hi(&s[1]))?);
                        let hi_e = bit(opname, &ah, &bh).unwrap();
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t}l = {lo}; uint t{t}h = {hi_e};"));
                        self.ws(&d, &format!("t{t}l"))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, &format!("t{t}h"))?;
                        self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                    } else {
                        self.ws(&d, &lo)?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                    }
                    return Ok(());
                }
                let e = match m {
                    "s_add_u32" | "s_add_i32" => format!("({a} + {b})"),
                    "s_sub_u32" | "s_sub_i32" => format!("({a} - {b})"),
                    "s_addc_u32" => format!("({a} + {b} + (scc & 1u))"),
                    "s_subb_u32" => format!("({a} - {b} - (scc & 1u))"),
                    "s_mul_i32" => format!("({a} * {b})"),
                    "s_mul_hi_u32" => format!("tb_mulhi_u32({a}, {b})"),
                    "s_mul_hi_i32" => format!("tb_mulhi_i32({a}, {b})"),
                    "s_lshl_b32" => format!("({a} << ({b} & 31u))"),
                    "s_lshr_b32" => format!("({a} >> ({b} & 31u))"),
                    "s_ashr_i32" => format!("asuint(asint({a}) >> ({b} & 31u))"),
                    "s_min_u32" => format!("min({a}, {b})"),
                    "s_max_u32" => format!("max({a}, {b})"),
                    "s_min_i32" => format!("asuint(min(asint({a}), asint({b})))"),
                    "s_max_i32" => format!("asuint(max(asint({a}), asint({b})))"),
                    "s_cselect_b32" => format!("((scc != 0u) ? {a} : {b})"),
                    "s_bfe_u32" => format!("(((({b} >> 16) & 127u) >= 32u) ? ({a} >> ({b} & 31u)) : (({a} >> ({b} & 31u)) & ((1u << (({b} >> 16) & 31u)) - 1u)))"),
                    "s_bfe_i32" => format!("tb_bfe_i32({a}, {b})"),
                    "s_bfm_b32" => format!("(((1u << ({a} & 31u)) - 1u) << ({b} & 31u))"),
                    "s_lshl1_add_u32" => format!("(({a} << 1) + {b})"),
                    "s_lshl2_add_u32" => format!("(({a} << 2) + {b})"),
                    "s_lshl3_add_u32" => format!("(({a} << 3) + {b})"),
                    "s_lshl4_add_u32" => format!("(({a} << 4) + {b})"),
                    "s_absdiff_i32" => format!("asuint(abs(asint({a}) - asint({b})))"),
                    "s_pack_ll_b32_b16" => format!("(({a} & 0xffffu) | ({b} << 16))"),
                    "s_pack_lh_b32_b16" => format!("(({a} & 0xffffu) | ({b} & 0xffff0000u))"),
                    "s_pack_hh_b32_b16" => format!("(({a} >> 16) | ({b} & 0xffff0000u))"),
                    _ => return Err("unsupported SOP2".into()),
                };
                let scc = match m {
                    "s_add_u32" => Some(format!("(({a} + {b}) < {a}) ? 1u : 0u")),
                    "s_addc_u32" => Some(format!("((({a} + {b}) < {a}) || (({a} + {b} + (scc & 1u)) < ({a} + {b}))) ? 1u : 0u")),
                    "s_subb_u32" => Some(format!("(({b} > {a}) || ((scc & 1u) > ({a} - {b}))) ? 1u : 0u")),
                    "s_sub_u32" => Some(format!("({b} > {a}) ? 1u : 0u")),
                    "s_add_i32" => Some(format!("((((({a}) ^ ({a} + {b})) & (({b}) ^ ({a} + {b}))) >> 31) != 0u) ? 1u : 0u")),
                    "s_sub_i32" => Some(format!("((((({a}) ^ ({b})) & (({a}) ^ ({a} - {b}))) >> 31) != 0u) ? 1u : 0u")),
                    "s_min_u32" => Some(format!("({a} < {b}) ? 1u : 0u")),
                    "s_max_u32" => Some(format!("({a} > {b}) ? 1u : 0u")),
                    "s_min_i32" => Some(format!("(asint({a}) < asint({b})) ? 1u : 0u")),
                    "s_max_i32" => Some(format!("(asint({a}) > asint({b})) ? 1u : 0u")),
                    "s_lshl1_add_u32" | "s_lshl2_add_u32" | "s_lshl3_add_u32" | "s_lshl4_add_u32" => {
                        let n = m.as_bytes()[6] - b'0';
                        Some(format!("((({a} >> {}u) != 0u) || ((({a} << {n}u) + {b}) < ({a} << {n}u))) ? 1u : 0u", 32 - n as u32))
                    }
                    "s_lshl_b32" | "s_lshr_b32" | "s_ashr_i32" | "s_bfe_u32" | "s_bfe_i32" | "s_absdiff_i32" => Some("__NZ__".into()),
                    _ => None,
                };
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t} = {e};"));
                match scc {
                    Some(x) if x == "__NZ__" => self.line(&format!("scc = (t{t} != 0u) ? 1u : 0u;")),
                    Some(x) => self.line(&format!("scc = {x};")),
                    None => {}
                }
                self.ws(&d, &format!("t{t}"))
            }
            Enc::Sop1 => {
                let d = d.ok_or("no dst")?;
                let a = r(&s[0])?;
                match m {
                    "s_mov_b32" => self.ws(&d, &a),
                    "s_wqm_b32" => {
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t} = {a}; t{t} |= (t{t} >> 1) & 0x77777777u; t{t} |= (t{t} >> 2) & 0x33333333u; t{t} &= 0x11111111u; t{t} *= 0xfu;"));
                        if name(&d)? == "exec_lo" {
                            self.line(&format!("t{t} &= 1u;"));
                        }
                        self.ws(&d, &format!("t{t}"))?;
                        self.line(&format!("scc = (t{t} != 0u) ? 1u : 0u;"));
                        Ok(())
                    }
                    "s_wqm_b64" => {
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        let t = self.tmp;
                        self.tmp += 1;
                        for (k, x) in [("l", &a), ("h", &ah)] {
                            self.line(&format!("uint t{t}{k} = {x}; t{t}{k} |= (t{t}{k} >> 1) & 0x77777777u; t{t}{k} |= (t{t}{k} >> 2) & 0x33333333u; t{t}{k} &= 0x11111111u; t{t}{k} *= 0xfu;"));
                        }
                        if name(&d)? == "exec_lo" {
                            self.line(&format!("t{t}l &= 1u; t{t}h = 0u;"));
                        }
                        self.ws(&d, &format!("t{t}l"))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, &format!("t{t}h"))?;
                        self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                        Ok(())
                    }
                    "s_quadmask_b32" | "s_quadmask_b64" => {
                        let low = self.quadmask32(&a);
                        let result = if m == "s_quadmask_b64" {
                            let source_high = hi(&s[0]);
                            self.touch(&source_high);
                            let high = self.quadmask32(&r(&source_high)?);
                            format!("({low} | ({high} << 8))")
                        } else {
                            low
                        };
                        self.ws(&d, &result)?;
                        if m == "s_quadmask_b64" {
                            self.ws(&hi(&d), "0u")?;
                        }
                        self.line(&format!("scc = ({result} != 0u) ? 1u : 0u;"));
                        Ok(())
                    }
                    // D[S0[4:0]] = 0 / 1
                    "s_bitset0_b32" => {
                        self.touch(&d);
                        let cur = r(&d)?;
                        self.ws(&d, &format!("({cur} & ~(1u << (({a}) & 31u)))"))
                    }
                    "s_bitset1_b32" => {
                        self.touch(&d);
                        let cur = r(&d)?;
                        self.ws(&d, &format!("({cur} | (1u << (({a}) & 31u)))"))
                    }
                    "s_mov_b64" => {
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t}l = {a}; uint t{t}h = {ah};"));
                        self.ws(&d, &format!("t{t}l"))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, &format!("t{t}h"))
                    }
                    "s_not_b32" => {
                        self.ws(&d, &format!("(~{a})"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_cmov_b32" => self.ws(&d, &format!("((scc != 0u) ? {a} : {})", name(&d)?)),
                    "s_not_b64" | "s_cmov_b64" => {
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        let dh = hi(&d);
                        self.touch(&d);
                        self.touch(&dh);
                        let t = self.tmp;
                        self.tmp += 1;
                        if m == "s_not_b64" {
                            self.line(&format!("uint t{t}l = ~{a}; uint t{t}h = ~{ah};"));
                        } else {
                            self.line(&format!(
                                "uint t{t}l = (scc != 0u) ? {a} : {}; uint t{t}h = (scc != 0u) ? {ah} : {};",
                                name(&d)?,
                                name(&dh)?
                            ));
                        }
                        self.ws(&d, &format!("t{t}l"))?;
                        self.ws(&dh, &format!("t{t}h"))?;
                        if m == "s_not_b64" {
                            self.line(&format!("scc = ((t{t}l | t{t}h) != 0u) ? 1u : 0u;"));
                        }
                        Ok(())
                    }
                    "s_flbit_i32_b32" => self.ws(&d, &format!("(({a}) == 0u ? 0xffffffffu : (31u - firstbithigh({a})))")),
                    "s_flbit_i32" => self.ws(&d, &format!("((({a}) == 0u || ({a}) == 0xffffffffu) ? 0xffffffffu : (31u - (uint)firstbithigh(asint({a}))))")),
                    "s_ff0_i32_b32" => self.ws(&d, &format!("firstbitlow(~({a}))")),
                    "s_bcnt0_i32_b32" => {
                        self.ws(&d, &format!("(32u - countbits({a}))"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_bitreplicate_b64_b32" => {
                        let dh = hi(&d);
                        self.touch(&dh);
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint2 t{t} = uint2(({a}) & 0xffffu, ({a}) >> 16u);"));
                        for (shift, mask) in [(8, "00ff00ff"), (4, "0f0f0f0f"), (2, "33333333"), (1, "55555555")] {
                            self.line(&format!("t{t} = (t{t} | (t{t} << {shift}u)) & 0x{mask}u;"));
                        }
                        self.line(&format!("t{t} |= t{t} << 1u;"));
                        self.ws(&d, &format!("t{t}.x"))?;
                        self.ws(&dh, &format!("t{t}.y"))
                    }
                    "s_brev_b64" | "s_bitset0_b64" | "s_bitset1_b64" => {
                        let dh = hi(&d);
                        self.touch(&d);
                        self.touch(&dh);
                        let t = self.tmp;
                        self.tmp += 1;
                        if m == "s_brev_b64" {
                            self.touch(&hi(&s[0]));
                            let ah = r(&hi(&s[0]))?;
                            self.line(&format!("uint t{t}l = reversebits({ah}); uint t{t}h = reversebits({a});"));
                        } else {
                            let (dl, dhn) = (name(&d)?, name(&dh)?);
                            let (op, inv) = if m == "s_bitset1_b64" { ("|", "") } else { ("&", "~") };
                            self.line(&format!(
                                "uint t{t}b = ({a}) & 63u; uint t{t}l = {dl} {op} {inv}(t{t}b < 32u ? (1u << t{t}b) : 0u); uint t{t}h = {dhn} {op} {inv}(t{t}b >= 32u ? (1u << (t{t}b - 32u)) : 0u);"
                            ));
                        }
                        self.ws(&d, &format!("t{t}l"))?;
                        self.ws(&dh, &format!("t{t}h"))
                    }
                    "s_bcnt0_i32_b64" | "s_ff0_i32_b64" | "s_flbit_i32_b64" | "s_flbit_i32_i64" => {
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        let e = match m {
                            "s_bcnt0_i32_b64" => format!("(64u - countbits({a}) - countbits({ah}))"),
                            "s_ff0_i32_b64" => format!("((~({a})) != 0u ? firstbitlow(~({a})) : ((~({ah})) != 0u ? 32u + firstbitlow(~({ah})) : 0xffffffffu))"),
                            "s_flbit_i32_b64" => format!("(({ah}) != 0u ? (31u - firstbithigh({ah})) : (({a}) != 0u ? (63u - firstbithigh({a})) : 0xffffffffu))"),
                            _ => format!(
                                "((({ah}) == 0u && ({a}) == 0u) || (({ah}) == 0xffffffffu && ({a}) == 0xffffffffu)) ? 0xffffffffu : ((({ah}) != 0u && ({ah}) != 0xffffffffu) ? (31u - (uint)firstbithigh(asint({ah}))) : ((((({a}) ^ ({ah})) >> 31) != 0u) ? 32u : (63u - firstbithigh(({ah}) == 0u ? ({a}) : ~({a})))))"
                            ),
                        };
                        self.ws(&d, &e)?;
                        if m == "s_bcnt0_i32_b64" {
                            self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        }
                        Ok(())
                    }
                    "s_ff1_i32_b64" => {
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        self.ws(&d, &format!("(({a}) != 0u ? firstbitlow({a}) : (({ah}) != 0u ? 32u + firstbitlow({ah}) : 0xffffffffu))"))
                    }
                    "s_brev_b32" => self.ws(&d, &format!("reversebits({a})")),
                    "s_bcnt1_i32_b64" => {
                        // Population count of a 64-bit value — typically the number of active
                        // lanes (wave-level allocation). One invocation = one lane, so a lane
                        // mask counts as 0 or 1.
                        self.touch(&hi(&s[0]));
                        let ah = r(&hi(&s[0]))?;
                        self.ws(&d, &format!("(countbits({a}) + countbits({ah}))"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_bcnt1_i32_b32" => {
                        self.ws(&d, &format!("countbits({a})"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_ff1_i32_b32" => self.ws(&d, &format!("firstbitlow({a})")),
                    "s_sext_i32_i8" => self.ws(&d, &format!("asuint(asint({a} << 24) >> 24)")),
                    "s_sext_i32_i16" => self.ws(&d, &format!("asuint(asint({a} << 16) >> 16)")),
                    "s_abs_i32" => {
                        self.ws(&d, &format!("asuint(abs(asint({a})))"))?;
                        self.line(&format!("scc = ({} != 0u) ? 1u : 0u;", name(&d)?));
                        Ok(())
                    }
                    "s_getpc_b64" => {
                        // Address of the next instruction, relative to the program start.
                        self.ws(&d, &format!("{:#x}u", ins.offset as u32 + 4))?;
                        let dh = hi(&d);
                        self.touch(&dh);
                        self.ws(&dh, "0u")
                    }
                    "s_swappc_b64" => {
                        // Vertex-fetch shader call: inputs are materialised from the D3D input layout.
                        self.line("// (fetch-shader call: vertex attributes come from the input layout)");
                        Ok(())
                    }
                    _ if m.contains("wrexec") => {
                        let op = m.trim_start_matches("s_").split("_wrexec").next().unwrap_or("");
                        let e = match op {
                            "andn1" => format!("(~{a} & exec_lo)"),
                            "andn2" => format!("({a} & ~exec_lo)"),
                            _ => return Err("unsupported wrexec".into()),
                        };
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t} = {e} & 1u;"));
                        self.line(&format!("exec_lo = t{t}; scc = t{t};"));
                        self.ws(&d, &format!("t{t}"))?;
                        if m.ends_with("_b64") {
                            let dh = hi(&d);
                            self.touch(&dh);
                            self.ws(&dh, "0u")?;
                        }
                        Ok(())
                    }
                    _ if m.contains("saveexec") => {
                        let op = m.trim_start_matches("s_").split("_saveexec").next().unwrap_or("");
                        let e = match op {
                            "and" => format!("({a} & exec_lo)"),
                            "or" => format!("({a} | exec_lo)"),
                            "xor" => format!("({a} ^ exec_lo)"),
                            "andn2" => format!("({a} & ~exec_lo)"),
                            "orn2" => format!("({a} | ~exec_lo)"),
                            "andn1" => format!("(~{a} & exec_lo)"),
                            "orn1" => format!("(~{a} | exec_lo)"),
                            "nand" => format!("(~({a} & exec_lo))"),
                            "nor" => format!("(~({a} | exec_lo))"),
                            "xnor" => format!("(~({a} ^ exec_lo))"),
                            _ => return Err("unsupported saveexec".into()),
                        };
                        let t = self.tmp;
                        self.tmp += 1;
                        // Lane model: only bit 0 is meaningful.
                        self.line(&format!("uint t{t} = {e} & 1u;"));
                        self.ws(&d, "exec_lo")?;
                        if m.ends_with("_b64") {
                            let dh = hi(&d);
                            self.touch(&dh);
                            self.ws(&dh, "0u")?;
                        }
                        self.line(&format!("exec_lo = t{t}; scc = t{t};"));
                        Ok(())
                    }
                    // Return from a vertex shader chained in front of a geometry shader: the
                    // program ends here (it is followed directly by s_endpgm).
                    "s_setpc_b64" if idx + 2 >= self.insns.len() => {
                        self.line("// (return to the chained stage)");
                        Ok(())
                    }
                    _ => Err("unsupported SOP1".into()),
                }
            }
            Enc::Sopk => {
                let d = d.ok_or("no dst")?;
                let imm = format!("{:#x}u", ins.aux.imm as i32 as u32);
                match m {
                    "s_movk_i32" => self.ws(&d, &imm),
                    "s_version" => Ok(()),
                    "s_cmovk_i32" => {
                        self.touch(&d);
                        self.ws(&d, &format!("((scc != 0u) ? {imm} : {})", name(&d)?))
                    }
                    // Hardware mode-register writes (rounding / denormal control) and the
                    // counter waits have no HLSL-visible effect.
                    "s_setreg_imm32_b32" | "s_setreg_b32" | "s_getreg_b32" => {
                        let sim = ins.aux.imm as u32 & 0xffff;
                        let (id, off, size) = (sim & 63, (sim >> 6) & 31, ((sim >> 11) & 31) + 1);
                        let fmask = if size >= 32 { u32::MAX } else { (1u32 << size) - 1 };
                        if !matches!(id, 1..=7 | 15..=25 | 29) {
                            return Err(format!("{m}: hardware register {id} unsupported"));
                        }
                        self.hw_regs.insert(id);
                        if m == "s_getreg_b32" {
                            let src = match id {
                                2 => {
                                    self.uses_wave = true;
                                    let (ez, vz) = (self.guest_any("(exec_lo & 1u) != 0u"), self.guest_any("(vcc_lo & 1u) != 0u"));
                                    format!("(scc | (tb_hw2 & 0x18u) | ({ez} ? 0u : 0x200u) | ({vz} ? 0u : 0x400u) | 0x10000u)")
                                }
                                29 => {
                                    self.line("tb_hw29 = (tb_hw29 + 4u) & 0xfffffu;");
                                    "tb_hw29".to_string()
                                }
                                _ => format!("tb_hw{id}"),
                            };
                            return self.ws(&d, &format!("(({src} >> {off}u) & {fmask:#x}u)"));
                        }
                        let v = if m == "s_setreg_b32" {
                            self.touch(&d);
                            r(&d)?
                        } else {
                            format!("{:#x}u", ins.raw.get(1).copied().unwrap_or(0))
                        };
                        let mask = fmask.checked_shl(off).unwrap_or(0);
                        self.line(&format!("tb_hw{id} = (tb_hw{id} & {:#x}u) | (({v} << {off}u) & {mask:#x}u);", !mask));
                        Ok(())
                    }
                    "s_waitcnt_vscnt" | "s_waitcnt_vmcnt" | "s_waitcnt_expcnt" | "s_waitcnt_lgkmcnt" => Ok(()),
                    "s_addk_i32" => {
                        let t = self.tmp;
                        self.tmp += 1;
                        let dv = r(&d)?;
                        self.line(&format!("uint t{t} = {dv} + {imm}; scc = (((({dv} ^ t{t}) & ({imm} ^ t{t})) >> 31) != 0u) ? 1u : 0u;"));
                        self.ws(&d, &format!("t{t}"))
                    }
                    "s_mulk_i32" => self.ws(&d, &format!("({} * {imm})", r(&d)?)),
                    _ if m.starts_with("s_cmpk_") => {
                        let fake = m.replace("s_cmpk_", "s_cmp_");
                        let k = if m.ends_with("_u32") { ins.aux.imm as i32 as u32 & 0xffff } else { ins.aux.imm as i32 as u32 };
                        let c = Self::cmp_expr(&fake, &d, &Opnd::ConstInt(k as i32 as _))?;
                        self.line(&format!("scc = {c} ? 1u : 0u;"));
                        Ok(())
                    }
                    _ => Err("unsupported SOPK".into()),
                }
            }
            Enc::Sopc => {
                let c = match m {
                    "s_bitcmp0_b32" => format!("((({} >> ({} & 31u)) & 1u) == 0u)", r(&s[0])?, r(&s[1])?),
                    "s_bitcmp1_b32" => format!("((({} >> ({} & 31u)) & 1u) != 0u)", r(&s[0])?, r(&s[1])?),
                    "s_bitcmp0_b64" | "s_bitcmp1_b64" => {
                        self.touch(&hi(&s[0]));
                        let (lo, hi_r, b) = (r(&s[0])?, r(&hi(&s[0]))?, r(&s[1])?);
                        let bit = format!("((((({b}) & 63u) >= 32u ? ({hi_r} >> ((({b}) & 63u) - 32u)) : ({lo} >> (({b}) & 31u)))) & 1u)");
                        if m == "s_bitcmp1_b64" { format!("({bit} != 0u)") } else { format!("({bit} == 0u)") }
                    }
                    "s_cmp_eq_u64" => format!(
                        "({} == {} && {} == {})",
                        r(&s[0])?,
                        r(&s[1])?,
                        r(&hi(&s[0]))?,
                        r(&hi(&s[1]))?
                    ),
                    "s_cmp_lg_u64" => format!(
                        "({} != {} || {} != {})",
                        r(&s[0])?,
                        r(&s[1])?,
                        r(&hi(&s[0]))?,
                        r(&hi(&s[1]))?
                    ),
                    _ => Self::cmp_expr(m, &s[0], &s[1])?,
                };
                self.line(&format!("scc = {c} ? 1u : 0u;"));
                Ok(())
            }
            Enc::Smem if matches!(ins.op, 0x1f | 0x20 | 0x21 | 0x26 | 0x27) => Ok(()),
            Enc::Smem if matches!(ins.op, 0x24 | 0x25) => {
                let (lo, hi, step) = if ins.op == 0x24 { (1000u32, 1001u32, 16u32) } else { (1002, 1003, 1) };
                self.hw_regs.insert(lo);
                self.hw_regs.insert(hi);
                self.line(&format!("tb_hw{lo} += {step}u; if (tb_hw{lo} < {step}u) tb_hw{hi} += 1u;"));
                let base = sreg_num(&d.ok_or("no dst")?).ok_or("bad dst")?;
                for (k, src) in [(0u8, lo), (1, hi)] {
                    let dst = sreg_opnd(base + k);
                    self.touch(&dst);
                    self.ws(&dst, &format!("tb_hw{src}"))?;
                }
                Ok(())
            }
            Enc::Smem => {
                if ins.op <= 4 {
                    if let Some(Res::CodeTbl) = self.res.get(&idx).and_then(|v| v.first()) {
                        // Constant table embedded in the code section: byte address = pointer
                        // (relative to the start of the program) + immediate.
                        let n = 1u32 << ins.op;
                        let base = sreg_num(&d.ok_or("no dst")?).ok_or("bad dst")?;
                        let ndw = ((self.ex.isa_data.len().saturating_sub(self.ex.isa.len())) / 4).max(1);
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("uint t{t} = ({} + {:#x}u) >> 2;", r(&s[0])?, ins.aux.imm as u32));
                        for k in 0..n {
                            self.line(&format!("uint t{t}_{k} = tb_code[(t{t} + {k}u - {}u) % {ndw}u];", self.ex.isa.len() / 4));
                        }
                        for k in 0..n {
                            let dst = sreg_opnd(base + k as u8);
                            self.touch(&dst);
                            self.ws(&dst, &format!("t{t}_{k}"))?;
                        }
                        return Ok(());
                    }
                    if let Some(&(bn, soff)) = self.bl_loads.get(&idx) {
                        self.touch(&sreg_opnd(soff));
                        let so = r(&sreg_opnd(soff))?;
                        let imm = self.bl.get(bn as usize).map(|x| x.1).unwrap_or(0);
                        self.line(&format!("tb_bi{bn} = {so} + {imm:#x}u;"));
                        return Ok(());
                    }
                    if self.emit_rm_load(idx, ins)?.is_some() {
                        return Ok(());
                    }
                    self.line("// (descriptor fetch from root table — resolved statically)");
                    return Ok(());
                }
                if (0x10..=0x12).contains(&ins.op) {
                    return Err(format!(
                        "{} at {:#x}: stores through a 64-bit scalar address (SBASE), which is not bound to a resource (no GPU-VA-to-resource map)",
                        ins.mnemonic, ins.offset
                    ));
                }
                if (0x18..=0x1a).contains(&ins.op) {
                    let n = 1u32 << (ins.op - 0x18);
                    let d = ins.dst.first().and_then(sreg_num).ok_or("s_buffer_store: bad data")?;
                    let slot = match self.res.get(&idx).and_then(|v| v.first()).copied() {
                        Some(Res::Buf(slot, true)) => slot,
                        _ => return Err(format!("s_buffer_store at {:#x}: buffer did not resolve to a UAV", ins.offset)),
                    };
                    let soff = ins.raw.get(1).copied().unwrap_or(0) >> 25;
                    let imm = ins.aux.imm as u32 & !3;
                    let t = self.tmp;
                    self.tmp += 1;
                    if soff != 0x7d {
                        self.touch(&sreg_opnd(soff as u8));
                        self.line(&format!("uint t{t} = ({} & 0xfffffffcu) + {imm:#x}u;", ssrc_name(soff)?));
                    } else if ins.aux.imm < 0 {
                        return Err(format!("s_buffer_store at {:#x}: negative offset without SOFFSET", ins.offset));
                    } else {
                        self.line(&format!("uint t{t} = {imm:#x}u;"));
                    }
                    let nwords = self.sbuf_words((true, slot));
                    for k in 0..n {
                        let src = sreg_opnd(d + k as u8);
                        self.touch(&src);
                        let at = if k == 0 { format!("t{t}") } else { format!("(t{t} + {}u)", 4 * k) };
                        self.line(&format!("uint t{t}w{k} = {at} >> 2;"));
                        self.line(&format!("ubuf{slot}[t{t}w{k} / {nwords}u].w[t{t}w{k} % {nwords}u] = {};", r(&src)?));
                    }
                    return Ok(());
                }
                let n = 1u32 << (ins.op - 8);
                let base = sreg_num(&d.ok_or("no dst")?).ok_or("bad dst")?;
                let soff = ins.raw.get(1).copied().unwrap_or(0) >> 25;
                match soff {
                    0..=107 => self.touch(&sreg_opnd(soff as u8)),
                    124 => self.touch(&Opnd::M0),
                    _ => {}
                }
                let slot = match self.res.get(&idx).and_then(|v| v.first()).copied() {
                    Some(Res::Cb(s)) => s,
                    Some(Res::Buf(slot, is_uav)) => {
                        // Structured buffer read by byte offset (see `tb_sbw_*`).
                        let nm = if is_uav { "ubuf" } else { "sbuf" };
                        let bound = match ins.src.first() {
                            Some(Opnd::Sgpr(b)) => self.vs_bound(idx, *b),
                            _ => smem_bounds::Bound::Mem,
                        };
                        let imm4 = ins.aux.imm as u32 & !3;
                        let past = if ins.aux.imm < 0 { ">=" } else { "<" };
                        let t = self.tmp;
                        self.tmp += 1;
                        if soff != 0x7d {
                            self.line(&format!("uint t{t} = ({} & 0xfffffffcu) + {imm4:#x}u; uint t{t}c = (t{t} {past} {imm4:#x}u) ? 1u : 0u;", ssrc_name(soff)?));
                        } else {
                            self.line(&format!("uint t{t} = {imm4:#x}u;"));
                        }
                        for k in 0..n {
                            let read = format!("tb_sbw_{nm}{slot}(t{t} + {}u)", 4 * k);
                            let cond = if soff != 0x7d { self.smem_cond_dyn(&bound, &format!("t{t}"), &format!("t{t}c"), k, false, false) } else { self.smem_cond_static(&bound, imm4 as u64 + 4 * k as u64) };
                            let e = match cond {
                                smem_bounds::Cond::Always => read,
                                smem_bounds::Cond::Never => "0u".to_string(),
                                smem_bounds::Cond::Expr(c) => format!("({c} ? {read} : 0u)"),
                            };
                            self.line(&format!("uint t{t}_{k} = {e};"));
                        }
                        for k in 0..n {
                            let dst = sreg_opnd(base + k as u8);
                            self.touch(&dst);
                            self.ws(&dst, &format!("t{t}_{k}"))?;
                        }
                        return Ok(());
                    }
                    _ => return Err("unresolved constant buffer".into()),
                };
                let bound = match ins.src.first() {
                    Some(Opnd::Sgpr(b)) => self.vs_bound(idx, *b),
                    _ => smem_bounds::Bound::Mem,
                };
                self.note_cb_bound(slot, Some(&bound));
                let imm4 = ins.aux.imm as u32 & !3;
                let past = if ins.aux.imm < 0 { ">=" } else { "<" };
                if soff != 0x7d {
                    let so = ssrc_name(soff)?;
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint t{t} = ({so} & 0xfffffffcu) + {imm4:#x}u; uint t{t}c = (t{t} {past} {imm4:#x}u) ? 1u : 0u;"));
                    for k in 0..n {
                        let lo = if k == 0 { format!("t{t}") } else { format!("(t{t} + {:#x}u)", 4 * k) };
                        match self.smem_cond_dyn(&bound, &format!("t{t}"), &format!("t{t}c"), k, true, true) {
                            smem_bounds::Cond::Never => self.line(&format!("uint t{t}_{k} = 0u;")),
                            smem_bounds::Cond::Always => self.line(&format!("uint t{t}_{k} = cb{slot}[{lo} >> 4][({lo} >> 2) & 3u];")),
                            smem_bounds::Cond::Expr(c) => {
                                self.line(&format!("bool t{t}b{k} = {c};"));
                                self.line(&format!("uint t{t}_{k} = t{t}b{k} ? cb{slot}[(t{t}b{k} ? {lo} : 0u) >> 4][({lo} >> 2) & 3u] : 0u;"));
                            }
                        }
                    }
                    for k in 0..n {
                        let dst = sreg_opnd(base + k as u8);
                        self.touch(&dst);
                        self.ws(&dst, &format!("t{t}_{k}"))?;
                    }
                    return Ok(());
                }
                let w0 = imm4 / 4;
                let mut vals = Vec::with_capacity(n as usize);
                for k in 0..n {
                    let w = w0 + k;
                    let read = format!("cb{slot}[{}].{}", w / 4, XYZW[(w % 4) as usize]);
                    vals.push(match self.smem_cond_static(&bound, 4 * w as u64) {
                        smem_bounds::Cond::Always => read,
                        smem_bounds::Cond::Never => "0u".to_string(),
                        smem_bounds::Cond::Expr(c) => format!("({c} ? {read} : 0u)"),
                    });
                }
                let staged = vals.iter().any(|v| v.contains("tb_smem_in"));
                let t = self.tmp;
                if staged {
                    self.tmp += 1;
                    for (k, v) in vals.iter().enumerate() {
                        self.line(&format!("uint t{t}_{k} = {v};"));
                    }
                }
                for (k, v) in vals.iter().enumerate() {
                    let dst = sreg_opnd(base + k as u8);
                    self.touch(&dst);
                    self.ws(&dst, &if staged { format!("t{t}_{k}") } else { v.clone() })?;
                }
                Ok(())
            }
            Enc::Mimg => self.emit_mimg(idx, ins),
            Enc::Mubuf if ins.op == 0x71 || ins.op == 0x72 => {
                self.line("// (cache flush: no-op)");
                Ok(())
            }
            Enc::Mubuf => self.emit_mubuf(idx, ins),
            Enc::Mtbuf => self.emit_mtbuf(idx, ins),
            Enc::Flat if ins.aux.mask == 1 => self.emit_scratch(ins),
            Enc::Flat if global::routes(ins, &self.insns) => {
                self.uses_global = true;
                self.emit_global(ins)
            }
            Enc::Flat if self.amp_flat(ins) => self.emit_amp_flat(ins),
            Enc::Flat if m.starts_with("flat_load_dword") => {
                // Per-lane load through a 64-bit address. Shaders use this for lookups in
                // constant tables embedded in their own code section (address derived from
                // `s_getpc_b64`), so it is only accepted when the program takes its PC.
                if ins.aux.mask == 1 {
                    return Err("scratch-memory load unsupported".into());
                }
                if !self.insns.iter().any(|i| i.mnemonic == "s_getpc_b64") {
                    return Err("global/flat load in a shader that never takes its PC".into());
                }
                self.uses_code = true;
                let n: u8 = match m {
                    "flat_load_dword" => 1,
                    "flat_load_dwordx2" => 2,
                    "flat_load_dwordx3" => 3,
                    _ => 4,
                };
                let extra = match &s[2] {
                    Opnd::Null => String::new(),
                    o => format!(" + {}", r(o)?),
                };
                let ndw = ((self.ex.isa_data.len().saturating_sub(self.ex.isa.len())) / 4).max(1);
                let vd = match d {
                    Some(Opnd::Vgpr(v)) => v,
                    _ => return Err("bad vdst".into()),
                };
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t} = ({} + {:#x}u{extra}) >> 2;", r(&s[0])?, ins.aux.imm as i32 as u32));
                for k in 0..n {
                    self.line(&format!("uint t{t}_{k} = tb_code[(t{t} + {k}u - {}u) % {ndw}u];", self.ex.isa.len() / 4));
                }
                for k in 0..n {
                    self.wv(&Opnd::Vgpr(vd + k), &format!("t{t}_{k}"))?;
                }
                Ok(())
            }
            Enc::Ds if gds::is_gds(ins) => self.emit_gds(ins),
            Enc::Ds
                if !self.is_gs
                    && !self.is_cs
                    && self.ex.stage == Stage::Vertex
                    && matches!(m, "ds_write_b32" | "ds_write2_b32" | "ds_write_b64" | "ds_write2_b64") =>
            {
                // A vertex shader chained in front of a geometry shader hands its outputs over
                // through LDS instead of parameter exports: the vertex record is the output
                // registers back to back, 4 dwords each (observed in a55400ccab602283: dwords
                // 0..3 = TEXCOORD6.xyzw, 4..6 = TEXCOORD8.xyz, matching the output signature).
                // The address register is this vertex's record base and is not needed.
                self.vs_lds_out = true;
                let off = ins.aux.imm as u32;
                let vg = |o: &Opnd, k: u8| match o {
                    Opnd::Vgpr(n) => Ok(Opnd::Vgpr(n + k)),
                    _ => Err("LDS output write with a non-VGPR source".to_string()),
                };
                // (byte offset, source register) per written dword.
                let writes: Vec<(u32, Opnd)> = match m {
                    "ds_write_b32" => vec![(off & 0xffff, s[1])],
                    "ds_write2_b32" => vec![((off & 0xff) * 4, s[1]), (((off >> 8) & 0xff) * 4, s[2])],
                    "ds_write_b64" => vec![(off & 0xffff, s[1]), ((off & 0xffff) + 4, vg(&s[1], 1)?)],
                    _ => vec![
                        ((off & 0xff) * 8, s[1]),
                        ((off & 0xff) * 8 + 4, vg(&s[1], 1)?),
                        (((off >> 8) & 0xff) * 8, s[2]),
                        (((off >> 8) & 0xff) * 8 + 4, vg(&s[2], 1)?),
                    ],
                };
                let osig = self.ex.output_sig.clone();
                for (byte, src) in writes {
                    let dw = (byte / 4) as usize;
                    // The record is indexed by output *register* (4 dwords each), system
                    // values included: in 008bccdcf9dcecf9 register 1 is SV_Position and is
                    // written at dwords 4..7.
                    let reg = (dw / 4) as u32;
                    let comp = dw % 4;
                    self.touch(&src);
                    let mut hit = false;
                    for (ei, e) in osig.iter().enumerate() {
                        if e.register == reg && (e.mask >> comp) & 1 != 0 {
                            let idx = (0..comp).filter(|c| (e.mask >> c) & 1 != 0).count();
                            let (ty, n) = sig_type(e);
                            let field = if n == 1 { format!("o.o{ei}") } else { format!("o.o{ei}.{}", XYZW[idx]) };
                            let val = if ty.starts_with("float") { f(&src)? } else if ty.starts_with("int") { format!("asint({})", r(&src)?) } else { r(&src)? };
                            self.line(&format!("if (exec_lo & 1u) {field} = {val};"));
                            hit = true;
                        }
                    }
                    if !hit {
                        self.line(&format!("// (LDS output dword {dw} has no output-signature component: ignored)"));
                    }
                }
                Ok(())
            }
            Enc::Ds if (self.is_cs || self.is_gs) && ds_ext::is_wide_lds(m) => {
                for p in ds_ext::split_wide_lds(ins)? {
                    self.emit_insn(idx, &p, is_ps, None)?;
                }
                Ok(())
            }
            Enc::Ds if matches!(m, "ds_permute_b32" | "ds_bpermute_b32") => self.emit_ds_permute(ins),
            Enc::Ds if m == "ds_swizzle_b32" && self.is_cs => self.emit_ds_permute(ins),
            Enc::Ds if self.is_gs && matches!(m, "ds_read_b32" | "ds_read2_b32" | "ds_write_b32" | "ds_write2_b32") => {
                self.emit_gs_lds(ins)
            }
            Enc::Ds
                if self.is_cs
                    && matches!(
                        m,
                        "ds_read_b32" | "ds_read2_b32" | "ds_read2st64_b32" | "ds_write_b32" | "ds_write2_b32" | "ds_write2st64_b32"
                    ) =>
            {
                // Compute: LDS is the thread group's shared memory → an HLSL `groupshared`
                // array addressed by byte address / 4 (32 KiB, the D3D groupshared limit).
                self.uses_lds = true;
                let rd = m.starts_with("ds_read");
                self.lds_order(!rd, rd);
                let off = ins.aux.imm as u32;
                // Two-address forms scale their 8-bit offsets by 4 bytes ("2") or by
                // 64 dwords = 256 bytes ("2st64"); express both in units of 4 bytes.
                let scale = if m.contains("st64") { 64 } else { 1 };
                let (off0, off1) = ((off & 0xff) * scale, ((off >> 8) & 0xff) * scale);
                let addr = r(&s[0])?;
                let wrap = self.lds_wrap();
                let at = |o: u32| format!("tb_lds[(({addr} + {o:#x}u) >> 2) {wrap}]");
                let vd = match d {
                    Some(Opnd::Vgpr(v)) => v,
                    _ => return Err("bad vdst".into()),
                };
                let t = self.tmp;
                self.tmp += 1;
                match m {
                    "ds_read_b32" => {
                        self.line(&format!("uint t{t} = {};", at(off & 0xffff)));
                        self.wv(&Opnd::Vgpr(vd), &format!("t{t}"))
                    }
                    "ds_read2_b32" | "ds_read2st64_b32" => {
                        self.line(&format!("uint t{t}a = {}; uint t{t}b = {};", at(off0 * 4), at(off1 * 4)));
                        self.wv(&Opnd::Vgpr(vd), &format!("t{t}a"))?;
                        self.wv(&Opnd::Vgpr(vd + 1), &format!("t{t}b"))
                    }
                    "ds_write_b32" => {
                        self.line(&format!("if (exec_lo & 1u) {} = {};", at(off & 0xffff), r(&s[1])?));
                        Ok(())
                    }
                    _ => {
                        self.line(&format!(
                            "if (exec_lo & 1u) {{ {} = {}; {} = {}; }}",
                            at(off0 * 4),
                            r(&s[1])?,
                            at(off1 * 4),
                            r(&s[2])?
                        ));
                        Ok(())
                    }
                }
            }
            Enc::Ds
                if self.is_cs
                    && matches!(
                        m,
                        "ds_write_b16" | "ds_write_b16_d16_hi" | "ds_write_b8" | "ds_write_b8_d16_hi" | "ds_read_u16" | "ds_read_i16" | "ds_read_u8" | "ds_read_i8"
                            | "ds_read_u8_d16" | "ds_read_u8_d16_hi" | "ds_read_i8_d16" | "ds_read_i8_d16_hi" | "ds_read_u16_d16" | "ds_read_u16_d16_hi"
                    ) =>
            {
                // Sub-word accesses to group-shared memory: tb_lds is addressed in dwords, so
                // read-modify-write the containing dword.
                self.uses_lds = true;
                let rd = m.starts_with("ds_read");
                self.lds_order(!rd, rd);
                let off = ins.aux.imm as u32 & 0xffff;
                let bits = if m.contains("b16") || m.contains("u16") || m.contains("i16") { 16u32 } else { 8 };
                let mask = (1u32 << bits) - 1;
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t}a = ({}) + {off:#x}u; uint t{t}s = (t{t}a & 3u) * 8u;", r(&s[0])?));
                let word = format!("tb_lds[(t{t}a >> 2) {}]", self.lds_wrap());
                if m.starts_with("ds_write") {
                    let data = if m.ends_with("_d16_hi") { format!("(({}) >> 16u)", r(&s[1])?) } else { r(&s[1])? };
                    self.line(&format!(
                        "if (exec_lo & 1u) {{ uint t{t}o = {word}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 4096u; t{t}i++) {{ uint t{t}p; InterlockedCompareExchange({word}, t{t}o, (t{t}o & ~({mask:#x}u << t{t}s)) | (({data} & {mask:#x}u) << t{t}s), t{t}p); if (t{t}p == t{t}o) break; t{t}o = t{t}p; }} }}"
                    ));
                    return Ok(());
                }
                let v = format!("(({word} >> t{t}s) & {mask:#x}u)");
                let e = if m.contains("_i") { format!("asuint(asint({v} << {}) >> {})", 32 - bits, 32 - bits) } else { v };
                let dst = d.ok_or("no dst")?;
                let e = if m.ends_with("_d16_hi") {
                    self.touch(&dst);
                    format!("((({}) & 0xffffu) | (({e}) << 16u))", r(&dst)?)
                } else if m.ends_with("_d16") {
                    self.touch(&dst);
                    format!("((({}) & 0xffff0000u) | (({e}) & 0xffffu))", r(&dst)?)
                } else {
                    e
                };
                return self.wv(&dst, &e);
            }
            Enc::Ds if self.is_cs && matches!(m, "ds_wrxchg2_rtn_b32" | "ds_wrxchg2st64_rtn_b32") => self.emit_lds_wrxchg2(ins),
            Enc::Ds if self.is_cs && lds64::is_atomic64(m) => self.emit_lds_atomic64(ins),
            Enc::Ds
                if self.is_cs
                    && matches!(
                        m,
                        "ds_add_src2_u32"
                            | "ds_sub_src2_u32"
                            | "ds_rsub_src2_u32"
                            | "ds_inc_src2_u32"
                            | "ds_dec_src2_u32"
                            | "ds_min_src2_i32"
                            | "ds_max_src2_i32"
                            | "ds_min_src2_u32"
                            | "ds_max_src2_u32"
                            | "ds_and_src2_b32"
                            | "ds_or_src2_b32"
                            | "ds_xor_src2_b32"
                            | "ds_write_src2_b32"
                    ) =>
            {
                self.uses_lds = true;
                self.lds_order(true, true);
                let off = ins.aux.imm as u32 & 0xffff;
                let o0 = off & 0xff;
                let o1 = (off >> 8) & 0xff;
                let addr = r(&s[0])?;
                let t = self.tmp;
                self.tmp += 1;
                let wrap = self.lds_wrap();
                let word = format!("tb_lds[(({addr}) >> 2) {wrap}]");
                let x = format!("t{t}x");
                let body = match m {
                    "ds_add_src2_u32" => format!("InterlockedAdd({word}, {x}, t{t});"),
                    "ds_min_src2_u32" => format!("InterlockedMin({word}, {x}, t{t});"),
                    "ds_max_src2_u32" => format!("InterlockedMax({word}, {x}, t{t});"),
                    "ds_and_src2_b32" => format!("InterlockedAnd({word}, {x}, t{t});"),
                    "ds_or_src2_b32" => format!("InterlockedOr({word}, {x}, t{t});"),
                    "ds_xor_src2_b32" => format!("InterlockedXor({word}, {x}, t{t});"),
                    "ds_write_src2_b32" => format!("InterlockedExchange({word}, {x}, t{t});"),
                    op => {
                        let c = format!("t{t}c");
                        let new = match op {
                            "ds_sub_src2_u32" => format!("{c} - ({x})"),
                            "ds_rsub_src2_u32" => format!("({x}) - {c}"),
                            "ds_min_src2_i32" => format!("asuint(min(asint({c}), asint({x})))"),
                            "ds_max_src2_i32" => format!("asuint(max(asint({c}), asint({x})))"),
                            "ds_inc_src2_u32" => format!("(({c} >= ({x})) ? 0u : ({c} + 1u))"),
                            _ => format!("((({c} == 0u) || ({c} > ({x}))) ? ({x}) : ({c} - 1u))"),
                        };
                        format!(
                            "uint {c} = {word}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 4096u; t{t}i++) {{ uint t{t}p; InterlockedCompareExchange({word}, {c}, {new}, t{t}p); if (t{t}p == {c}) break; {c} = t{t}p; }} t{t} = {c};"
                        )
                    }
                };
                self.line(&format!(
                    "uint t{t} = 0u; if (exec_lo & 1u) {{ int t{t}o = (({o1:#x}u & 0x80u) != 0u) ? (int)((({addr}) >> 17u) << 17u) >> 17 : (int)((((({o1:#x}u & 0x7fu) << 8u) | {o0:#x}u) << 17u)) >> 17; uint {x} = tb_lds[((({addr}) + (uint)(t{t}o * 4)) >> 2) {wrap}]; {body} }}"
                ));
                Ok(())
            }
            Enc::Ds
                if self.is_cs
                    && matches!(
                        m.replace("_rtn", "").as_str(),
                        "ds_add_u32"
                            | "ds_sub_u32"
                            | "ds_rsub_u32"
                            | "ds_inc_u32"
                            | "ds_dec_u32"
                            | "ds_min_i32"
                            | "ds_max_i32"
                            | "ds_min_u32"
                            | "ds_max_u32"
                            | "ds_and_b32"
                            | "ds_or_b32"
                            | "ds_xor_b32"
                            | "ds_wrxchg_b32"
                            | "ds_cmpst_b32"
                            | "ds_mskor_b32"
                            | "ds_wrap_b32"
                    ) =>
            {
                // Atomic read-modify-write on group-shared memory (the `_rtn` forms also
                self.uses_lds = true;
                self.lds_order(true, true);
                let off = ins.aux.imm as u32 & 0xffff;
                let addr = r(&s[0])?;
                let x0 = r(&s[1])?;
                let t = self.tmp;
                self.tmp += 1;
                let word = format!("tb_lds[(({addr} + {off:#x}u) >> 2) {}]", self.lds_wrap());
                let body = match m.replace("_rtn", "").as_str() {
                    "ds_add_u32" => format!("InterlockedAdd({word}, {x0}, t{t});"),
                    "ds_min_u32" => format!("InterlockedMin({word}, {x0}, t{t});"),
                    "ds_max_u32" => format!("InterlockedMax({word}, {x0}, t{t});"),
                    "ds_and_b32" => format!("InterlockedAnd({word}, {x0}, t{t});"),
                    "ds_or_b32" => format!("InterlockedOr({word}, {x0}, t{t});"),
                    "ds_xor_b32" => format!("InterlockedXor({word}, {x0}, t{t});"),
                    "ds_wrxchg_b32" => format!("InterlockedExchange({word}, {x0}, t{t});"),
                    "ds_cmpst_b32" => format!("InterlockedCompareExchange({word}, {x0}, {}, t{t});", r(&s[2])?),
                    op => {
                        let c = format!("t{t}c");
                        let new = match op {
                            "ds_sub_u32" => format!("{c} - ({x0})"),
                            "ds_rsub_u32" => format!("({x0}) - {c}"),
                            "ds_min_i32" => format!("asuint(min(asint({c}), asint({x0})))"),
                            "ds_max_i32" => format!("asuint(max(asint({c}), asint({x0})))"),
                            "ds_inc_u32" => format!("(({c} >= ({x0})) ? 0u : ({c} + 1u))"),
                            "ds_mskor_b32" => format!("(({c} & ~({x0})) | ({}))", r(&s[2])?),
                            "ds_wrap_b32" => format!("(({c} >= ({x0})) ? ({c} - ({x0})) : ({c} + ({})))", r(&s[2])?),
                            _ => format!("((({c} == 0u) || ({c} > ({x0}))) ? ({x0}) : ({c} - 1u))"),
                        };
                        format!(
                            "uint {c} = {word}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 4096u; t{t}i++) {{ uint t{t}p; InterlockedCompareExchange({word}, {c}, {new}, t{t}p); if (t{t}p == {c}) break; {c} = t{t}p; }} t{t} = {c};"
                        )
                    }
                };
                self.line(&format!("uint t{t} = 0u; if (exec_lo & 1u) {{ {body} }}"));
                if m.contains("_rtn") {
                    self.wv(&d.ok_or("no dst")?, &format!("t{t}"))?;
                }
                Ok(())
            }
            Enc::Ds if m == "ds_swizzle_b32" => {
                // Quad swizzle (QDMode): each lane of a 2x2 pixel quad reads the lane selected
                // by a 2-bit field of the 8-bit mask — how RDNA2 pixel shaders fetch neighbour
                // values for derivatives. Per pixel: value at quad position (sx, sy) =
                // v + (sx - px) * ddx_fine(v) + (sy - py) * ddy_fine(v), with (px, py) this
                // pixel's position in its quad. Treats the register as float (derivative use).
                if !is_ps {
                    return Err("ds_swizzle outside a pixel shader".into());
                }
                let off = ins.aux.imm as u32;
                if off & 0x8000 == 0 {
                    return Err("ds_swizzle without QDMode unsupported".into());
                }
                self.uses_quad = true;
                let src = f(&s[0])?;
                let usrc = r(&s[0])?;
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t}s = ({:#x}u >> (tb_quad * 2u)) & 3u;", off & 0xff));
                self.line("#if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6");
                self.line(&format!(
                    "uint t{t}q0 = QuadReadLaneAt({usrc}, 0u), t{t}q1 = QuadReadLaneAt({usrc}, 1u), t{t}q2 = QuadReadLaneAt({usrc}, 2u), t{t}q3 = QuadReadLaneAt({usrc}, 3u);"
                ));
                self.line(&format!("uint t{t} = t{t}s == 0u ? t{t}q0 : t{t}s == 1u ? t{t}q1 : t{t}s == 2u ? t{t}q2 : t{t}q3;"));
                self.line("#else");
                self.line(&format!(
                    "float t{t}f = {src}; uint t{t} = asuint(t{t}f + ((float)(t{t}s & 1u) - (float)(tb_quad & 1u)) * ddx_fine(t{t}f) + ((float)(t{t}s >> 1) - (float)(tb_quad >> 1)) * ddy_fine(t{t}f));"
                ));
                self.line("#endif");
                self.wv(&d.ok_or("no dst")?, &format!("t{t}"))
            }
            Enc::Vinterp => self.emit_interp(ins, is_ps),
            Enc::Exp => self.emit_exp(ins, is_ps),
            _ => Err("unsupported encoding".into()),
        }
    }

    fn emit_interp(&mut self, ins: &Insn, is_ps: bool) -> Result<(), String> {
        if !is_ps {
            return Err("v_interp outside a pixel shader".into());
        }
        if ins.enc == Enc::Vop3 && matches!(ins.op, 0x342 | 0x343 | 0x35a) {
            let d1 = ins.raw.get(1).copied().unwrap_or(0);
            return Err(format!(
                "{} attr{}.{} {} half: the packing of 16-bit attributes into LDS dword halves is not established",
                ins.mnemonic,
                ins.aux.imm,
                XYZW[(ins.aux.imm2 & 3) as usize],
                if d1 & 0x100 != 0 { "high" } else { "low" }
            ));
        }
        if ins.enc == Enc::Vop3 {
            let w = ins.raw[0];
            let d1 = ins.raw.get(1).copied().unwrap_or(0);
            let mods = ((w >> 9) & 1) | ((w >> 11) & 0xf) | ((w >> 15) & 1) | ((d1 >> 27) & 3) | ((d1 >> 30) & 1);
            if mods != 0 {
                return Err(format!("{} VOP3 form with abs/neg/clamp/omod/opsel modifiers is not supported", ins.mnemonic));
            }
            if d1 & 0x100 != 0 {
                return Err(format!("{} VOP3 form with the f16 high bit is not supported", ins.mnemonic));
            }
        }
        let attr = ins.aux.imm as usize;
        let chan = ins.aux.imm2 as usize;
        let (reg, layer) = match self.ps_attrs.get(attr) {
            Some(r) => (*r, false),
            None => (
                *distinct_regs(self.ex.input_sig.iter().filter(|e| matches!(e.sysval, 4 | 5) && e.rw_mask != 0))
                    .get(attr - self.ps_attrs.len())
                    .ok_or_else(|| format!("PS attr{attr} has no signature register"))?,
                true,
            ),
        };
        let found = if layer {
            self.ex.input_sig.iter().enumerate().find_map(|(ei, e)| {
                (matches!(e.sysval, 4 | 5) && e.register == reg && (e.mask >> chan) & 1 != 0).then(|| (ei, (e.mask & ((1u8 << chan) - 1)).count_ones() as usize))
            })
        } else {
            sig_lookup(&self.ex.input_sig, reg, chan)
        };
        let val = if let Some((ei, k)) = found {
            let e = &self.ex.input_sig[ei];
            let field = format!("i.i{ei}");
            let component = XYZW[k];
            let acc = if ins.op == 2 && e.sysval == 0 && matches!(e.comp_type, 1 | 2) {
                let [Opnd::Vgpr(selector)] = ins.src.as_slice() else {
                    return Err("v_interp_mov_f32 missing parameter selector".into());
                };
                match selector & 3 {
                    vertex @ (0 | 1) => {
                        self.sm6_minor = self.sm6_minor.max(2);
                        let vertex = vertex + 1;
                        format!("GetAttributeAtVertex({field}, {vertex}).{component}")
                    }
                    2 => format!("{field}.{component}"),
                    _ => return Err("v_interp_mov_f32 reserved parameter selector 3".into()),
                }
            } else {
                format!("{field}.{component}")
            };
            if e.comp_type == 3 { format!("asuint({acc})") } else { format!("(uint)({acc})") }
        } else if chan == 3 {
            "0x3f800000u".to_string()
        } else {
            "0u".to_string()
        };
        let dst = ins.dst.first().ok_or("no dst")?;
        self.wv(dst, &val)
    }

    fn vcmp_cond(&mut self, ins: &Insn, base: &str) -> Result<String, String> {
        let s = &ins.src;
        let vop3 = ins.enc == Enc::Vop3;
        let mods = if vop3 { ins.aux.mask } else { 0 };
        let opsel = if vop3 { ((ins.raw[0] >> 11) & 3) as u8 } else { 0 };
        if base == "class_f32" {
            return Ok(format!("tb_cmp_class({}, {})", fcmp::class_src(r(&s[0])?, 0x8000_0000, mods)?, r(&s[1])?));
        }
        if base == "class_f16" {
            let x = match &s[0] {
                Opnd::ConstF32(_) if opsel & 1 == 0 => format!("tb_f2h({})", f(&s[0])?),
                o => half16(o, opsel & 1 != 0)?,
            };
            return Ok(format!("tb_cmp_class16({}, {})", fcmp::class_src(x, 0x8000, mods)?, r(&s[1])?));
        }
        if let Some(cc) = base.strip_suffix("_i64").or_else(|| base.strip_suffix("_u64")) {
            if mods != 0 {
                return Err(format!("v_cmp_{base}: VOP3 abs/neg on an integer compare"));
            }
            let signed = base.ends_with("_i64");
            for o in &s[..2] {
                self.touch(o);
                self.touch(&hi64(o));
            }
            return icmp::cond64(cc, signed, icmp::pair(&s[0], signed)?, icmp::pair(&s[1], signed)?);
        }
        Self::cmp_expr_mod(&format!("v_cmp_{base}"), &s[0], &s[1], mods, opsel)
    }

    fn guest_lane(&self, lane: &str) -> String {
        if self.wave64 {
            format!("(({lane}) & 31u)")
        } else {
            format!("((WaveGetLaneIndex() & ~31u) | (({lane}) & 31u))")
        }
    }

    fn guest_any(&mut self, cond: &str) -> String {
        if self.wave64 {
            return format!("WaveActiveAnyTrue({cond})");
        }
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint4 t{t}b = WaveActiveBallot({cond}); uint t{t}h = WaveGetLaneIndex() >> 5;"));
        format!("((t{t}h == 0u ? t{t}b.x : t{t}h == 1u ? t{t}b.y : t{t}h == 2u ? t{t}b.z : t{t}b.w) != 0u)")
    }

    fn emit_dpp(&mut self, ins: &Insn) -> Result<(), String> {
        let code = match ins.src.first() {
            Some(Opnd::Raw(c)) => *c,
            _ => return Err("DPP: no DPP operand".into()),
        };
        let ext = *ins.raw.get(1).ok_or("DPP: missing control dword")?;
        let m = ins.mnemonic.clone();
        let src = Opnd::Vgpr((ext & 0xff) as u8);
        self.touch(&src);
        self.uses_wave = true;
        let x = r(&src)?;
        let t = self.tmp;
        self.tmp += 1;
        let (l, rr, b) = (format!("t{t}l"), format!("t{t}r"), format!("t{t}b"));
        let (sl, inb, fi, bc, write, mods) = if code == 250 {
            let ctrl = (ext >> 8) & 0x1ff;
            let n = ctrl & 15;
            let (sl, inb) = match ctrl {
                0x000..=0x0ff => (format!("(({l} & ~3u) | (({ctrl:#x}u >> (({l} & 3u) * 2u)) & 3u))"), "true".to_string()),
                0x101..=0x10f => (format!("({b} + {rr} + {n}u)"), format!("({rr} + {n}u < 16u)")),
                0x111..=0x11f => (format!("({b} + {rr} - {n}u)"), format!("({rr} >= {n}u)")),
                0x121..=0x12f => (format!("({b} + (({rr} - {n}u) & 15u))"), "true".to_string()),
                0x140 => (format!("({b} + (15u - {rr}))"), "true".to_string()),
                0x141 => (format!("(({l} & ~7u) + (7u - ({l} & 7u)))"), "true".to_string()),
                0x150..=0x15f => (format!("({b} + {n}u)"), "true".to_string()),
                0x160..=0x16f => (format!("({b} + ({rr} ^ {n}u))"), "true".to_string()),
                c => return Err(format!("{m}: DPP control {c:#x} is not valid on RDNA2")),
            };
            let (row_mask, bank_mask) = ((ext >> 28) & 15, (ext >> 24) & 15);
            let row = if self.wave64 { format!("({l} >> 4)") } else { format!("(({l} >> 4) & 1u)") };
            let write = format!("((({row_mask:#x}u >> {row}) & 1u) != 0u && (({bank_mask:#x}u >> (({l} >> 2) & 3u)) & 1u) != 0u)");
            (sl, inb, (ext >> 18) & 1 != 0, (ext >> 19) & 1 != 0, write, (ext >> 20) & 15)
        } else {
            (format!("(({l} & ~7u) | (({:#x}u >> (({l} & 7u) * 3u)) & 7u))", ext >> 8), "true".to_string(), code == 234, true, "true".to_string(), 0)
        };
        self.line(&format!(
            "uint {l} = WaveGetLaneIndex(); uint {rr} = {l} & 15u; uint {b} = {l} & ~15u; bool t{t}o = {inb}; uint t{t}s = t{t}o ? ({sl}) : {l}; uint t{t}v = WaveReadLaneAt({x}, t{t}s); bool t{t}a = WaveReadLaneAt(exec_lo & 1u, t{t}s) != 0u; bool t{t}k = t{t}o && ({} || t{t}a); bool t{t}w = {write} && (t{t}k || {});",
            fi, bc
        ));
        let is_float = m.contains("_f32") || m.contains("_f16");
        if mods != 0 && (!is_float || m.starts_with("v_pk_") || m.contains("_f64")) {
            return Err(format!("{m}: DPP source modifiers on a non-float operation"));
        }
        let sign = if m.contains("_f16") { 0x8000u32 } else { 0x8000_0000 };
        let modify = |e: String, neg: bool, abs: bool| {
            let e = if abs { format!("({e} & {:#x}u)", !sign) } else { e };
            if neg { format!("({e} ^ {sign:#x}u)") } else { e }
        };
        let mut c = ins.clone();
        c.raw = vec![ins.raw[0]];
        let t0 = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint tb_s{t0} = {};", modify(format!("(t{t}k ? t{t}v : 0u)"), mods & 1 != 0, mods & 2 != 0)));
        c.src[0] = Opnd::Temp(t0 as u16);
        if mods & 12 != 0 {
            let s1 = c.src.get(1).cloned().ok_or_else(|| format!("{m}: DPP src1 modifier without src1"))?;
            let t1 = self.tmp;
            self.tmp += 1;
            self.line(&format!("uint tb_s{t1} = {};", modify(r(&s1)?, mods & 4 != 0, mods & 8 != 0)));
            c.src[1] = Opnd::Temp(t1 as u16);
        }
        self.line(&format!("uint t{t}e = exec_lo; exec_lo = t{t}w ? exec_lo : 0u;"));
        self.emit_vec(&c)?;
        if !m.starts_with("v_cmpx") {
            self.line(&format!("exec_lo = t{t}e;"));
        }
        Ok(())
    }

    fn emit_vec(&mut self, ins: &Insn) -> Result<(), String> {
        let mut m = ins.mnemonic.clone();
        if m == "v_nop" {
            return Ok(());
        }
        let s = &ins.src;
        let d = ins.dst.first().cloned().ok_or("no dst")?;
        if self.mode_own(ins)? {
            return Ok(());
        }
        if dpp_vgpr(ins).is_some() {
            return self.emit_dpp(ins);
        }
        if ins.op == OP_WAVE_REDUCE && ins.enc == Enc::Vop3 {
            // See `lower_wave_reductions`. Every invocation computes the same value; lanes
            if !self.wave64 {
                return Err(format!("{m}: whole-wave reduction in a wave32 program"));
            }
            self.uses_wave = true;
            let (f, kind) = m.strip_prefix("tb_wave_").and_then(wave_reduce_op).ok_or_else(|| format!("bad wave op {m}"))?;
            let cast = ["", "asfloat", "asint"][kind as usize];
            let e = format!(
                "asuint({f}((({} & 1u) != 0u) ? {cast}({}) : {cast}({:#x}u)))",
                r(&s[1])?,
                r(&s[0])?,
                ins.aux.imm as u32
            );
            return self.ws(&d, &e);
        }
        if ins.op == OP_WAVE_SCAN && ins.enc == Enc::Vop3 {
            if !self.wave64 {
                return Err(format!("{m}: whole-wave scan in a wave32 program"));
            }
            self.uses_wave = true;
            let (f, kind) = m.strip_prefix("tb_scan_").and_then(wave_scan_op).ok_or_else(|| format!("bad scan op {m}"))?;
            let x = r(&s[0])?;
            let t = self.tmp;
            self.tmp += 1;
            let e = match (kind, ins.aux.imm2 != 0) {
                (1, false) => format!("asuint({f}(asfloat({x})))"),
                (1, true) => format!("asuint({f}(asfloat({x})) {} asfloat({x}))", if f == "WavePrefixSum" { "+" } else { "*" }),
                (_, false) => format!("{f}({x})"),
                (_, true) => format!("({f}({x}) {} {x})", if f == "WavePrefixSum" { "+" } else { "*" }),
            };
            self.line(&format!("uint t{t} = {e};"));
            return self.wv(&d, &format!("t{t}"));
        }
        if ins.enc == Enc::Vop3 && matches!(m.as_str(), "v_readlane_b32" | "v_writelane_b32") {
            self.uses_wave = true;
            let lane = r(&s[1])?;
            let t = self.tmp;
            self.tmp += 1;
            if m == "v_readlane_b32" {
                let Some(sd) = readlane_dst(&d) else {
                    return Err(format!("v_readlane: destination {d:?} unsupported"));
                };
                if sd == Opnd::Null {
                    return Ok(());
                }
                self.line(&format!("uint t{t} = WaveReadLaneAt({}, {});", r(&s[0])?, self.guest_lane(&lane)));
                return self.ws(&sd, &format!("t{t}"));
            }
            self.touch(&d);
            let dn = name(&d)?;
            self.line(&format!("uint t{t} = {}; if (WaveGetLaneIndex() == {}) {dn} = t{t};", r(&s[0])?, self.guest_lane(&lane)));
            return Ok(());
        }
        if matches!(m.as_str(), "v_permlane16_b32" | "v_permlanex16_b32") {
            self.uses_wave = true;
            let (a, b, c) = (r(&s[0])?, r(&s[1])?, r(&s[2])?);
            self.touch(&d);
            let old = r(&d)?;
            let fi = (ins.raw[0] >> 11) & 1 != 0;
            let bc = (ins.raw[0] >> 12) & 1 != 0;
            let t = self.tmp;
            self.tmp += 1;
            let row = if m == "v_permlanex16_b32" { format!("((t{t}l & ~15u) ^ 16u)") } else { format!("(t{t}l & ~15u)") };
            self.line(&format!(
                "uint t{t}l = WaveGetLaneIndex(); uint t{t}q = t{t}l & 15u; uint t{t}s = {row} | (((t{t}q < 8u) ? ({b} >> (t{t}q * 4u)) : ({c} >> ((t{t}q - 8u) * 4u))) & 15u); uint t{t}v = WaveReadLaneAt({a}, t{t}s); bool t{t}a = WaveReadLaneAt(exec_lo & 1u, t{t}s) != 0u;"
            ));
            let e = if fi { format!("t{t}v") } else { format!("(t{t}a ? t{t}v : {})", if bc { "0u".to_string() } else { old }) };
            return self.wv(&d, &e);
        }
        if m.contains("_f64") || matches!(m.as_str(), "v_mad_u64_u32" | "v_mad_i64_i32") {
            return self.emit_f64(ins, &m.clone());
        }
        if let Some(sd) = ins.aux.sdwa {
            // SDWA: operate on a selected byte/word of each source. Materialise the selected
            // values into temporaries, then translate the instruction as usual.
            let is_cmp = ins.enc == Enc::Vopc;
            let dst_sel = if is_cmp { 6 } else { (sd >> 8) & 7 };
            if dst_sel == 7 {
                return Err("SDWA destination select 7 is reserved".into());
            }
            let fsign = match m.rsplit('_').next() {
                Some("f16") => Some(0x8000u32),
                Some("f32") => Some(0x8000_0000u32),
                _ => None,
            };
            if sd & 0x3030_0000 != 0 && fsign.is_none() {
                return Err("SDWA neg/abs modifiers on a non-float instruction".into());
            }
            let mut c = ins.clone();
            c.aux.sdwa = None;
            for (i, shift) in [(0usize, 16u32), (1usize, 24u32)] {
                if i >= c.src.len() {
                    continue;
                }
                let sel = (sd >> shift) & 7;
                let sext = (sd >> (shift + 3)) & 1 != 0;
                let neg = (sd >> (shift + 4)) & 1 != 0;
                let abs = (sd >> (shift + 5)) & 1 != 0;
                if sel == 6 && !neg && !abs {
                    continue;
                }
                let x = r(&c.src[i])?;
                let mut e = match sel {
                    0..=3 | 4 | 5 => {
                        let (bits, lsb) = if sel <= 3 { (8u32, sel * 8) } else { (16, (sel - 4) * 16) };
                        if sext {
                            format!("asuint(asint({x} << {}) >> {})", 32 - bits - lsb, 32 - bits)
                        } else {
                            format!("(({x} >> {lsb}) & {:#x}u)", (1u32 << bits) - 1)
                        }
                    }
                    6 => format!("({x})"),
                    _ => return Err(format!("SDWA select {sel} is reserved")),
                };
                if let Some(sb) = fsign {
                    if abs {
                        e = format!("({e} & {:#x}u)", !sb);
                    }
                    if neg {
                        e = format!("({e} ^ {sb:#x}u)");
                    }
                }
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint tb_s{t} = {e};"));
                c.src[i] = Opnd::Temp(t as u16);
            }
            if dst_sel == 6 {
                return self.emit_vec(&c);
            }
            // Partial destination: compute into a temporary, then place the selected
            // byte/word into the register. DST_UNUSED (bits 12:11): 0 = pad with zeros,
            // 1 = sign-extend upwards, 2 = keep the other bits of the destination.
            let (bits, lsb) = if dst_sel <= 3 { (8u32, dst_sel * 8) } else { (16, (dst_sel - 4) * 16) };
            let mask = (1u32 << bits) - 1;
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("uint tb_s{t} = 0u;"));
            c.dst = vec![Opnd::Temp(t as u16)];
            self.emit_vec(&c)?;
            let val = format!("(tb_s{t} & {mask:#x}u)");
            let e = match (sd >> 11) & 3 {
                0 => format!("({val} << {lsb})"),
                1 => format!("asuint(asint({val} << {}) >> {})", 32 - bits, 32 - bits - lsb),
                2 => {
                    self.touch(&d);
                    format!("((({}) & {:#x}u) | ({val} << {lsb}))", r(&d)?, !(mask << lsb))
                }
                _ => return Err("SDWA DST_UNUSED 3 is reserved".into()),
            };
            return self.wv(&d, &e);
        }
        // ---- half-float arithmetic: operands are f16 in the low 16 bits (inline float
        // constants denote the half-float of that value); the result is written as f16.
        // VOP3 forms carry per-source abs/neg modifiers and a result clamp/output modifier,
        // exactly as for f32 (see `fm` / `fres`).
        let h16 = |o: &Opnd| -> Result<String, String> {
            let hi_sel = ins.enc == Enc::Vop3 && s.iter().position(|x| std::ptr::eq(x, o)).map_or(false, |i| (ins.raw[0] >> (11 + i)) & 1 != 0);
            let mut e = match o {
                Opnd::ConstF32(x) => f16_inline_val(*x, hi_sel),
                _ if hi_sel => format!("f16tof32(({}) >> 16)", r(o)?),
                _ => format!("f16tof32(({}) & 0xffffu)", r(o)?),
            };
            if ins.enc == Enc::Vop3 {
                if let Some(i) = s.iter().position(|x| std::ptr::eq(x, o)) {
                    if (ins.aux.mask >> i) & 1 != 0 {
                        e = format!("abs({e})");
                    }
                    if (ins.aux.mask >> (3 + i)) & 1 != 0 {
                        e = format!("(-{e})");
                    }
                }
            }
            Ok(e)
        };
        let hres = |e: String| -> String {
            let mut e = e;
            if ins.enc == Enc::Vop3 {
                if ins.aux.imm != 0 {
                    e = format!("saturate({e})");
                }
            }
            format!("tb_f2h({e})")
        };
        if matches!(m.as_str(), "v_add_f16" | "v_sub_f16" | "v_subrev_f16" | "v_mul_f16" | "v_max_f16" | "v_min_f16") {
            let (a, b) = (h16(&s[0])?, h16(&s[1])?);
            let e = match m.as_str() {
                "v_add_f16" => format!("tb_hadd({a}, {b})"),
                "v_sub_f16" => format!("tb_hsub({a}, {b})"),
                "v_subrev_f16" => format!("tb_hsub({b}, {a})"),
                "v_mul_f16" => format!("tb_hmul({a}, {b})"),
                "v_max_f16" => format!("max({a}, {b})"),
                _ => format!("min({a}, {b})"),
            };
            return self.wv(&d, &hres(e));
        }
        if let Some(op) = crate::trans16::op_index(m.as_str()).filter(|_| !f16_native()) {
            self.uses_trans16 = true;
            let o = &s[0];
            let hi_sel = ins.enc == Enc::Vop3 && (ins.raw[0] >> 11) & 1 != 0;
            let mut x = match o {
                Opnd::ConstF32(_) => format!("tb_f2h({})", f(o)?),
                _ if hi_sel => format!("((({}) >> 16) & 0xffffu)", r(o)?),
                _ => format!("(({}) & 0xffffu)", r(o)?),
            };
            if ins.enc == Enc::Vop3 {
                if ins.aux.mask & 1 != 0 {
                    x = format!("({x} & 0x7fffu)");
                }
                if (ins.aux.mask >> 3) & 1 != 0 {
                    x = format!("({x} ^ 0x8000u)");
                }
            }
            let e = format!("tb_t16({op}u, {x})");
            if ins.enc == Enc::Vop3 && ins.aux.imm != 0 {
                return self.wv(&d, &hres(format!("f16tof32({e})")));
            }
            return self.wv(&d, &e);
        }
        if matches!(m.as_str(), "v_rcp_f16" | "v_sqrt_f16" | "v_rsq_f16" | "v_floor_f16" | "v_ceil_f16" | "v_trunc_f16" | "v_rndne_f16" | "v_fract_f16") {
            let a = h16(&s[0])?;
            let e = match m.as_str() {
                "v_rcp_f16" => format!("1.0 / {a}"),
                "v_sqrt_f16" => format!("sqrt({a})"),
                "v_rsq_f16" => format!("rsqrt({a})"),
                "v_floor_f16" => format!("floor({a})"),
                "v_ceil_f16" => format!("ceil({a})"),
                "v_trunc_f16" => format!("trunc({a})"),
                "v_rndne_f16" => format!("tb_rne({a})"),
                _ => format!("((tb_nan({a}) || isinf({a})) ? asfloat(0x7fc00000u) : min(frac({a}), 0.99951171875))"),
            };
            return self.wv(&d, &hres(e));
        }
        if m == "v_cvt_f16_u16" {
            return self.wv(&d, &format!("tb_f2h((float)(({}) & 0xffffu))", r(&s[0])?));
        }
        if m == "v_cvt_f16_i16" {
            return self.wv(&d, &format!("tb_f2h((float)(asint(({}) << 16) >> 16))", r(&s[0])?));
        }
        if m == "v_cvt_u16_f16" {
            let x = h16(&s[0])?;
            return self.wv(&d, &format!("(tb_nan({x}) ? 0u : (uint)clamp({x}, 0.0, 65535.0))"));
        }
        if m == "v_cvt_i16_f16" {
            let x = h16(&s[0])?;
            return self.wv(&d, &format!("(tb_nan({x}) ? 0u : (asuint((int)clamp({x}, -32768.0, 32767.0)) & 0xffffu))"));
        }
        if matches!(m.as_str(), "v_fma_f16" | "v_fmac_f16" | "v_fmamk_f16" | "v_fmaak_f16") {
            let k = format!("f16tof32({:#x}u)", ins.raw.last().copied().unwrap_or(0) & 0xffff);
            let (x, y, z, exact) = match m.as_str() {
                "v_fma_f16" => (h16(&s[0])?, h16(&s[1])?, h16(&s[2])?, fma16_operands_exact(&s[..2])),
                "v_fmac_f16" => {
                    self.touch(&d);
                    (h16(&s[0])?, h16(&s[1])?, h16(&d)?, fma16_operands_exact(&s[..2]))
                }
                "v_fmaak_f16" => (h16(&s[0])?, h16(&s[1])?, k, fma16_operands_exact(&s[..2])),
                _ => (h16(&s[0])?, k, h16(&s[1])?, fma16_operands_exact(&s[..1])),
            };
            let f = if exact { "tb_fma16" } else { "tb_fma_rto" };
            let k0 = ins.raw.last().copied().unwrap_or(1) & 0xffff == 0;
            let z0 = |i: usize| matches!(s.get(i), Some(Opnd::ConstInt(0) | Opnd::Literal(0))) && !(ins.enc == Enc::Vop3 && (ins.aux.mask >> (3 + i)) & 1 != 0);
            let pz = match m.as_str() {
                "v_fmaak_f16" => k0,
                "v_fma_f16" => z0(2),
                _ => false,
            };
            let e = format!("{f}({x}, {y}, {z})");
            let e = if pz { format!("tb_pz({e})") } else { e };
            return self.wv(&d, &hres(e));
        }
        if matches!(m.as_str(), "v_pk_fma_f16" | "v_pk_add_f16" | "v_pk_mul_f16" | "v_pk_min_f16" | "v_pk_max_f16") {
            // Packed half-float ops (VOP3P): two f16 lanes per register. For the low result,
            // OPSEL bit i (dword0[13:11]) picks the low/high half of source i and NEG bit i
            // (dword1[31:29]) negates it; for the high result OPSEL_HI (dword1[28:27],
            // dword0[14]) and NEG_HI (dword0[10:8]) do the same.
            let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
            let nsrc = if m == "v_pk_fma_f16" { 3 } else { 2 };
            let sel = [(w0 >> 11) & 7, ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2)];
            let neg = [(w1 >> 29) & 7, (w0 >> 8) & 7];
            let mut halves = Vec::new();
            for h in 0..2 {
                let mut v = Vec::new();
                for i in 0..nsrc {
                    let x = match &s[i] {
                        o @ Opnd::ConstF32(_) => format!("tb_f2h({})", f(o)?),
                        o => r(o)?,
                    };
                    let mut e = if (sel[h] >> i) & 1 != 0 { format!("f16tof32(({x}) >> 16)") } else { format!("f16tof32(({x}) & 0xffffu)") };
                    if (neg[h] >> i) & 1 != 0 {
                        e = format!("(-{e})");
                    }
                    v.push(e);
                }
                halves.push(match m.as_str() {
                    "v_pk_fma_f16" => format!("tb_fma16({}, {}, {})", v[0], v[1], v[2]),
                    "v_pk_add_f16" => format!("{} + {}", v[0], v[1]),
                    "v_pk_mul_f16" => format!("{} * {}", v[0], v[1]),
                    "v_pk_min_f16" => format!("min({}, {})", v[0], v[1]),
                    _ => format!("max({}, {})", v[0], v[1]),
                });
            }
            let halves: Vec<String> = if (w0 >> 15) & 1 != 0 { halves.into_iter().map(|h| format!("saturate({h})")).collect() } else { halves };
            return self.wv(&d, &format!("(tb_f2h({}) | (tb_f2h({}) << 16))", halves[0], halves[1]));
        }
        if let Some(e) = pk_int16(&m, ins)? {
            return self.wv(&d, &e);
        }
        if matches!(m.as_str(), "v_min3_f16" | "v_max3_f16" | "v_med3_f16") {
            let (a, b, c) = (h16(&s[0])?, h16(&s[1])?, h16(&s[2])?);
            let e = match m.as_str() {
                "v_min3_f16" => format!("min(min({a}, {b}), {c})"),
                "v_max3_f16" => format!("max(max({a}, {b}), {c})"),
                _ => format!("max(min({a}, {b}), min(max({a}, {b}), {c}))"),
            };
            return self.wv(&d, &hres(e));
        }
        if m == "v_clrexcp" {
            self.hw_regs.insert(3);
            self.line("tb_hw3 &= ~0x1ffu;");
            return Ok(());
        }
        if m == "v_pipeflush" {
            return Ok(());
        }
        if matches!(m.as_str(), "v_log_f16" | "v_exp_f16" | "v_sin_f16" | "v_cos_f16") {
            let a = h16(&s[0])?;
            let e = match m.as_str() {
                "v_log_f16" => format!("log2({a})"),
                "v_exp_f16" => format!("exp2({a})"),
                "v_sin_f16" => format!("sin({a} * 6.28318530718)"),
                _ => format!("cos({a} * 6.28318530718)"),
            };
            return self.wv(&d, &hres(e));
        }
        if m == "v_ldexp_f16" {
            let x = h16(&s[0])?;
            let e = format!("tb_hz(ldexp({x}, (float)clamp(asint(({}) << 16) >> 16, -64, 64)), asuint({x}) & 0x80000000u)", r(&s[1])?);
            return self.wv(&d, &hres(e));
        }
        if m == "v_pk_fmac_f16" {
            self.touch(&d);
            let (a, b, c) = (r(&s[0])?, r(&s[1])?, r(&d)?);
            let x = |v: &str, hi: bool| if hi { format!("f16tof32(({v}) >> 16)") } else { format!("f16tof32(({v}) & 0xffffu)") };
            let lo = format!("{} * {} + {}", x(&a, false), x(&b, false), x(&c, false));
            let hi_e = format!("{} * {} + {}", x(&a, true), x(&b, true), x(&c, true));
            return self.wv(&d, &format!("(tb_f2h({lo}) | (tb_f2h({hi_e}) << 16))"));
        }
        if matches!(m.as_str(), "v_frexp_exp_i32_f32" | "v_frexp_mant_f32") {
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!(
                "uint t{t} = asuint({}); uint t{t}e = (t{t} >> 23) & 0xffu; uint t{t}m = t{t} & 0x7fffffu; uint t{t}k = t{t}m == 0u ? 0u : (uint)firstbithigh(t{t}m);",
                self.fm(ins, 0)?
            ));
            let e = if m == "v_frexp_exp_i32_f32" {
                format!("(t{t}e == 0xffu || (t{t}e == 0u && t{t}m == 0u)) ? 0u : (t{t}e == 0u ? (t{t}k - 148u) : (t{t}e - 126u))")
            } else {
                format!(
                    "(t{t}e == 0xffu || (t{t}e == 0u && t{t}m == 0u)) ? t{t} : (t{t}e == 0u ? ((t{t} & 0x80000000u) | 0x3f000000u | ((t{t}m << (23u - t{t}k)) & 0x7fffffu)) : ((t{t} & 0x807fffffu) | 0x3f000000u))"
                )
            };
            return self.wv(&d, &e);
        }
        if matches!(m.as_str(), "v_frexp_exp_i16_f16" | "v_frexp_mant_f16") {
            let t = self.tmp;
            self.tmp += 1;
            let mut x = format!("(({}) & 0xffffu)", r(&s[0])?);
            if ins.enc == Enc::Vop3 {
                if ins.aux.mask & 1 != 0 {
                    x = format!("({x} & 0x7fffu)");
                }
                if (ins.aux.mask >> 3) & 1 != 0 {
                    x = format!("({x} ^ 0x8000u)");
                }
            }
            self.line(&format!(
                "uint t{t} = {x}; uint t{t}e = (t{t} >> 10) & 0x1fu; uint t{t}m = t{t} & 0x3ffu; uint t{t}k = t{t}m == 0u ? 0u : (uint)firstbithigh(t{t}m);"
            ));
            let e = if m == "v_frexp_exp_i16_f16" {
                format!("((t{t}e == 0x1fu || (t{t}e == 0u && t{t}m == 0u)) ? 0u : (t{t}e == 0u ? (t{t}k - 23u) : (t{t}e - 14u))) & 0xffffu")
            } else {
                format!(
                    "(t{t}e == 0x1fu || (t{t}e == 0u && t{t}m == 0u)) ? t{t} : (t{t}e == 0u ? ((t{t} & 0x8000u) | 0x3800u | ((t{t}m << (10u - t{t}k)) & 0x3ffu)) : ((t{t} & 0x83ffu) | 0x3800u))"
                )
            };
            return self.wv(&d, &e);
        }
        if m == "v_sat_pk_u8_i16" {
            let a = r(&s[0])?;
            return self.wv(
                &d,
                &format!("((uint)clamp(asint({a} << 16) >> 16, 0, 255) | ((uint)clamp(asint({a}) >> 16, 0, 255) << 8))"),
            );
        }
        if matches!(m.as_str(), "v_cvt_norm_i16_f16" | "v_cvt_norm_u16_f16") {
            let a = h16(&s[0])?;
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("float t{t} = {a};"));
            let e = if m == "v_cvt_norm_i16_f16" {
                format!("tb_nan(t{t}) ? 0u : (asuint(tb_nrm(clamp(t{t}, -1.0, 1.0), 32767u)) & 0xffffu)")
            } else {
                format!("tb_nan(t{t}) ? 0u : (uint)tb_nrm(saturate(t{t}), 65535u)")
            };
            return self.wv(&d, &e);
        }
        let is16 = matches!(
            m.as_str(),
            "v_add_nc_u16" | "v_sub_nc_u16" | "v_mul_lo_u16" | "v_lshrrev_b16" | "v_ashrrev_i16" | "v_lshlrev_b16"
                | "v_max_u16" | "v_max_i16" | "v_min_u16" | "v_min_i16" | "v_add_nc_i16" | "v_sub_nc_i16"
                | "v_mad_u16" | "v_mad_i16" | "v_min3_i16" | "v_min3_u16" | "v_max3_i16" | "v_max3_u16"
                | "v_med3_i16" | "v_med3_u16" | "v_mad_u32_u16" | "v_mad_i32_i16" | "v_pack_b32_f16"
                | "v_cvt_pknorm_i16_f16" | "v_cvt_pknorm_u16_f16"
        );
        if is16 {
            let opsel = if ins.enc == Enc::Vop3 { (ins.raw[0] >> 11) & 15 } else { 0 };
            let clamp = ins.enc == Enc::Vop3 && ins.aux.imm != 0;
            let n = s.len().min(3);
            let mut u = Vec::new();
            let mut i = Vec::new();
            for k in 0..n {
                let x = match &s[k] {
                    Opnd::ConstF32(x) if m.ends_with("_f16") => format!("{:#x}u", f16_inline(*x)),
                    o => r(o)?,
                };
                if (opsel >> k) & 1 != 0 {
                    u.push(format!("(({x}) >> 16)"));
                    i.push(format!("(asint({x}) >> 16)"));
                } else {
                    u.push(format!("(({x}) & 0xffffu)"));
                    i.push(format!("(asint(({x}) << 16) >> 16)"));
                }
            }
            let sat_i = |e: String| if clamp { sat_int(&e, 16) } else { format!("asuint({e})") };
            let e = match m.as_str() {
                "v_add_nc_u16" => if clamp { format!("min({} + {}, 0xffffu)", u[0], u[1]) } else { format!("({} + {})", u[0], u[1]) },
                "v_sub_nc_u16" => if clamp { format!("({0} < {1} ? 0u : {0} - {1})", u[0], u[1]) } else { format!("({} - {})", u[0], u[1]) },
                "v_mul_lo_u16" => format!("({} * {})", u[0], u[1]),
                "v_lshrrev_b16" => format!("({} >> ({} & 15u))", u[1], u[0]),
                "v_lshlrev_b16" => format!("({} << ({} & 15u))", u[1], u[0]),
                "v_ashrrev_i16" => format!("asuint({} >> ({} & 15u))", i[1], u[0]),
                "v_max_u16" => format!("max({}, {})", u[0], u[1]),
                "v_min_u16" => format!("min({}, {})", u[0], u[1]),
                "v_max_i16" => format!("asuint(max({}, {}))", i[0], i[1]),
                "v_min_i16" => format!("asuint(min({}, {}))", i[0], i[1]),
                "v_add_nc_i16" => sat_i(format!("({} + {})", i[0], i[1])),
                "v_sub_nc_i16" => sat_i(format!("({} - {})", i[0], i[1])),
                "v_mad_u16" => if clamp { format!("min({} * {} + {}, 0xffffu)", u[0], u[1], u[2]) } else { format!("({} * {} + {})", u[0], u[1], u[2]) },
                "v_mad_i16" => sat_i(format!("({} * {} + {})", i[0], i[1], i[2])),
                "v_min3_u16" => format!("min(min({}, {}), {})", u[0], u[1], u[2]),
                "v_max3_u16" => format!("max(max({}, {}), {})", u[0], u[1], u[2]),
                "v_med3_u16" => format!("max(min({0}, {1}), min(max({0}, {1}), {2}))", u[0], u[1], u[2]),
                "v_min3_i16" => format!("asuint(min(min({}, {}), {}))", i[0], i[1], i[2]),
                "v_max3_i16" => format!("asuint(max(max({}, {}), {}))", i[0], i[1], i[2]),
                "v_med3_i16" => format!("asuint(max(min({0}, {1}), min(max({0}, {1}), {2})))", i[0], i[1], i[2]),
                "v_mad_u32_u16" => return self.wv(&d, &format!("({} * {} + {})", u[0], u[1], r(&s[2])?)),
                "v_mad_i32_i16" => return self.wv(&d, &format!("asuint({} * {} + asint({}))", i[0], i[1], r(&s[2])?)),
                "v_pack_b32_f16" => {
                    let mut h = Vec::new();
                    for k in 0..2 {
                        let mut x = u[k].clone();
                        if ins.enc == Enc::Vop3 {
                            if (ins.aux.mask >> k) & 1 != 0 {
                                x = format!("({x} & 0x7fffu)");
                            }
                            if (ins.aux.mask >> (3 + k)) & 1 != 0 {
                                x = format!("({x} ^ 0x8000u)");
                            }
                        }
                        h.push(x);
                    }
                    return self.wv(&d, &format!("({} | ({} << 16))", h[0], h[1]));
                }
                _ => {
                    let mut h = Vec::new();
                    for k in 0..2 {
                        let t = self.tmp;
                        self.tmp += 1;
                        let mut x = format!("f16tof32({})", u[k]);
                        if ins.enc == Enc::Vop3 {
                            if (ins.aux.mask >> k) & 1 != 0 {
                                x = format!("abs({x})");
                            }
                            if (ins.aux.mask >> (3 + k)) & 1 != 0 {
                                x = format!("(-{x})");
                            }
                        }
                        self.line(&format!("float t{t} = {x};"));
                        h.push(if m == "v_cvt_pknorm_i16_f16" {
                            format!("(tb_nan(t{t}) ? 0u : (asuint(tb_nrm(clamp(t{t}, -1.0, 1.0), 32767u)) & 0xffffu))")
                        } else {
                            format!("(tb_nan(t{t}) ? 0u : (uint)tb_nrm(saturate(t{t}), 65535u))")
                        });
                    }
                    return self.wv(&d, &format!("({} | ({} << 16))", h[0], h[1]));
                }
            };
            return self.wv(&d, &format!("(({e}) & 0xffffu)"));
        }
        if matches!(m.as_str(), "v_qsad_pk_u16_u8" | "v_mqsad_pk_u16_u8" | "v_mqsad_u32_u8") {
            let d = ins.dst.first().cloned().ok_or("no dst")?;
            let vd = |k: u8| -> Result<Opnd, String> {
                match d {
                    Opnd::Vgpr(n) => Ok(Opnd::Vgpr(n + k)),
                    _ => Err(format!("{m}: wide destination is not a VGPR")),
                }
            };
            let (a0, a1, a2) = (s.get(0).ok_or("missing src0")?, s.get(1).ok_or("missing src1")?, s.get(2).ok_or("missing src2")?);
            if !matches!(a0, Opnd::Vgpr(_)) {
                return Err(format!("{m}: src0 must be a 64-bit VGPR pair"));
            }
            if !matches!(a2, Opnd::Vgpr(_)) {
                return Err(format!("{m}: src2 must be a VGPR group"));
            }
            let s2w: u8 = if m == "v_mqsad_u32_u8" { 4 } else { 2 };
            if s2w == 2 && ins.aux.imm != 0 {
                return Err(format!("{m}: CLAMP on a packed 16-bit multi-SAD is not lowered (the saturation width is not established)"));
            }
            let t = self.tmp;
            self.tmp += 3 + s2w as usize;
            self.line(&format!("uint t{t}a = {}; uint t{t}b = {}; uint t{t}q = {};", r(a0)?, r(&hi64(a0))?, r(a1)?));
            self.touch(&hi64(a0));
            for k in 0..s2w {
                let o = match a2 {
                    Opnd::Vgpr(n) => Opnd::Vgpr(n + k),
                    _ => unreachable!(),
                };
                if k != 0 {
                    self.touch(&o);
                }
                self.line(&format!("uint t{t}s{k} = {};", r(&o)?));
            }
            let byte = |i: usize| -> String {
                if i < 4 {
                    format!("((t{t}a >> {}u) & 0xffu)", 8 * i)
                } else {
                    format!("((t{t}b >> {}u) & 0xffu)", 8 * (i - 4))
                }
            };
            let masked = m != "v_qsad_pk_u16_u8";
            let sad = |off: usize| -> String {
                (0..4)
                    .map(|k| {
                        let (x, y) = (byte(off + k), format!("((t{t}q >> {}u) & 0xffu)", 8 * k));
                        if masked {
                            format!("(({y} == 0u) ? 0u : (max({x}, {y}) - min({x}, {y})))")
                        } else {
                            format!("(max({x}, {y}) - min({x}, {y}))")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" + ")
            };
            let mut fields = Vec::new();
            for k in 0..4u8 {
                let c = if s2w == 4 {
                    format!("t{t}s{k}")
                } else {
                    format!("((t{t}s{} >> {}u) & 0xffffu)", k / 2, 16 * (k % 2))
                };
                let sum = sad(k as usize);
                if ins.aux.imm != 0 {
                    let q = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint t{q} = ({sum}) + {c};"));
                    fields.push(format!("((t{q} < {c}) ? 0xffffffffu : t{q})"));
                } else {
                    fields.push(format!("(({sum}) + {c})"));
                }
            }
            if s2w == 4 {
                for k in 0..4u8 {
                    self.wv(&vd(k)?, &fields[k as usize])?;
                }
            } else {
                let lo = format!("(({} & 0xffffu) | (({}) << 16))", fields[0], fields[1]);
                let hi = format!("(({} & 0xffffu) | (({}) << 16))", fields[2], fields[3]);
                self.wv(&vd(0)?, &lo)?;
                self.wv(&vd(1)?, &hi)?;
            }
            return Ok(());
        }
        if matches!(m.as_str(), "v_sad_u8" | "v_sad_hi_u8" | "v_sad_u16" | "v_sad_u32" | "v_msad_u8") {
            let (a, b, c) = (r(&s[0])?, r(&s[1])?, r(&s[2])?);
            let ad = |x: String, y: String| format!("(max({x}, {y}) - min({x}, {y}))");
            let sum = match m.as_str() {
                "v_sad_u32" => ad(a.clone(), b.clone()),
                "v_sad_u16" => (0..2).map(|k| ad(format!("(({a} >> {}u) & 0xffffu)", 16 * k), format!("(({b} >> {}u) & 0xffffu)", 16 * k))).collect::<Vec<_>>().join(" + "),
                "v_msad_u8" => (0..4)
                    .map(|k| {
                        let (x, y) = (format!("(({a} >> {}u) & 0xffu)", 8 * k), format!("(({b} >> {}u) & 0xffu)", 8 * k));
                        format!("(({y} == 0u) ? 0u : {})", ad(x, y.clone()))
                    })
                    .collect::<Vec<_>>()
                    .join(" + "),
                _ => (0..4).map(|k| ad(format!("(({a} >> {}u) & 0xffu)", 8 * k), format!("(({b} >> {}u) & 0xffu)", 8 * k))).collect::<Vec<_>>().join(" + "),
            };
            let e = if m == "v_sad_hi_u8" { format!("((({sum}) << 16) + {c})") } else { format!("(({sum}) + {c})") };
            if ins.aux.imm != 0 {
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t} = {e};"));
                return self.wv(&d, &format!("((t{t} < {c}) ? 0xffffffffu : t{t})"));
            }
            return self.wv(&d, &e);
        }
        if m == "v_mullit_f32" {
            let (a, b, c) = (self.fm(ins, 0)?, self.fm(ins, 1)?, self.fm(ins, 2)?);
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("float t{t}a = {a}, t{t}b = {b}, t{t}c = {c};"));
            let e = format!("((asuint(t{t}b) == 0xff7fffffu || asuint(t{t}b) == 0xff800000u || tb_nan(t{t}b) || (asuint(t{t}c) & 0x7fffffffu) == 0u || (asuint(t{t}c) >> 31) != 0u || tb_nan(t{t}c)) ? -3.402823466e+38 : (float)((double)t{t}a * (double)t{t}b))");
            return self.wv(&d, &format!("asuint({})", self.fres(ins, e)));
        }
        if m == "v_cvt_pk_u8_f32" {
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("float t{t} = {}; uint t{t}s = (({}) & 3u) * 8u;", self.fm(ins, 0)?, r(&s[1])?));
            return self.wv(
                &d,
                &format!("(({}) & ~(0xffu << t{t}s)) | ((tb_nan(t{t}) ? 0u : (uint)clamp(tb_rne(t{t}), 0.0, 255.0)) << t{t}s)", r(&s[2])?),
            );
        }
        if matches!(m.as_str(), "v_cvt_pknorm_i16_f32" | "v_cvt_pknorm_u16_f32") {
            let mut h = Vec::new();
            for k in 0..2 {
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("float t{t} = {};", self.fm(ins, k)?));
                h.push(if m == "v_cvt_pknorm_i16_f32" {
                    format!("(tb_nan(t{t}) ? 0u : (asuint(tb_nrm(clamp(t{t}, -1.0, 1.0), 32767u)) & 0xffffu))")
                } else {
                    format!("(tb_nan(t{t}) ? 0u : (uint)tb_nrm(saturate(t{t}), 65535u))")
                });
            }
            return self.wv(&d, &format!("({} | ({} << 16))", h[0], h[1]));
        }
        if m == "v_cvt_pk_u16_u32" {
            return self.wv(&d, &format!("(min({}, 0xffffu) | (min({}, 0xffffu) << 16))", r(&s[0])?, r(&s[1])?));
        }
        if m == "v_cvt_pk_i16_i32" {
            return self.wv(
                &d,
                &format!(
                    "({} | (asuint(clamp(asint({}), -32768, 32767)) << 16))",
                    sat_int(&format!("asint({})", r(&s[0])?), 16),
                    r(&s[1])?
                ),
            );
        }
        if matches!(m.as_str(), "v_add_nc_i32" | "v_sub_nc_i32") {
            let (a, b) = (r(&s[0])?, r(&s[1])?);
            let op = if m == "v_add_nc_i32" { "+" } else { "-" };
            if ins.aux.imm == 0 {
                return self.wv(&d, &format!("({a} {op} {b})"));
            }
            let t = self.tmp;
            self.tmp += 1;
            let ovf = if m == "v_add_nc_i32" {
                format!("((({a} ^ t{t}) & ({b} ^ t{t})) >> 31)")
            } else {
                format!("((({a} ^ {b}) & ({a} ^ t{t})) >> 31)")
            };
            self.line(&format!("uint t{t} = {a} {op} {b};"));
            return self.wv(&d, &format!("({ovf} != 0u) ? ((({a}) >> 31) != 0u ? 0x80000000u : 0x7fffffffu) : t{t}"));
        }
        if m == "v_bcnt_u32_b32" {
            return self.wv(&d, &format!("(countbits({}) + {})", r(&s[0])?, r(&s[1])?));
        }
        if m == "v_bfm_b32" {
            return self.wv(&d, &format!("(((1u << (({}) & 31u)) - 1u) << (({}) & 31u))", r(&s[0])?, r(&s[1])?));
        }
        if m == "v_perm_b32" {
            return self.wv(&d, &format!("tb_perm({}, {}, {})", r(&s[0])?, r(&s[1])?, r(&s[2])?));
        }
        if m == "v_bfe_i32" {
            // Sign-extended bit field: offset = S1[4:0], width = S2[4:0].
            let (a, o, w) = (r(&s[0])?, r(&s[1])?, r(&s[2])?);
            return self.wv(&d, &format!("tb_bfe_i32({a}, (({o}) & 31u) | ((({w}) & 31u) << 16))"));
        }
        if matches!(m.as_str(), "v_movrelsd_2_b32" | "v_swaprel_b32") {
            let (Opnd::Vgpr(dn), Opnd::Vgpr(sn)) = (d, s[0]) else {
                return Err("register-relative move with a non-VGPR operand".into());
            };
            if !self.vreg_array {
                return Err("register-relative move outside VGPR-array mode".into());
            }
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("uint t{t}s = ({sn}u + (m0 & 0x3ffu)) & 255u; uint t{t}d = ({dn}u + ((m0 >> 16) & 0x3ffu)) & 255u; uint t{t}a = 0u; uint t{t}b = 0u;"));
            for (idx, val) in [("s", "a"), ("d", "b")] {
                self.line(&format!("switch (t{t}{idx}) {{"));
                for k in 0..256 {
                    self.line(&format!("case {k}u: t{t}{val} = v{k}; break;"));
                }
                self.line("}");
            }
            let writes: &[(&str, &str)] = if m == "v_swaprel_b32" { &[("d", "a"), ("s", "b")] } else { &[("d", "a")] };
            self.line("if (exec_lo & 1u) {");
            for (idx, val) in writes {
                self.line(&format!("switch (t{t}{idx}) {{"));
                for k in 0..256 {
                    self.line(&format!("case {k}u: v{k} = t{t}{val}; break;"));
                }
                self.line("}");
            }
            self.line("}");
            return Ok(());
        }
        if matches!(m.as_str(), "v_movrels_b32" | "v_movreld_b32" | "v_movrelsd_b32") {
            let (Opnd::Vgpr(dn), Opnd::Vgpr(sn)) = (d, s[0]) else {
                return Err("register-relative move with a non-VGPR operand".into());
            };
            if !self.vreg_array {
                return Err("register-relative move outside VGPR-array mode".into());
            }
            let t = self.tmp;
            self.tmp += 1;
            if m == "v_movreld_b32" {
                self.line(&format!("uint t{t} = v{sn};"));
            } else {
                self.line(&format!("uint t{t} = 0u;"));
                self.line(&format!("switch (({sn}u + m0) & 255u) {{"));
                for k in 0..256 {
                    self.line(&format!("case {k}u: t{t} = v{k}; break;"));
                }
                self.line("}");
            }
            if m == "v_movrels_b32" {
                self.line(&format!("if (exec_lo & 1u) v{dn} = t{t};"));
            } else {
                self.line("if (exec_lo & 1u) {");
                self.line(&format!("switch (({dn}u + m0) & 255u) {{"));
                for k in 0..256 {
                    self.line(&format!("case {k}u: v{k} = t{t}; break;"));
                }
                self.line("}");
                self.line("}");
            }
            return Ok(());
        }
        if matches!(m.as_str(), "v_fma_mix_f32" | "v_fma_mixlo_f16" | "v_fma_mixhi_f16") {
            // VOP3P: D = S0 * S1 + S2 where each source is either a full float or a half
            // float taken from the low/high 16 bits of its register and widened:
            //   OPSEL_HI bit i (dword1[28:27], dword0[14] for i = 2) = 0 -> 32-bit float,
            //                                                         1 -> 16-bit float;
            //   OPSEL bit i (dword0[13:11]) picks the high half when the source is 16-bit;
            //   NEG bit i (dword1[31:29]) negates, NEG_HI bit i (dword0[10:8]) takes |x|.
            let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
            let opsel = (w0 >> 11) & 7;
            let opsel_hi = ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2);
            let (neg, abs) = ((w1 >> 29) & 7, (w0 >> 8) & 7);
            let mut v = Vec::new();
            for i in 0..3 {
                let x = r(&s[i])?;
                let mut e = if (opsel_hi >> i) & 1 != 0 {
                    if (opsel >> i) & 1 != 0 { format!("f16tof32(({x}) >> 16)") } else { format!("f16tof32(({x}) & 0xffffu)") }
                } else {
                    format!("asfloat({x})")
                };
                if (abs >> i) & 1 != 0 {
                    e = format!("abs({e})");
                }
                if (neg >> i) & 1 != 0 {
                    e = format!("(-{e})");
                }
                v.push(e);
            }
            let e = format!("{}({}, {}, {})", if opsel_hi & 3 == 3 { "tb_fma16" } else { "tb_fma_rto" }, v[0], v[1], v[2]);
            return match m.as_str() {
                "v_fma_mix_f32" => self.wv(&d, &format!("asuint(tb_fma({}, {}, {}))", v[0], v[1], v[2])),
                // The f16 result replaces one half of the destination; the other half is kept.
                "v_fma_mixlo_f16" => {
                    self.touch(&d);
                    self.wv(&d, &format!("(({}) & 0xffff0000u) | tb_f2h({e})", r(&d)?))
                }
                _ => {
                    self.touch(&d);
                    self.wv(&d, &format!("(({}) & 0xffffu) | (tb_f2h({e}) << 16)", r(&d)?))
                }
            };
        }
        if let Some(base) = m.strip_prefix("v_cmpx_").or_else(|| m.strip_prefix("v_cmp_")).filter(|_| ins.aux.sdwa.is_none()) {
            let c = self.vcmp_cond(ins, base)?;
            if m.starts_with("v_cmpx_") {
                self.line(&format!("exec_lo = (((exec_lo & 1u) != 0u) && {c}) ? 1u : 0u;"));
                return Ok(());
            }
            self.ws(&d, &format!("(((exec_lo & 1u) != 0u) && {c}) ? 1u : 0u"))?;
            let dh = hi(&d);
            if !matches!(dh, Opnd::Null | Opnd::ConstInt(_)) {
                self.touch(&dh);
                self.ws(&dh, "0u")?;
            }
            return Ok(());
        }
        if matches!(
            m.as_str(),
            "v_add_co_u32" | "v_sub_co_u32" | "v_subrev_co_u32" | "v_add_co_ci_u32" | "v_sub_co_ci_u32" | "v_subrev_co_ci_u32"
        ) {
            // Add/subtract with carry. VOP3B: carry-out register in bits [14:8] of dword0 and
            // carry-in is src2; VOP2: both are VCC. Lane model: carry = this lane's bit.
            let vop3f = ins.enc == Enc::Vop3;
            let cout = if vop3f {
                match (ins.raw[0] >> 8) & 0x7f {
                    c @ 0..=105 => Opnd::Sgpr(c as u8),
                    106 => Opnd::VccLo,
                    125 => Opnd::Null,
                    c => return Err(format!("carry-out register code {c} unsupported")),
                }
            } else {
                Opnd::VccLo
            };
            let cin = if m.contains("_ci_") { if vop3f { r(&s[2])? } else { "vcc_lo".to_string() } } else { "0u".to_string() };
            let (mut a, mut b) = (r(&s[0])?, r(&s[1])?);
            if m.starts_with("v_subrev") {
                core::mem::swap(&mut a, &mut b);
            }
            let t = self.tmp;
            self.tmp += 1;
            if m.starts_with("v_add") {
                self.line(&format!(
                    "uint t{t}c = ({cin}) & 1u; uint t{t}a = {a} + {b}; uint t{t} = t{t}a + t{t}c; uint t{t}o = ((t{t}a < {a}) || (t{t} < t{t}a)) ? 1u : 0u;"
                ));
            } else {
                self.line(&format!(
                    "uint t{t}c = ({cin}) & 1u; uint t{t}a = {a} - {b}; uint t{t} = t{t}a - t{t}c; uint t{t}o = (({b} > {a}) || (t{t}c > t{t}a)) ? 1u : 0u;"
                ));
            }
            let sat = if vop3f && (ins.raw[0] >> 15) & 1 != 0 {
                if m.starts_with("v_add") { format!("(t{t}o != 0u ? 0xffffffffu : t{t})") } else { format!("(t{t}o != 0u ? 0u : t{t})") }
            } else {
                format!("t{t}")
            };
            self.wv(&d, &sat)?;
            if !matches!(cout, Opnd::Null) {
                self.touch(&cout);
                self.ws(&cout, &format!("((exec_lo & 1u) != 0u) ? t{t}o : 0u"))?;
                let ch = hi(&cout);
                self.touch(&ch);
                self.ws(&ch, "0u")?;
            }
            return Ok(());
        }
        if matches!(m.as_str(), "v_lshlrev_b64" | "v_lshrrev_b64" | "v_ashrrev_i64") {
            // 64-bit vector shifts: value and result are register pairs (lo, hi).
            let pair_hi = |o: &Opnd| match o {
                Opnd::Vgpr(n) => Opnd::Vgpr(n + 1),
                other => hi(other),
            };
            let n = format!("({} & 63u)", r(&s[0])?);
            let (lo, hi_e) = (r(&s[1])?, r(&pair_hi(&s[1]))?);
            let t = self.tmp;
            self.tmp += 1;
            let (rl, rh) = match m.as_str() {
                "v_lshlrev_b64" => (
                    format!("(({n} >= 32u) ? 0u : ({lo} << ({n} & 31u)))"),
                    format!("(({n} == 0u) ? {hi_e} : (({n} >= 32u) ? ({lo} << ({n} - 32u)) : (({hi_e} << ({n} & 31u)) | ({lo} >> (32u - ({n} & 31u))))))"),
                ),
                "v_lshrrev_b64" => (
                    format!("(({n} == 0u) ? {lo} : (({n} >= 32u) ? ({hi_e} >> ({n} - 32u)) : (({lo} >> ({n} & 31u)) | ({hi_e} << (32u - ({n} & 31u))))))"),
                    format!("(({n} >= 32u) ? 0u : ({hi_e} >> ({n} & 31u)))"),
                ),
                _ => (
                    format!("(({n} == 0u) ? {lo} : (({n} >= 32u) ? asuint(asint({hi_e}) >> ({n} - 32u)) : (({lo} >> ({n} & 31u)) | ({hi_e} << (32u - ({n} & 31u))))))"),
                    format!("(({n} >= 32u) ? asuint(asint({hi_e}) >> 31) : asuint(asint({hi_e}) >> ({n} & 31u)))"),
                ),
            };
            self.line(&format!("uint t{t}l = {rl}; uint t{t}h = {rh};"));
            self.wv(&d, &format!("t{t}l"))?;
            let dh = pair_hi(&d);
            self.touch(&dh);
            return self.wv(&dh, &format!("t{t}h"));
        }
        if m == "v_swap_b32" {
            let (x, y) = (name(&d)?, name(&s[0])?);
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("if (exec_lo & 1u) {{ uint t{t} = {x}; {x} = {y}; {y} = t{t}; }}"));
            return Ok(());
        }
        if m == "v_readfirstlane_b32" {
            // Lane model: the first active lane's value. Compilers only use this on values
            // that are uniform across the wave, so this lane's value is equivalent.
            if let Some(e) = self.mesh_readfirstlane(&s[0])? {
                return self.ws(&d, &e);
            }
            return self.ws(&d, &r(&s[0])?);
        }
        if let Some(op) = crate::trans32::op_index(m.as_str()).filter(|_| !f32_native()) {
            self.uses_trans32 = true;
            let mut x = format!("({})", r(&s[0])?);
            if ins.enc == Enc::Vop3 {
                if ins.aux.mask & 1 != 0 {
                    x = format!("({x} & 0x7fffffffu)");
                }
                if (ins.aux.mask >> 3) & 1 != 0 {
                    x = format!("({x} ^ 0x80000000u)");
                }
            }
            let e = format!("tb_t32_{op}({x})");
            if ins.enc == Enc::Vop3 && (ins.aux.imm2 != 0 || ins.aux.imm != 0) {
                let fe = self.fres(ins, format!("asfloat({e})"));
                return self.wv(&d, &fe);
            }
            return self.wv(&d, &e);
        }
        if matches!(m.as_str(), "v_div_scale_f32" | "v_div_fmas_f32" | "v_div_fixup_f32") {
            if self.mode_use.sp {
                return Err(format!("{m} under a shader-written single-precision MODE is not lowered"));
            }
            let neg_only = m == "v_div_scale_f32";
            let src = |i: usize| -> Result<String, String> {
                let mut e = f(&s[i])?;
                if ins.enc == Enc::Vop3 {
                    if !neg_only && (ins.aux.mask >> i) & 1 != 0 {
                        e = format!("abs({e})");
                    }
                    if (ins.aux.mask >> (3 + i)) & 1 != 0 {
                        e = format!("(-{e})");
                    }
                }
                Ok(e)
            };
            let (x, y, z) = (src(0)?, src(1)?, src(2)?);
            if m == "v_div_scale_f32" {
                let sdst = match (ins.raw[0] >> 8) & 0x7f {
                    c @ 0..=105 => Opnd::Sgpr(c as u8),
                    106 => Opnd::VccLo,
                    124 => Opnd::Null,
                    c => return Err(format!("carry-out register code {c} unsupported")),
                };
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint t{t}v; float t{t}q = tb_div_scale32({x}, {y}, {z}, t{t}v);"));
                let e = self.fres(ins, format!("t{t}q"));
                self.wv(&d, &e)?;
                if !matches!(sdst, Opnd::Null) {
                    self.touch(&sdst);
                    self.ws(&sdst, &format!("(((exec_lo & 1u) != 0u) ? t{t}v : 0u)"))?;
                }
                return Ok(());
            }
            let e = if m == "v_div_fmas_f32" {
                format!("tb_div_fmas32({x}, {y}, {z}, vcc_lo)")
            } else {
                format!("tb_div_fixup32({x}, {y}, {z})")
            };
            return self.wv(&d, &self.fres(ins, e));
        }
        let vop3 = ins.enc == Enc::Vop3;
        let k = format!("asfloat({:#x}u)", ins.raw.last().copied().unwrap_or(0));
        let dn = name(&d)?;
        let sp = self.mode_use.sp;
        let fw = |n: usize| self.fm(ins, n).map(|x| if sp { mode::din(x) } else { x });
        let fa = || fw(0);
        let fb = || fw(1);
        let fc = || fw(2);
        let (a, b) = (r(&s[0])?, s.get(1).map(r).transpose()?.unwrap_or_default());
        let c = s.get(2).map(|o| r(o)).transpose().unwrap_or(None).unwrap_or_default();
        // `v_mul_legacy_f32` / `v_mad_legacy_f32`: DX9 multiply rules — the product is 0 when
        // either operand is 0, even against infinity or NaN (`tb_mull`). A plain multiply
        // only matched under fast-math; with exact arithmetic it produced NaN positions.
        let legacy = m.ends_with("_legacy_f32");
        if m.ends_with("_legacy_f32") {
            m = m.replace("_legacy", "");
        }
        let mode_e = if sp && mode::arith(&m, legacy, &[], "", "").is_some() {
            let ops: Vec<String> = (0..s.len().min(3)).map(|n| fw(n)).collect::<Result<_, _>>()?;
            mode::arith(&m, legacy, &ops, &dn, &mode::din(k.clone()))
        } else if sp {
            mode::cvt(&m, &a)
        } else {
            None
        };
        let fexpr: Option<String> = if mode_e.is_some() { mode_e } else { match m.as_str() {
            "v_add_f32" => Some(format!("{} + {}", fa()?, fb()?)),
            "v_sub_f32" => Some(format!("tb_fsub({}, {})", fa()?, fb()?)),
            "v_subrev_f32" => Some(format!("tb_fsub({}, {})", fb()?, fa()?)),
            "v_mul_f32" if legacy => Some(format!("tb_mull({}, {})", fa()?, fb()?)),
            "v_mul_f32" => Some(format!("{} * {}", fa()?, fb()?)),
            "v_min_f32" => Some(format!("min({}, {})", fa()?, fb()?)),
            "v_max_f32" => Some(format!("max({}, {})", fa()?, fb()?)),
            "v_mac_f32" if legacy => Some(format!("tb_madlz({}, {}, asfloat({dn}))", fa()?, fb()?)),
            "v_mac_f32" => Some(format!("tb_madz({}, {}, asfloat({dn}))", fa()?, fb()?)),
            "v_ldexp_f32" => Some(format!(
                "tb_ldexp32({}, asint({b}))",
                fa()?
            )),
            "v_cvt_off_f32_i4" => Some(format!("((float)(asint({a} << 28) >> 28) * 0.0625)")),
            "v_madmk_f32" => Some(format!("tb_madz({}, {k}, {})", fa()?, fb()?)),
            "v_madak_f32" => Some(format!("tb_madz({}, {}, {k})", fa()?, fb()?)),
            "v_mad_f32" if legacy => Some(format!("tb_madlz({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_mad_f32" => Some(format!("tb_madz({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_fma_f32" => Some(format!("tb_fma({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_fmac_f32" => Some(format!("tb_fma({}, {}, asfloat({dn}))", fa()?, fb()?)),
            "v_fmamk_f32" => Some(format!("tb_fma({}, {k}, {})", fa()?, fb()?)),
            "v_fmaak_f32" => Some(format!("tb_fma({}, {}, {k})", fa()?, fb()?)),
            "v_min3_f32" => Some(format!("min(min({}, {}), {})", fa()?, fb()?, fc()?)),
            "v_max3_f32" => Some(format!("max(max({}, {}), {})", fa()?, fb()?, fc()?)),
            "v_med3_f32" => Some(format!(
                "((tb_nan({0}) || tb_nan({1}) || tb_nan({2})) ? min(min({0}, {1}), {2}) : max(min({0}, {1}), min(max({0}, {1}), {2})))",
                fa()?,
                fb()?,
                fc()?
            )),
            "v_cvt_f32_i32" => Some(format!("(float)asint({a})")),
            "v_cvt_f32_u32" => Some(format!("tb_u2f({a})")),
            "v_cvt_f32_ubyte0" => Some(format!("(float)({a} & 0xffu)")),
            "v_cvt_f32_ubyte1" => Some(format!("(float)(({a} >> 8) & 0xffu)")),
            "v_cvt_f32_ubyte2" => Some(format!("(float)(({a} >> 16) & 0xffu)")),
            "v_cvt_f32_ubyte3" => Some(format!("(float)(({a} >> 24) & 0xffu)")),
            "v_cvt_f32_f16" => Some(match &s[0] {
                Opnd::ConstF32(x) => f16_inline_val(*x, false),
                _ => format!("f16tof32({a})"),
            }),
            "v_fract_f32" => Some(format!("tb_fract({})", fa()?)),
            "v_trunc_f32" => Some(format!("tb_trunc({})", fa()?)),
            "v_ceil_f32" => Some(format!("tb_ceil({})", fa()?)),
            "v_rndne_f32" => Some(format!("tb_rne({})", fa()?)),
            "v_floor_f32" => Some(format!("tb_floor({})", fa()?)),
            "v_exp_f32" => Some(format!("exp2({})", fa()?)),
            "v_log_f32" => Some(format!("tb_log({})", fa()?)),
            "v_rcp_f32" | "v_rcp_iflag_f32" => Some(format!("1.0 / {}", fa()?)),
            "v_rsq_f32" => Some(format!("tb_qn(rsqrt({}))", fa()?)),
            "v_sqrt_f32" => Some(format!("tb_qn(sqrt({}))", fa()?)),
            "v_sin_f32" => Some(format!("sin(tb_rev({}) * 6.28318530718)", fa()?)),
            "v_cubema_f32" => Some(format!("tb_cubema({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_cubesc_f32" => Some(format!("tb_cubesc({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_cubetc_f32" => Some(format!("tb_cubetc({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_cubeid_f32" => Some(format!("tb_cubeid({}, {}, {})", fa()?, fb()?, fc()?)),
            "v_cos_f32" => Some(format!("cos(tb_rev({}) * 6.28318530718)", fa()?)),
            _ => None,
        } };
        if let Some(e) = fexpr {
            let pz_src = |i: usize| matches!(s.get(i), Some(Opnd::ConstInt(0) | Opnd::Literal(0))) && !(vop3 && (ins.aux.mask >> (3 + i)) & 1 != 0);
            let pz = match m.as_str() {
                "v_add_f32" => pz_src(0) || pz_src(1),
                "v_fma_f32" | "v_mad_f32" => pz_src(2),
                "v_fmaak_f32" | "v_madak_f32" => ins.raw.last() == Some(&0),
                _ => false,
            };
            let e = match (pz, m.as_str()) {
                (true, "v_fma_f32" | "v_fmaak_f32") => format!("tb_pzf({e}, {}, {})", fa()?, fb()?),
                (true, _) => format!("tb_pz({e})"),
                _ => e,
            };
            let e = if sp { mode::dout(e) } else { e };
            let e = self.fres(ins, e);
            let e = if m == "v_mad_f32" && vop3 && ins.aux.imm2 != 0 { format!("asuint(tb_ftz(asfloat({e})))") } else { e };
            return self.wv(&d, &e);
        }
        if vop3 && ins.aux.imm != 0 && matches!(m.as_str(), "v_mul_u32_u24" | "v_mul_i32_i24" | "v_mad_u32_u24" | "v_mad_i32_i24") {
            let t = self.tmp;
            self.tmp += 1;
            let (setup, e) = int24_clamped(t, &m, &a, &b, &c);
            self.line(&setup);
            return self.wv(&d, &e);
        }
        let mask = if vop3 { c.clone() } else { "vcc_lo".to_string() };
        let e = match m.as_str() {
            "v_mov_b32" => a,
            "v_cndmask_b32" => {
                let md = |x: &str, i: u8| {
                    let mut e = x.to_string();
                    if vop3 && (ins.aux.mask >> i) & 1 != 0 {
                        e = format!("(({e}) & 0x7fffffffu)");
                    }
                    if vop3 && (ins.aux.mask >> (3 + i)) & 1 != 0 {
                        e = format!("(({e}) ^ 0x80000000u)");
                    }
                    e
                };
                format!("(({mask} & 1u) != 0u) ? {} : {}", md(&b, 1), md(&a, 0))
            }
            "v_and_b32" => format!("({a} & {b})"),
            "v_or_b32" => format!("({a} | {b})"),
            "v_xor_b32" => format!("({a} ^ {b})"),
            "v_xnor_b32" => format!("(~({a} ^ {b}))"),
            "v_not_b32" => format!("(~{a})"),
            "v_bfrev_b32" => format!("reversebits({a})"),
            // Find-first-bit: -1 when no bit is set (HLSL intrinsics agree for zero input).
            "v_ffbl_b32" => format!("firstbitlow({a})"),
            "v_ffbh_u32" => format!("(({a}) == 0u ? 0xffffffffu : (31u - firstbithigh({a})))"),
            "v_lshrrev_b32" => format!("({b} >> ({a} & 31u))"),
            "v_lshlrev_b32" => format!("({b} << ({a} & 31u))"),
            "v_ashrrev_i32" => format!("asuint(asint({b}) >> ({a} & 31u))"),
            "v_add_nc_u32" if vop3 && ins.aux.imm != 0 => format!("(({a} + {b}) < {a} ? 0xffffffffu : ({a} + {b}))"),
            "v_sub_nc_u32" if vop3 && ins.aux.imm != 0 => format!("({b} > {a} ? 0u : ({a} - {b}))"),
            "v_subrev_nc_u32" if vop3 && ins.aux.imm != 0 => format!("({a} > {b} ? 0u : ({b} - {a}))"),
            "v_add_nc_u32" => format!("({a} + {b})"),
            "v_sub_nc_u32" => format!("({a} - {b})"),
            "v_subrev_nc_u32" => format!("({b} - {a})"),
            "v_min_u32" => format!("min({a}, {b})"),
            "v_max_u32" => format!("max({a}, {b})"),
            "v_min_i32" => format!("asuint(min(asint({a}), asint({b})))"),
            "v_max_i32" => format!("asuint(max(asint({a}), asint({b})))"),
            "v_mul_u32_u24" => format!("(({a} & 0xffffffu) * ({b} & 0xffffffu))"),
            "v_mul_i32_i24" => format!("asuint((asint({a} << 8) >> 8) * (asint({b} << 8) >> 8))"),
            "v_mul_lo_u32" | "v_mul_lo_i32" => format!("({a} * {b})"),
            // Count of mask bits below this lane, plus src1. One invocation = one lane with
            // nothing below it, so the count term is zero.
            "v_mbcnt_lo_u32_b32" | "v_mbcnt_hi_u32_b32" if self.is_ms() => self.mesh_mbcnt(ins, b)?,
            "v_mbcnt_lo_u32_b32" | "v_mbcnt_hi_u32_b32" => b,
            "v_mul_hi_u32" => format!("tb_mulhi_u32({a}, {b})"),
            "v_mul_hi_i32" => format!("tb_mulhi_i32({a}, {b})"),
            "v_mad_u32_u24" => format!("(({a} & 0xffffffu) * ({b} & 0xffffffu) + {c})"),
            "v_mad_i32_i24" => format!("asuint((asint({a} << 8) >> 8) * (asint({b} << 8) >> 8) + asint({c}))"),
            "v_bfe_u32" => format!("(({a} >> ({b} & 31u)) & ((1u << ({c} & 31u)) - 1u))"),
            "v_bfi_b32" => format!("(({a} & {b}) | (~{a} & {c}))"),
            "v_lshl_add_u32" => format!("(({a} << ({b} & 31u)) + {c})"),
            "v_add_lshl_u32" => format!("(({a} + {b}) << ({c} & 31u))"),
            "v_add3_u32" => format!("({a} + {b} + {c})"),
            "v_lshl_or_b32" => format!("(({a} << ({b} & 31u)) | {c})"),
            "v_and_or_b32" => format!("(({a} & {b}) | {c})"),
            "v_or3_b32" => format!("({a} | {b} | {c})"),
            "v_xor3_b32" => format!("({a} ^ {b} ^ {c})"),
            "v_xad_u32" => format!("(({a} ^ {b}) + {c})"),
            "v_min3_u32" => format!("min(min({a}, {b}), {c})"),
            "v_max3_u32" => format!("max(max({a}, {b}), {c})"),
            "v_med3_u32" => format!("max(min({a}, {b}), min(max({a}, {b}), {c}))"),
            "v_min3_i32" => format!("asuint(min(min(asint({a}), asint({b})), asint({c})))"),
            "v_max3_i32" => format!("asuint(max(max(asint({a}), asint({b})), asint({c})))"),
            "v_med3_i32" => format!(
                "asuint(max(min(asint({a}), asint({b})), min(max(asint({a}), asint({b})), asint({c}))))"
            ),
            // {S0:S1} >> S2[4:0], low 32 bits (S0 is the high word).
            "v_alignbit_b32" => format!(
                "((({c}) & 31u) == 0u ? {b} : (({b} >> (({c}) & 31u)) | ({a} << (32u - (({c}) & 31u)))))"
            ),
            "v_alignbyte_b32" => format!(
                "((({c}) & 3u) == 0u ? {b} : (({b} >> ((({c}) & 3u) * 8u)) | ({a} << (32u - (({c}) & 3u) * 8u))))"
            ),
            "v_mul_hi_u32_u24" => format!("tb_mulhi_u32({a} & 0xffffffu, {b} & 0xffffffu)"),
            "v_mul_hi_i32_i24" => format!("tb_mulhi_i32(asuint(asint({a} << 8) >> 8), asuint(asint({b} << 8) >> 8))"),
            "v_ffbh_i32" => format!("((({a}) == 0u || ({a}) == 0xffffffffu) ? 0xffffffffu : (31u - (uint)firstbithigh(asint({a}))))"),
            "v_lerp_u8" => (0..4)
                .map(|k| {
                    let sh = 8 * k;
                    format!("((((({a} >> {sh}u) & 0xffu) + (({b} >> {sh}u) & 0xffu) + (({c} >> {sh}u) & 1u)) >> 1) << {sh}u)")
                })
                .collect::<Vec<_>>()
                .join(" | "),
            "v_cvt_u32_f32" => format!("tb_cvt_u({})", fa()?),
            "v_cvt_i32_f32" => format!("tb_cvt_i({})", fa()?),
            "v_cvt_flr_i32_f32" => format!("tb_cvt_flr_i({})", fa()?),
            "v_cvt_rpi_i32_f32" => format!("tb_cvt_rpi_i({})", fa()?),
            "v_cvt_f16_f32" => format!("tb_f2h({})", fa()?),
            "v_cvt_pkrtz_f16_f32" => format!("(tb_f16rtz({}) | (tb_f16rtz({}) << 16))", fa()?, fb()?),
            _ => return Err("unsupported vector op".into()),
        };
        self.wv(&d, &e)
    }

    /// Number of dwords moved by a MUBUF dword load/store mnemonic.
    fn mubuf_dwords(m: &str) -> Option<u32> {
        Some(match m.rsplit('_').next()? {
            "dword" => 1,
            "dwordx2" => 2,
            "dwordx3" => 3,
            "dwordx4" => 4,
            _ => return None,
        })
    }

    /// MUBUF on a structured buffer: `vaddr` is the element index (IDXEN), the 12-bit
    /// immediate the byte offset inside the element. Dynamic offsets (OFFEN / SOFFSET
    /// register) are not handled yet and fail explicitly.
    fn emit_mubuf(&mut self, idx: usize, ins: &Insn) -> Result<(), String> {
        if (ins.raw.first().copied().unwrap_or(0) >> 16) & 1 != 0 {
            return Err("MUBUF LDS (load to LDS) unsupported".into());
        }
        if (ins.raw.get(1).copied().unwrap_or(0) >> 23) & 1 != 0 {
            return Err("MUBUF TFE status return unsupported".into());
        }
        if let Some(Res::CbDyn(slot)) = self.res.get(&idx).and_then(|v| v.first()).copied() {
            let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
            if (idxen && offen) || !matches!(ins.src[2], Opnd::ConstInt(0)) || !ins.mnemonic.starts_with("buffer_load_dword") {
                return Err("constant-buffer MUBUF form unsupported".into());
            }
            let n = Self::mubuf_dwords(&ins.mnemonic).ok_or("unsupported MUBUF op")?;
            let vd = match ins.dst[0] {
                Opnd::Vgpr(v) => v,
                _ => return Err("bad vdata".into()),
            };
            let base = if offen {
                format!("({:#x}u + {})", ins.aux.imm as u32, r(&ins.src[0])?)
            } else if idxen {
                // Indexed access to a constant buffer = a dynamically indexed cbuffer array.
                // Elements of cbuffer arrays are 16-byte registers, which is the stride the
                // CBV descriptor carries (inferred: the title reads 3 dwords per element).
                format!("({:#x}u + {} * 16u)", ins.aux.imm as u32, r(&ins.src[0])?)
            } else {
                format!("{:#x}u", ins.aux.imm as u32)
            };
            let bound = match (idxen, ins.src.get(1)) {
                (false, Some(Opnd::Sgpr(b))) => match self.vs_bound(idx, *b) {
                    b @ smem_bounds::Bound::Static { stride: Some(0), .. } => b,
                    smem_bounds::Bound::Static { nr, .. } => smem_bounds::Bound::Static { nr, stride: None },
                    _ => smem_bounds::Bound::Mem,
                },
                _ => smem_bounds::Bound::Mem,
            };
            self.note_cb_bound(slot, None);
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("uint t{t} = {base}; uint t{t}c = 0u;"));
            for k in 0..n {
                let lo = if k == 0 { format!("t{t}") } else { format!("(t{t} + {:#x}u)", 4 * k) };
                match self.smem_cond_dyn(&bound, &format!("t{t}"), &format!("t{t}c"), k, true, true) {
                    smem_bounds::Cond::Never => self.line(&format!("uint t{t}_{k} = 0u;")),
                    smem_bounds::Cond::Always => self.line(&format!("uint t{t}_{k} = cb{slot}[{lo} >> 4][({lo} >> 2) & 3u];")),
                    smem_bounds::Cond::Expr(c) => {
                        self.line(&format!("bool t{t}b{k} = {c};"));
                        self.line(&format!("uint t{t}_{k} = t{t}b{k} ? cb{slot}[(t{t}b{k} ? {lo} : 0u) >> 4][({lo} >> 2) & 3u] : 0u;"));
                    }
                }
            }
            for k in 0..n {
                self.wv(&Opnd::Vgpr(vd + k as u8), &format!("t{t}_{k}"))?;
            }
            return Ok(());
        }
        if ins.mnemonic.contains("_atomic_") {
            // Buffer atomics → HLSL Interlocked* on the UAV element. With GLC set the
            // pre-operation value is returned in vdata.
            let (offen, idxen, glc) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0, ins.aux.mask & 4 != 0);
            let soff = match ins.src[2] {
                Opnd::ConstInt(0) | Opnd::Null => None,
                Opnd::Sgpr(_) | Opnd::M0 | Opnd::ConstInt(_) | Opnd::Literal(_) => {
                    self.touch(&ins.src[2]);
                    Some(r(&ins.src[2])?)
                }
                other => return Err(format!("atomic MUBUF SOFFSET operand {other:?} is unsupported")),
            };
            // Without IDXEN the address is just the immediate offset: element 0 (the usual
            // wave-level counter allocation `InterlockedAdd(buf[0], n)`). With OFFEN alone the
            // address register is a byte offset.
            let index = if idxen { r(&ins.src[0])? } else { "0u".to_string() };
            let voff = match (idxen, offen) {
                (_, false) => None,
                (false, true) => Some(r(&ins.src[0])?),
                (true, true) => {
                    let a1 = match ins.src[0] {
                        Opnd::Vgpr(v) => Opnd::Vgpr(v + 1),
                        _ => return Err("atomic MUBUF IDXEN+OFFEN without a VGPR address pair".into()),
                    };
                    self.touch(&a1);
                    Some(r(&a1)?)
                }
            };
            let dyn_off = match (voff, soff) {
                (v, None) => v,
                (None, Some(s)) => Some(s),
                (Some(v), Some(s)) => Some(format!("({v}) + ({s})")),
            };
            let imm = ins.aux.imm as u32;
            let op_full = ins.mnemonic.strip_prefix("buffer_atomic_").ok_or_else(|| format!("{} unsupported", ins.mnemonic))?;
            let is_x2 = op_full.ends_with("_x2");
            let op = if is_x2 { &op_full[..op_full.len() - 3] } else { op_full };
            let res0 = self.res.get(&idx).and_then(|v| v.first()).copied();
            let raw = match res0 {
                Some(Res::Buf(slot, true)) if !is_x2 && !self.sbuf_u64.contains(&slot) && self.buf_is_raw(true, slot) => {
                    let byte = match dyn_off.as_deref() {
                        Some(d) => format!("({d}) + {imm:#x}u"),
                        None => format!("{imm:#x}u"),
                    };
                    Some(self.raw_buf(true, slot, idxen, &byte)?)
                }
                _ => None,
            };
            let target = if is_x2 {
                match (res0, dyn_off.as_deref()) {
                    (Some(Res::TBuf(slot, true)), None) if imm == 0 => format!("utb{slot}[{index}]"),
                    (Some(Res::TBuf(slot, true)), None) if imm % 8 == 0 => format!("utb{slot}[{index} + {}u]", imm / 8),
                    (Some(Res::TBuf(slot, true)), Some(d)) if !idxen => format!("utb{slot}[(({d}) + {imm:#x}u) >> 3]"),
                    (Some(Res::TBuf(slot, true)), Some(d)) => format!("utb{slot}[{index} + ((({d}) + {imm:#x}u) >> 3)]"),
                    (Some(Res::Buf(slot, true)), None) => format!("ubuf{slot}[{index} + {}u]", imm / 8),
                    (Some(Res::Buf(slot, true)), Some(d)) if !idxen => format!("ubuf{slot}[(({d}) + {imm:#x}u) >> 3]"),
                    (Some(Res::Buf(slot, true)), Some(d)) => format!("ubuf{slot}[{index} + ((({d}) + {imm:#x}u) >> 3)]"),
                    _ => return Err("unresolved atomic target".into()),
                }
            } else {
                match (res0, dyn_off.as_deref()) {
                    (Some(Res::TBuf(slot, true)), None) if imm == 0 => format!("utb{slot}[{index}]"),
                    (Some(Res::TBuf(slot, true)), None) if imm % 4 == 0 => format!("utb{slot}[{index} + {}u]", imm / 4),
                    (Some(Res::TBuf(slot, true)), Some(d)) if !idxen => format!("utb{slot}[(({d}) + {imm:#x}u) >> 2]"),
                    (Some(Res::TBuf(slot, true)), Some(d)) => format!("utb{slot}[{index} + ((({d}) + {imm:#x}u) >> 2)]"),
                    (Some(Res::Buf(slot, true)), None) => {
                        let (nwords, w) = (self.sbuf_words((true, slot)), imm / 4);
                        format!("ubuf{slot}[{index} + {}u].w[{}]", w / nwords, w % nwords)
                    }
                    (Some(Res::Buf(slot, true)), Some(d)) if !idxen => {
                        let nwords = self.sbuf_words((true, slot));
                        let w = format!("((({d}) + {imm:#x}u) >> 2)");
                        format!("ubuf{slot}[{w} / {nwords}u].w[{w} % {nwords}u]")
                    }
                    (Some(Res::Buf(slot, true)), Some(d)) => {
                        let nwords = self.sbuf_words((true, slot));
                        let w = format!("((({d}) + {imm:#x}u) >> 2)");
                        format!("ubuf{slot}[{index} + {w} / {nwords}u].w[{w} % {nwords}u]")
                    }
                    _ => return Err("unresolved atomic target".into()),
                }
            };
            let target = raw.unwrap_or(BufRef::Elem(target));
            let vdata = ins.dst[0];
            if is_x2 {
                self.sm6_minor = self.sm6_minor.max(6);
                let vd = match vdata {
                    Opnd::Vgpr(v) => v,
                    _ => return Err("bad vdata".into()),
                };
                let old = self.atomic_rmw64(op, &target.read(), vd).map_err(|_| format!("{} unsupported", ins.mnemonic))?;
                if glc {
                    self.wv(&Opnd::Vgpr(vd), &format!("(uint){old}"))?;
                    self.wv(&Opnd::Vgpr(vd + 1), &format!("(uint)({old} >> 32)"))?;
                }
            } else {
                let old = self.atomic_rmw(op, &target, vdata).map_err(|_| format!("{} unsupported", ins.mnemonic))?;
                if glc {
                    self.wv(&vdata, &old)?;
                }
            }
            return Ok(());
        }
        if let Some(Res::TBuf(slot, true)) = self.res.get(&idx).and_then(|v| v.first()).copied() {
            if self.tbuf_uint.contains(&slot) {
                if ins.mnemonic.contains("_d16_") {
                    return Err("d16 typed-buffer MUBUF on an integer slot is unsupported".into());
                }
                // uint-typed UAV (see atomics): single-component raw loads/stores.
                let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
                // A buffer used with atomics has a single 32-bit channel (D3D requires
                // R32_UINT/SINT for Interlocked*), so multi-component `_format_xy/xyz/xyzw`
                // accesses move only X; a load fills the missing channels with (0, 0, 1).
                let comps = match ins.mnemonic.rsplit('_').next() {
                    Some("x") => 1u8,
                    Some("xy") => 2,
                    Some("xyz") => 3,
                    Some("xyzw") => 4,
                    _ => 0,
                };
                if offen || ins.aux.imm != 0 || comps == 0 || !matches!(ins.src[2], Opnd::ConstInt(0) | Opnd::Null) {
                    return Err("uint typed-buffer MUBUF form unsupported".into());
                }
                // Without IDXEN the access addresses element 0.
                let index = if idxen { r(&ins.src[0])? } else { "0u".to_string() };
                let vd = ins.dst[0];
                if ins.mnemonic.starts_with("buffer_store") {
                    self.touch(&vd);
                    self.line(&format!("if (exec_lo & 1u) utb{slot}[{index}] = {};", r(&vd)?));
                } else {
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint t{t} = utb{slot}[{index}];"));
                    self.wv(&vd, &format!("t{t}"))?;
                    if let Opnd::Vgpr(v) = vd {
                        for k in 1..comps {
                            self.wv(&Opnd::Vgpr(v + k), if k == 3 { "1u" } else { "0u" })?;
                        }
                    }
                }
                return Ok(());
            }
        }
        if let Some(Res::TBuf(slot, is_uav)) = self.res.get(&idx).and_then(|v| v.first()).copied() {
            // Typed buffer. The element type is declared float4: the view format is not
            // visible in the shader, so integer-format buffers are a known gap (TECHNICAL_DEBT).
            let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
            if offen || ins.aux.imm != 0 || !matches!(ins.src[2], Opnd::ConstInt(0)) {
                return Err("typed-buffer MUBUF with offset addressing unsupported".into());
            }
            let n = match ins.mnemonic.rsplit('_').next() {
                Some("x") => 1u8,
                Some("xy") => 2,
                Some("xyz") => 3,
                Some("xyzw") => 4,
                _ => return Err("unsupported typed-buffer op".into()),
            };
            // Without IDXEN (and with no offset) the access addresses element 0.
            let index = if idxen { r(&ins.src[0])? } else { "0u".to_string() };
            let vd = match ins.dst[0] {
                Opnd::Vgpr(v) => v,
                _ => return Err("bad vdata".into()),
            };
            let nm = if is_uav { format!("utb{slot}") } else { format!("stb{slot}") };
            if ins.mnemonic.contains("_d16_") {
                if self.tbuf_ty.get(&(is_uav, slot)).is_some() {
                    return Err("d16 typed-buffer MUBUF on an integer slot is unsupported".into());
                }
                let hi = ins.mnemonic.ends_with("_hi_x");
                let t = self.tmp;
                self.tmp += 1;
                if ins.mnemonic.starts_with("buffer_store") {
                    if !is_uav {
                        return Err("buffer_store_format_d16 to a non-UAV table".into());
                    }
                    let mut c = Vec::new();
                    for k in 0..4u8 {
                        let e = if k < n {
                            let o = Opnd::Vgpr(vd + k / 2);
                            self.touch(&o);
                            let r = r(&o)?;
                            if hi || k % 2 == 1 {
                                format!("f16tof32(({r}) >> 16)")
                            } else {
                                format!("f16tof32(({r}) & 0xffffu)")
                            }
                        } else {
                            "0.0".into()
                        };
                        c.push(e);
                    }
                    let addr = format!("{nm}[{index}]");
                    self.line(&format!("float4 t{t} = float4({});", c.join(", ")));
                    self.line(&format!("if (exec_lo & 1u) {{ {addr} = t{t}; {} }}", small_float_store_fixup(&addr, &format!("t{t}"))));
                } else {
                    self.line(&format!("float4 t{t} = {nm}[{index}];"));
                    for ri in 0..n.div_ceil(2) {
                        let lo_k = ri * 2;
                        let lo = format!("f32tof16(t{t}.{})", XYZW[lo_k as usize]);
                        let o = Opnd::Vgpr(vd + ri);
                        let v = if hi {
                            self.touch(&o);
                            format!("((({}) & 0xffffu) | (({lo}) << 16))", r(&o)?)
                        } else if lo_k + 1 < n {
                            format!("({lo}) | (f32tof16(t{t}.{}) << 16)", XYZW[(lo_k + 1) as usize])
                        } else {
                            self.touch(&o);
                            format!("((({}) & 0xffff0000u) | (({lo}) & 0xffffu))", r(&o)?)
                        };
                        self.wv(&o, &v)?;
                    }
                }
                return Ok(());
            }
            if ins.mnemonic.starts_with("buffer_store") {
                if !is_uav {
                    return Err("buffer_store_format to a non-UAV table".into());
                }
                let mut c = vec![];
                for k in 0..4u8 {
                    if k < n {
                        let o = Opnd::Vgpr(vd + k);
                        self.touch(&o);
                        c.push(f(&o)?);
                    } else {
                        c.push("0.0".into());
                    }
                }
                match self.tbuf_ty.get(&(is_uav, slot)) {
                    Some(t) => {
                        let mut c = vec![];
                        for k in 0..4u8 {
                            c.push(if k < n { r(&Opnd::Vgpr(vd + k))? } else { "0u".into() });
                        }
                        let ctor = if *t == 2 { "int4" } else { "uint4" };
                        self.line(&format!("if (exec_lo & 1u) {nm}[{index}] = {ctor}({});", c.join(", ")));
                    }
                    None => {
                        let addr = format!("{nm}[{index}]");
                        let t = self.tmp;
                        self.tmp += 1;
                        self.line(&format!("float4 t{t} = float4({});", c.join(", ")));
                        self.line(&format!("if (exec_lo & 1u) {{ {addr} = t{t}; {} }}", small_float_store_fixup(&addr, &format!("t{t}"))));
                    }
                }
            } else {
                let t = self.tmp;
                self.tmp += 1;
                let ety = match self.tbuf_ty.get(&(is_uav, slot)) {
                    Some(2) => "int4",
                    Some(_) => "uint4",
                    None => "float4",
                };
                self.line(&format!("{ety} t{t} = {nm}[{index}];"));
                for k in 0..n {
                    let c = XYZW[k as usize];
                    let e = match ety {
                        "float4" => format!("asuint(t{t}.{c})"),
                        "int4" => format!("(uint)t{t}.{c}"),
                        _ => format!("t{t}.{c}"),
                    };
                    self.wv(&Opnd::Vgpr(vd + k), &e)?;
                }
            }
            return Ok(());
        }
        let (slot, is_uav) = match self.res.get(&idx).and_then(|v| v.first()) {
            Some(Res::Buf(s, u)) => (*s, *u),
            _ => return Err("unresolved buffer".into()),
        };
        // Address = element index (IDXEN: first address register) * stride + byte offset
        // (OFFEN: the next address register, or the first when there is no index) + imm.
        let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
        let soff = match ins.src[2] {
            Opnd::ConstInt(0) | Opnd::Null => None,
            Opnd::Sgpr(_) | Opnd::M0 | Opnd::ConstInt(_) | Opnd::Literal(_) => {
                self.touch(&ins.src[2]);
                Some(r(&ins.src[2])?)
            }
            other => return Err(format!("MUBUF SOFFSET operand {other:?} is unsupported")),
        };
        let sub: Option<(u32, bool, bool)> = match ins.mnemonic.as_str() {
            "buffer_load_ubyte" => Some((8, false, false)),
            "buffer_load_sbyte" => Some((8, true, false)),
            "buffer_load_ushort" => Some((16, false, false)),
            "buffer_load_sshort" => Some((16, true, false)),
            "buffer_store_byte" => Some((8, false, true)),
            "buffer_store_short" => Some((16, false, true)),
            "buffer_load_ubyte_d16" | "buffer_load_ubyte_d16_hi" => Some((8, false, false)),
            "buffer_load_sbyte_d16" | "buffer_load_sbyte_d16_hi" => Some((8, true, false)),
            "buffer_load_short_d16" | "buffer_load_short_d16_hi" => Some((16, false, false)),
            "buffer_store_byte_d16_hi" => Some((8, false, true)),
            "buffer_store_short_d16_hi" => Some((16, false, true)),
            _ => None,
        };
        let n = match (Self::mubuf_dwords(&ins.mnemonic), sub) {
            (Some(n), _) => n,
            (None, Some(_)) => 1,
            (None, None) => return Err("unsupported MUBUF op".into()),
        };
        let a0 = ins.src[0];
        let a1 = match a0 {
            Opnd::Vgpr(v) => Opnd::Vgpr(v + 1),
            other => other,
        };
        let index = if idxen { r(&a0)? } else { "0u".to_string() };
        let voff = match (idxen, offen) {
            (_, false) => None,
            (false, true) => Some(r(&a0)?),
            (true, true) => {
                self.touch(&a1);
                Some(r(&a1)?)
            }
        };
        let voff = match (voff, soff) {
            (v, None) => v,
            (None, Some(s)) => Some(s),
            (Some(v), Some(s)) => Some(format!("({v}) + ({s})")),
        };
        let w0 = ins.aux.imm as u32 / 4;
        let vd = match ins.dst[0] {
            Opnd::Vgpr(v) => v,
            _ => return Err("bad vdata".into()),
        };
        let nm = if is_uav { format!("ubuf{slot}") } else { format!("sbuf{slot}") };
        // Element `k` of this access. With a run-time byte offset the buffer is addressed
        // flat over the declared element size (which assumes, like every structured-buffer
        // access here, that the declared element matches the view's stride).
        let nwords = self.sbuf_words((is_uav, slot));
        if let Some((bits, signed, store)) = sub {
            let t = self.tmp;
            self.tmp += 1;
            let mask = (1u32 << bits) - 1;
            let byte = match &voff {
                Some(vo) => format!("(({vo}) + {:#x}u)", ins.aux.imm as u32),
                None => format!("{:#x}u", ins.aux.imm as u32),
            };
            self.line(&format!("uint t{t}b = {byte}; uint t{t}w = t{t}b >> 2; uint t{t}s = (t{t}b & 3u) * 8u;"));
            let elem = if self.buf_is_raw(is_uav, slot) {
                self.raw_buf(is_uav, slot, idxen, &format!("t{t}b"))?
            } else {
                BufRef::Elem(format!("{nm}[{index} + t{t}w / {nwords}u].w[t{t}w % {nwords}u]"))
            };
            let o = Opnd::Vgpr(vd);
            if store {
                if !is_uav {
                    return Err("buffer_store to a non-UAV table".into());
                }
                self.touch(&o);
                let data = if ins.mnemonic.ends_with("_d16_hi") { format!("(({}) >> 16u)", r(&o)?) } else { r(&o)? };
                self.line(&format!(
                    "if (exec_lo & 1u) {{ {} {} }}",
                    elem.call("InterlockedAnd", &format!("~({mask:#x}u << t{t}s)")),
                    elem.call("InterlockedOr", &format!("({data} & {mask:#x}u) << t{t}s"))
                ));
                return Ok(());
            }
            let v = format!("(({} >> t{t}s) & {mask:#x}u)", elem.read());
            let e = if signed { format!("asuint(asint({v} << {}) >> {})", 32 - bits, 32 - bits) } else { v };
            let e = if ins.mnemonic.ends_with("_d16_hi") {
                self.touch(&o);
                format!("((({}) & 0xffffu) | (({e}) << 16u))", r(&o)?)
            } else if ins.mnemonic.ends_with("_d16") {
                self.touch(&o);
                format!("((({}) & 0xffff0000u) | (({e}) & 0xffffu))", r(&o)?)
            } else {
                e
            };
            return self.wv(&o, &e);
        }
        let t = self.tmp;
        self.tmp += 1;
        if let Some(vo) = &voff {
            self.line(&format!("uint t{t}w = (({vo}) + {:#x}u) >> 2;", ins.aux.imm as u32));
        }
        let raw = self.buf_is_raw(is_uav, slot);
        if raw {
            self.raw_buf(is_uav, slot, idxen, "0u")?;
        }
        let elem = |k: u32| match &voff {
            None if raw => BufRef::Raw(nm.clone(), format!("{:#x}u", (w0 + k) * 4)),
            Some(_) if raw => BufRef::Raw(nm.clone(), format!("((t{t}w + {k}u) << 2)")),
            // A constant offset past the element addresses the following elements.
            None if w0 + k >= nwords => BufRef::Elem(format!("{nm}[{index} + {}u].w[{}]", (w0 + k) / nwords, (w0 + k) % nwords)),
            None => BufRef::Elem(format!("{nm}[{index}].w[{}]", w0 + k)),
            Some(_) => BufRef::Elem(format!("{nm}[{index} + (t{t}w + {k}u) / {nwords}u].w[(t{t}w + {k}u) % {nwords}u]")),
        };
        if ins.mnemonic.starts_with("buffer_store") {
            if !is_uav {
                return Err("buffer_store to a non-UAV table".into());
            }
            for k in 0..n {
                let o = Opnd::Vgpr(vd + k as u8);
                self.touch(&o);
                self.line(&format!("if (exec_lo & 1u) {}", elem(k).store(&r(&o)?)));
            }
        } else {
            for k in 0..n {
                self.line(&format!("uint t{t}_{k} = {};", elem(k).read()));
            }
            for k in 0..n {
                self.wv(&Opnd::Vgpr(vd + k as u8), &format!("t{t}_{k}"))?;
            }
        }
        Ok(())
    }

    /// Geometry-shader LDS layout (read off WriteToSliceMainGS): input vertex data lives at
    /// `vertexBase + register*16 + component*4`, emitted vertices at
    /// `primBase + vertex*stride + register*16 + component*4`, stride = 16 * output registers.
    /// The translation seeds the vertex-offset registers with synthetic offsets (vertex k at
    /// dword offset k*0x400, i.e. byte k*0x1000 after the shader's own <<2), so an input
    /// address decodes exactly as (vertex = A >> 12, slot = (A & 0xfff) >> 2).
    fn gs_dims(&self) -> Result<(usize, usize, usize, &'static str, &'static str), String> {
        let gi = self.ex.gs_info.ok_or("geometry shader without PSV0 GS info")?;
        let (prim, nv) = match gi.input_primitive {
            1 => ("point", 1usize),
            2 => ("line", 2),
            3 => ("triangle", 3),
            6 => ("lineadj", 4),
            7 => ("triangleadj", 6),
            other => return Err(format!("GS input primitive {other} unsupported")),
        };
        let stream = match gi.output_topology {
            1 => "PointStream",
            3 => "LineStream",
            5 => "TriangleStream",
            other => return Err(format!("GS output topology {other} unsupported")),
        };
        let in_dw = 4 * (self.ex.input_sig.iter().map(|e| e.register).max().unwrap_or(0) as usize + 1);
        let out_dw = 4 * (self.ex.output_sig.iter().map(|e| e.register).max().unwrap_or(0) as usize + 1);
        Ok((nv, in_dw, out_dw, prim, stream))
    }

    fn gs_max_vertices(&self) -> usize {
        let emits = self
            .insns
            .iter()
            .filter(|i| i.enc == Enc::Sopp && i.op == 0x10 && (i.aux.imm as u32 & 0xf) == 2 && (i.aux.imm as u32 >> 4) & 2 != 0)
            .count();
        (self.ex.gs_info.map(|g| g.max_vertex_count as usize).unwrap_or(0)).max(emits).max(1)
    }

    fn emit_gs_lds(&mut self, ins: &Insn) -> Result<(), String> {
        let (nv, in_dw, out_dw, _, _) = self.gs_dims()?;
        let gs_dw = out_dw * self.gs_max_vertices();
        let off = ins.aux.imm as u32;
        let (off0, off1) = (off & 0xff, (off >> 8) & 0xff);
        let addr = r(&ins.src[0])?;
        let t = self.tmp;
        self.tmp += 1;
        let in_at = |a: String| format!("tb_in[({a} >> 12) % {nv}u][(({a} & 0xfffu) >> 2) % {in_dw}u]");
        let vd = match ins.dst[0] {
            Opnd::Vgpr(v) => v,
            _ => return Err("bad vdst".into()),
        };
        match ins.mnemonic.as_str() {
            "ds_read_b32" => {
                self.line(&format!("uint t{t} = {};", in_at(format!("({addr} + {:#x}u)", off & 0xffff))));
                self.wv(&Opnd::Vgpr(vd), &format!("t{t}"))
            }
            "ds_read2_b32" => {
                self.line(&format!(
                    "uint t{t}a = {}; uint t{t}b = {};",
                    in_at(format!("({addr} + {:#x}u)", off0 * 4)),
                    in_at(format!("({addr} + {:#x}u)", off1 * 4))
                ));
                self.wv(&Opnd::Vgpr(vd), &format!("t{t}a"))?;
                self.wv(&Opnd::Vgpr(vd + 1), &format!("t{t}b"))
            }
            "ds_write_b32" => {
                self.line(&format!(
                    "if (exec_lo & 1u) tb_gs[(({addr} + {:#x}u) >> 2) % {gs_dw}u] = {};",
                    off & 0xffff,
                    r(&ins.src[1])?
                ));
                Ok(())
            }
            "ds_write2_b32" => {
                self.line(&format!(
                    "if (exec_lo & 1u) {{ tb_gs[(({addr} + {:#x}u) >> 2) % {gs_dw}u] = {}; tb_gs[(({addr} + {:#x}u) >> 2) % {gs_dw}u] = {}; }}",
                    off0 * 4,
                    r(&ins.src[1])?,
                    off1 * 4,
                    r(&ins.src[2])?
                ));
                Ok(())
            }
            _ => Err("unsupported LDS op".into()),
        }
    }

    /// `s_sendmsg`: in a geometry shader, message 2 = GS with op bits (1 = cut, 2 = emit),
    /// message 3 = GS_DONE.
    fn emit_sendmsg(&mut self, ins: &Insn) -> Result<(), String> {
        if self.is_ms() {
            return self.mesh_sendmsg(ins);
        }
        if !self.is_gs {
            return Err("s_sendmsg outside a geometry shader".into());
        }
        let imm = ins.aux.imm as u32 & 0xffff;
        let (msg, op, stream) = (imm & 0xf, (imm >> 4) & 3, (imm >> 8) & 3);
        match msg {
            3 => Ok(()),
            2 => {
                if stream != 0 {
                    return Err("GS output stream != 0 unsupported".into());
                }
                let (_, _, out_dw, _, _) = self.gs_dims()?;
                let maxv = self.gs_max_vertices();
                if op & 2 != 0 {
                    let mut body = format!("if (exec_lo & 1u) {{ SOut o = (SOut)0; uint tb_b = min(tb_emit, {}u) * {out_dw}u;", maxv - 1);
                    for (ei, e) in self.ex.output_sig.iter().enumerate() {
                        let (ty, _) = sig_type(e);
                        let comps: Vec<String> = (0..4)
                            .filter(|c| (e.mask >> c) & 1 != 0)
                            .map(|c| {
                                let w = format!("tb_gs[tb_b + {}u]", e.register as usize * 4 + c);
                                if e.comp_type == 3 { format!("asfloat({w})") } else { w }
                            })
                            .collect();
                        let _ = write!(body, " o.o{ei} = ({ty}){};", if comps.len() == 1 { comps[0].clone() } else { format!("{ty}({})", comps.join(", ")) });
                    }
                    body.push_str(" stream.Append(o); tb_emit++; }");
                    self.line(&body);
                }
                if op & 1 != 0 {
                    self.line("if (exec_lo & 1u) stream.RestartStrip();");
                }
                Ok(())
            }
            other => Err(format!("s_sendmsg message {other} unsupported")),
        }
    }

    fn reads_hwreg(&self, id: u32) -> bool {
        self.insns.iter().any(|i| i.mnemonic == "s_getreg_b32" && (i.aux.imm as u32 & 63) == id)
    }

    fn emit_bvh(&mut self, ins: &Insn) -> Result<(), String> {
        let n = ins.src.len();
        let a16 = ins.raw.get(1).is_some_and(|w| (w >> 30) & 1 != 0);
        let wide = ins.mnemonic == "image_bvh64_intersect_ray";
        let addrs: Vec<Opnd> = ins.src[..n - 2].to_vec();
        let need = if wide { 2 } else { 1 } + if a16 { 7 } else { 10 };
        let addrs: Vec<Opnd> = if ins.aux.imm2 == 0 {
            let base = match addrs.first() {
                Some(Opnd::Vgpr(b)) => *b,
                _ => return Err("bvh: bad vaddr".into()),
            };
            (0..need as u8).map(|k| Opnd::Vgpr(base + k)).collect()
        } else {
            addrs
        };
        if addrs.len() < need {
            return Err(format!("bvh: {} address registers, need {need}", addrs.len()));
        }
        let rsrc = match ins.src[n - 2] {
            Opnd::Sgpr(s) => s,
            _ => return Err("bvh: bad srsrc".into()),
        };
        let mut a = Vec::new();
        for o in &addrs[..need] {
            self.touch(o);
            a.push(r(o)?);
        }
        let mut d = Vec::new();
        for k in 0..4u8 {
            let o = Opnd::Sgpr(rsrc + k);
            self.touch(&o);
            d.push(r(&o)?);
        }
        self.uses_bvh = true;
        let (node, rest) = if wide {
            (format!("(((uint64_t){}) << 32) | (uint64_t){}", a[1], a[0]), &a[2..])
        } else {
            (format!("(uint64_t){}", a[0]), &a[1..])
        };
        let (dir, inv) = if a16 {
            (
                format!("float3(f16tof32({0}), f16tof32({0} >> 16), f16tof32({1}))", rest[4], rest[5]),
                format!("float3(f16tof32({0} >> 16), f16tof32({1}), f16tof32({1} >> 16))", rest[5], rest[6]),
            )
        } else {
            (
                format!("asfloat(uint3({}, {}, {}))", rest[4], rest[5], rest[6]),
                format!("asfloat(uint3({}, {}, {}))", rest[7], rest[8], rest[9]),
            )
        };
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!(
            "uint4 t{t} = tb_bvh_intersect(uint4({}, {}, {}, {}), {node}, asfloat({}), asfloat(uint3({}, {}, {})), {dir}, {inv});",
            d[0], d[1], d[2], d[3], rest[0], rest[1], rest[2], rest[3]
        ));
        let vd = match ins.dst[0] {
            Opnd::Vgpr(v) => v,
            _ => return Err("bvh: bad vdata".into()),
        };
        for (k, c) in ["x", "y", "z", "w"].iter().enumerate() {
            self.wv(&Opnd::Vgpr(vd + k as u8), &format!("t{t}.{c}"))?;
        }
        Ok(())
    }

    fn emit_mimg(&mut self, idx: usize, ins: &Insn) -> Result<(), String> {
        if ins.mnemonic.starts_with("image_bvh") {
            return self.emit_bvh(ins);
        }
        let w0 = ins.raw.first().copied().unwrap_or(0);
        let w1 = ins.raw.get(1).copied().unwrap_or(0);
        let unorm = (w0 >> 12) & 1 != 0;
        let a16 = (w1 >> 30) & 1 != 0;
        for (bit, name) in [(15, "R128"), (16, "TFE"), (17, "LWE")] {
            if (w0 >> bit) & 1 != 0 {
                return Err(format!("{} with {name} set at {:#x} is not translated", ins.mnemonic, ins.offset));
            }
        }
        if matches!(ins.mnemonic.as_str(), "image_atomic_fcmpswap" | "image_atomic_fmin" | "image_atomic_fmax") {
            return Err(format!(
                "{} at {:#x} is not translated: the RDNA2 ISA only says the float compare handles NaN/INF/denorm, DXIL has no float min/max image atomic, and no RDNA2-recorded results pin the NaN, signed-zero and denormal cases",
                ins.mnemonic, ins.offset
            ));
        }
        let rs = self.res.get(&idx).cloned().ok_or("unresolved image")?;
        if ins.mnemonic == "image_get_resinfo" {
            let (slot, dim, is_uav) = match rs.first() {
                Some(Res::Tex(t, d)) => (*t, *d, false),
                Some(Res::Uav(t, d)) => (*t, *d, true),
                _ => return Err("unresolved image".into()),
            };
            let mip = ins.src[0];
            self.touch(&mip);
            let mip = if a16 { format!("({} & 0xffffu)", r(&mip)?) } else { r(&mip)? };
            let obj = if is_uav { format!("uav{slot}") } else { format!("tex{slot}") };
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!("uint t{t}x = 0u, t{t}y = 0u, t{t}z = 0u, t{t}w = 1u;"));
            let call = match (dim, is_uav) {
                (0, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}w);"),
                (1, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}y, t{t}w);"),
                (2, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}y, t{t}z, t{t}w);"),
                (3, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}y, t{t}w); t{t}z = 6u;"),
                (4, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}y, t{t}w);"),
                (5, false) => format!("{obj}.GetDimensions({mip}, t{t}x, t{t}y, t{t}z, t{t}w);"),
                (6, false) => format!("{obj}.GetDimensions(t{t}x, t{t}y, t{t}z); t{t}z = 1u;"),
                (0, true) => format!("{obj}.GetDimensions(t{t}x);"),
                (1, true) | (4, true) => format!("{obj}.GetDimensions(t{t}x, t{t}y);"),
                (2, true) | (5, true) => format!("{obj}.GetDimensions(t{t}x, t{t}y, t{t}z);"),
                _ => return Err(format!("image dim {dim} unsupported for get_resinfo")),
            };
            self.line(&call);
            if !is_uav && dim != 6 {
                let q = call.replace(&format!("t{t}"), &format!("t{t}q")).replacen(&format!("({mip}, "), "(0u, ", 1);
                self.line(&format!("uint t{t}qx = 0u, t{t}qy = 0u, t{t}qz = 0u, t{t}qw = 1u; {q}"));
                self.line(&format!("if ({mip} >= t{t}qw) {{ t{t}x = 0u; t{t}y = 0u; t{t}z = 0u; t{t}w = t{t}qw; }}"));
            }
            let vd = match ins.dst[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vdata".into()),
            };
            let mut n = 0u8;
            for ch in 0..4 {
                if (ins.aux.mask >> ch) & 1 != 0 {
                    self.wv(&Opnd::Vgpr(vd + n), &format!("t{t}{}", ["x", "y", "z", "w"][ch]))?;
                    n += 1;
                }
            }
            return Ok(());
        }
        // Texel-addressed ops (no sampler): image_load / image_store, on SRVs or UAVs.
        let pck = matches!(
            ins.mnemonic.as_str(),
            "image_load_pck" | "image_load_pck_sgn" | "image_load_mip_pck" | "image_load_mip_pck_sgn" | "image_store_pck" | "image_store_mip_pck"
        );
        if pck || matches!(ins.mnemonic.as_str(), "image_load" | "image_load_mip" | "image_store" | "image_store_mip") || ins.mnemonic.starts_with("image_atomic_") {
            let with_mip = ins.mnemonic.starts_with("image_load_mip") || ins.mnemonic.starts_with("image_store_mip");
            let store = ins.mnemonic.starts_with("image_store");
            let (slot, dim, is_uav) = match rs.first() {
                Some(Res::Tex(t, d)) => (*t, *d, false),
                Some(Res::Uav(t, d)) => (*t, *d, true),
                _ => return Err("unresolved image".into()),
            };
            let ncoord = match dim {
                0 => 1,     // 1D
                1 => 2,     // 2D
                2 | 5 => 3, // 3D / 2D array
                4 => 2,     // 1D array
                6 => 2,     // 2D multisampled (the next component is the sample index)
                _ => return Err(format!("image dim {dim} unsupported for load/store")),
            };
            let nsa = ins.aux.imm2 as usize;
            let first = match ins.src[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vaddr".into()),
            };
            let addr_reg = |i: usize| -> Opnd { if nsa > 0 { ins.src[i] } else { Opnd::Vgpr(first + i as u8) } };
            let addr_i = |em: &mut Self, i: usize| -> Result<String, String> {
                if a16 {
                    let o = addr_reg(i / 2);
                    em.touch(&o);
                    Ok(format!("(({} >> {}u) & 0xffffu)", r(&o)?, 16 * (i % 2)))
                } else {
                    let o = addr_reg(i);
                    em.touch(&o);
                    r(&o)
                }
            };
            let mut c = vec![];
            for i in 0..ncoord {
                c.push(addr_i(self, i)?);
            }
            let vd = match ins.dst[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vdata".into()),
            };
            let as_uint = is_uav && self.uav_uint.contains(&slot);
            let as_u64 = is_uav && self.uav_u64.contains(&slot);
            if as_uint && as_u64 {
                return Err("image slot mixes 32-bit and 64-bit image atomics".into());
            }
            let uobj = if is_uav && with_mip {
                if slot & BINDLESS != 0 {
                    return Err(format!("{} on a bindless UAV: per-mip views need a heap-mirrored descriptor range", ins.mnemonic));
                }
                let mip_u = addr_i(self, ncoord)?;
                self.uav_mips.insert(slot);
                format!("uav{slot}_m[NonUniformResourceIndex(min({mip_u}, 15u))]")
            } else {
                format!("uav{slot}")
            };
            let d16_bit = ins.raw.get(1).is_some_and(|w| (w >> 31) & 1 != 0);
            if d16_bit && ins.mnemonic.starts_with("image_atomic_") {
                return Err("D16 image atomic unsupported (16-bit atomic data is not specified)".into());
            }
            if pck {
                if d16_bit {
                    return Err(format!("{} at {:#x} with D16 is not translated: the packed (no format conversion) opcodes have no D16 form", ins.mnemonic, ins.offset));
                }
                if !((as_uint && ins.aux.mask == 0x1) || (as_u64 && ins.aux.mask == 0x3)) {
                    return Err(format!(
                        "{} at {:#x} with dmask {:#x} is not translated: raw texel bits are only reproducible on an image the shader also uses with 32-bit (dmask 0x1) or 64-bit (dmask 0x3) image atomics; any other DXIL typed load or store converts through the view format, which the translator does not know",
                        ins.mnemonic, ins.offset, ins.aux.mask
                    ));
                }
            }
            let uint_zero = store && as_uint && ins.aux.mask == 1 && self.const_vgpr(idx, vd) == Some(0);
            if d16_bit && !uint_zero && as_uint {
                return Err("D16 access to an image also used with image atomics unsupported (texel signedness unknown)".into());
            }
            if let Some(op) = ins.mnemonic.strip_prefix("image_atomic_") {
                if !as_uint && !as_u64 {
                    return Err("image atomic on a non-UAV table".into());
                }
                let glc = ins.raw.first().is_some_and(|w| (w >> 13) & 1 != 0);
                if as_u64 {
                    let dmask_64 = if op == "cmpswap" { 0xf } else { 0x3 };
                    if ins.aux.mask != dmask_64 {
                        return Err(format!("{} with dmask {:#x}: unsupported 64-bit image atomic dmask", ins.mnemonic, ins.aux.mask));
                    }
                    self.sm6_minor = self.sm6_minor.max(6);
                    self.touch(&Opnd::Vgpr(vd));
                    self.touch(&Opnd::Vgpr(vd + 1));
                    let v64 = format!("(((uint64_t)v{} << 32) | (uint64_t)v{vd})", vd + 1);
                    let addr = format!("{uobj}[uint{ncoord}({})]", c.join(", "));
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint64_t t{t} = 0;"));
                    let call = match op {
                        "swap" => format!("InterlockedExchange({addr}, {v64}, t{t});"),
                        "add" => format!("InterlockedAdd({addr}, {v64}, t{t});"),
                        "sub" => format!("InterlockedAdd({addr}, (uint64_t)0 - ({v64}), t{t});"),
                        "umin" => format!("InterlockedMin({addr}, {v64}, t{t});"),
                        "umax" => format!("InterlockedMax({addr}, {v64}, t{t});"),
                        "and" => format!("InterlockedAnd({addr}, {v64}, t{t});"),
                        "or" => format!("InterlockedOr({addr}, {v64}, t{t});"),
                        "xor" => format!("InterlockedXor({addr}, {v64}, t{t});"),
                        "cmpswap" => {
                            self.touch(&Opnd::Vgpr(vd + 2));
                            self.touch(&Opnd::Vgpr(vd + 3));
                            let cmp64 = format!("(((uint64_t)v{} << 32) | (uint64_t)v{})", vd + 3, vd + 2);
                            format!("InterlockedCompareExchange({addr}, {cmp64}, {v64}, t{t});")
                        }
                        _ => return Err(format!("64-bit image atomic {op} unsupported")),
                    };
                    self.line(&format!("if (exec_lo & 1u) {call}"));
                    if glc {
                        self.wv(&Opnd::Vgpr(vd), &format!("(uint)t{t}"))?;
                        self.wv(&Opnd::Vgpr(vd + 1), &format!("(uint)(t{t} >> 32)"))?;
                    }
                    return Ok(());
                }
                let dmask_32 = if op == "cmpswap" { 0x3 } else { 0x1 };
                if ins.aux.mask != dmask_32 {
                    return Err(format!("{} with dmask {:#x}: only 32-bit and 64-bit image atomics are supported", ins.mnemonic, ins.aux.mask));
                }
                let o = Opnd::Vgpr(vd);
                let old = self.atomic_rmw(op, &BufRef::Elem(format!("{uobj}[uint{ncoord}({})]", c.join(", "))), o)?;
                if glc {
                    self.wv(&o, &old)?;
                }
                return Ok(());
            }
            if store && as_u64 {
                if ins.aux.mask != 0x3 {
                    return Err(format!("image_store with dmask {:#x} on a 64-bit atomic UAV image unsupported", ins.aux.mask));
                }
                self.sm6_minor = self.sm6_minor.max(6);
                self.touch(&Opnd::Vgpr(vd));
                self.touch(&Opnd::Vgpr(vd + 1));
                self.line(&format!("if (exec_lo & 1u) {uobj}[uint{ncoord}({})] = (((uint64_t)v{} << 32) | (uint64_t)v{vd});", c.join(", "), vd + 1));
                return Ok(());
            }
            if !store && as_u64 {
                if ins.aux.mask != 0x3 {
                    return Err(format!("image_load with dmask {:#x} on a 64-bit atomic UAV image unsupported", ins.aux.mask));
                }
                self.sm6_minor = self.sm6_minor.max(6);
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint64_t t{t} = {uobj}[uint{ncoord}({})];", c.join(", ")));
                self.wv(&Opnd::Vgpr(vd), &format!("(uint)t{t}"))?;
                self.wv(&Opnd::Vgpr(vd + 1), &format!("(uint)(t{t} >> 32)"))?;
                return Ok(());
            }
            if store && as_uint {
                let v = self.mimg_store_lanes(ins, vd, Lane::Uint, "0u")?;
                let first_on = (0..4).find(|ch| (ins.aux.mask >> ch) & 1 != 0).unwrap_or(0);
                self.line(&format!("if (exec_lo & 1u) {uobj}[uint{ncoord}({})] = {};", c.join(", "), v[first_on]));
                return Ok(());
            }
            if !store && as_uint {
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint4 t{t} = uint4({uobj}[uint{ncoord}({})], 0u, 0u, 0u);", c.join(", ")));
                let vals = (0..4).filter(|ch| (ins.aux.mask >> ch) & 1 != 0).map(|ch| format!("t{t}.{}", XYZW[ch])).collect();
                return self.mimg_write_lanes(ins, vd, vals, Lane::Uint);
            }
            let ity = self.img_ty.get(&(is_uav, slot)).copied();
            if store {
                if !is_uav {
                    return Err("image_store to a non-UAV table".into());
                }
                if let Some(t) = ity {
                    let comp = self.mimg_store_lanes(ins, vd, if t == 2 { Lane::Sint } else { Lane::Uint }, "0u")?;
                    let ctor = if t == 2 { "int4" } else { "uint4" };
                    self.line(&format!("if (exec_lo & 1u) {uobj}[uint{ncoord}({})] = {ctor}({});", c.join(", "), comp.join(", ")));
                    return Ok(());
                }
                let comp = self.mimg_store_lanes(ins, vd, Lane::Float, "0.0")?;
                let addr = format!("{uobj}[uint{ncoord}({})]", c.join(", "));
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("float4 t{t} = float4({});", comp.join(", ")));
                self.line(&format!("if (exec_lo & 1u) {{ {addr} = t{t}; {} }}", small_float_store_fixup(&addr, &format!("t{t}"))));
            } else {
                let t = self.tmp;
                self.tmp += 1;
                // image_load_mip carries the mip level as one more address component.
                let mip = if with_mip {
                    format!("(int){}", addr_i(self, ncoord)?)
                } else {
                    "0".to_string()
                };
                let fm = if is_uav || with_mip { None } else { self.fmask.get(&slot).copied() };
                if let Some(ms) = fm {
                    let t = self.tmp;
                    self.tmp += 1;
                    self.line(&format!("uint t{t}w, t{t}h, t{t}n; tex{ms}.GetDimensions(t{t}w, t{t}h, t{t}n); uint t{t} = t{t}n >= 8u ? 0x76543210u : (t{t}n >= 4u ? 0x88883210u : (t{t}n >= 2u ? 0x88888810u : 0x88888880u));"));
                    let vals = (0..4).filter(|ch| (ins.aux.mask >> ch) & 1 != 0).map(|ch| if ch == 0 { format!("t{t}") } else { "0u".to_string() }).collect();
                    return self.mimg_write_lanes(ins, vd, vals, Lane::Uint);
                }
                let e = if dim == 6 {
                    if is_uav {
                        return Err("multisampled UAV load unsupported".into());
                    }
                    let sample = addr_i(self, ncoord)?;
                    format!("tex{slot}.Load(int2({}), (int){sample})", c.iter().map(|x| format!("(int){x}")).collect::<Vec<_>>().join(", "))
                } else if is_uav {
                    format!("{uobj}[uint{ncoord}({})]", c.join(", "))
                } else {
                    format!("tex{slot}.Load(int{}({}, {mip}))", ncoord + 1, c.iter().map(|x| format!("(int){x}")).collect::<Vec<_>>().join(", "))
                };
                let ety = match ity {
                    Some(2) => "int4",
                    Some(_) => "uint4",
                    None => "float4",
                };
                self.line(&format!("{ety} t{t} = {e};"));
                let vals = (0..4)
                    .filter(|ch| (ins.aux.mask >> ch) & 1 != 0)
                    .map(|ch| match ety {
                        "float4" => format!("asuint(t{t}.{})", XYZW[ch]),
                        "int4" => format!("(uint)t{t}.{}", XYZW[ch]),
                        _ => format!("t{t}.{}", XYZW[ch]),
                    })
                    .collect();
                let lane = match ety {
                    "float4" => Lane::Float,
                    "int4" => Lane::Sint,
                    _ => Lane::Uint,
                };
                self.mimg_write_lanes(ins, vd, vals, lane)?;
            }
            return Ok(());
        }
        let (tslot, dim) = match rs.first() {
            Some(Res::Tex(t, d)) => (*t, *d),
            _ => return Err("unresolved image".into()),
        };
        let sslot = match rs.get(1) {
            Some(Res::Samp(s)) => *s,
            _ => return Err("image op without sampler unsupported".into()),
        };
        // RDNA2 sample/gather family. Opcode = family base (0x20 sample, 0x40 gather4) +
        // modifier (0 none, 1 cl, 2 d, 3 d_cl, 4 l, 5 b, 6 b_cl, 7 lz) + 8 (depth compare)
        // + 0x10 (texel offset). Address operands arrive in this order:
        //   [offset] [bias] [zref] [ddx.. ddy..] coords.. [lod] [lod clamp]
        if ins.mnemonic == "image_get_lod" {
            if self.ex.stage != Stage::Pixel {
                return Err("image_get_lod outside a pixel shader".into());
            }
            let nc = match dim {
                0 | 4 => 1usize,
                1 | 5 => 2,
                2 | 3 => 3,
                _ => return Err(format!("image dim {dim} unsupported for get_lod")),
            };
            let nsa = ins.aux.imm2 as usize;
            let first = match ins.src[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vaddr".into()),
            };
            let mut c = vec![];
            for i in 0..nc {
                let o = if nsa > 0 {
                    ins.src[if a16 { i / 2 } else { i }]
                } else {
                    Opnd::Vgpr(first + (if a16 { i / 2 } else { i }) as u8)
                };
                self.touch(&o);
                c.push(if a16 { format!("f16tof32({} >> {}u)", r(&o)?, 16 * (i % 2)) } else { f(&o)? });
            }
            if unorm {
                let (outs, n) = match dim {
                    0 => ("w, l", 1),
                    4 => ("w, e, l", 1),
                    1 => ("w, h, l", 2),
                    5 => ("w, h, e, l", 2),
                    2 => ("w, h, d, l", 3),
                    _ => return Err("unnormalised coordinates on a cube image unsupported".into()),
                };
                let td = self.tmp;
                self.tmp += 1;
                let decl: Vec<String> = outs.split(", ").map(|v| format!("{v}{td}")).collect();
                self.line(&format!("uint {}; tex{tslot}.GetDimensions(0u, {});", decl.join(", "), decl.join(", ")));
                for (k, sz) in ["w", "h", "d"].iter().take(n).enumerate() {
                    c[k] = format!("({} / (float){sz}{td})", c[k]);
                }
            }
            let cv = if dim == 3 { format!("tb_cube_dir({})", c.join(", ")) } else if nc == 1 { c[0].clone() } else { format!("float{nc}({})", c.join(", ")) };
            let vd = match ins.dst[0] {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad vdata".into()),
            };
            let t = self.tmp;
            self.tmp += 1;
            self.line(&format!(
                "float2 t{t} = float2(tex{tslot}.CalculateLevelOfDetail(samp{sslot}, {cv}), tex{tslot}.CalculateLevelOfDetailUnclamped(samp{sslot}, {cv}));"
            ));
            let vals = (0..4).filter(|ch| (ins.aux.mask >> ch) & 1 != 0).map(|ch| if ch < 2 { format!("asuint(t{t}.{})", XYZW[ch]) } else { "0u".to_string() }).collect();
            return self.mimg_write_lanes(ins, vd, vals, Lane::Float);
        }
        let op = ins.op;
        let g16 = op & 0x80 != 0;
        let op = op & 0x7f;
        let gather = (0x40..0x60).contains(&op);
        if !gather && !(0x20..0x40).contains(&op) {
            return Err("unsupported image op".into());
        }
        let (modi, cmp, has_off) = (op & 7, op & 8 != 0, op & 0x10 != 0);
        // (coordinate count, derivative components, offset components)
        let (ncoord, nder, noff) = match dim {
            0 => (1usize, 1usize, 1usize),
            1 => (2, 2, 2),
            2 => (3, 3, 3),
            3 => (3, 3, 0),
            4 => (2, 1, 1),
            5 => (3, 2, 2),
            _ => return Err(format!("image dim {dim} unsupported")),
        };
        // Address operands: explicit list with NSA, else consecutive VGPRs from vaddr.
        let nsa = ins.aux.imm2 as usize;
        let first = match ins.src[0] {
            Opnd::Vgpr(n) => n,
            _ => return Err("bad vaddr".into()),
        };
        let addr = |i: usize| -> Opnd {
            if nsa > 0 { ins.src[i] } else { Opnd::Vgpr(first + i as u8) }
        };
        let mut next = 0usize;
        let mut take = |em: &mut Self| -> Opnd {
            let o = addr(next);
            next += 1;
            em.touch(&o);
            o
        };
        // Texel offset: packed signed 6-bit fields (x [5:0], y [13:8], z [21:16]).
        let offset = if has_off {
            let o = take(self);
            if noff == 0 {
                return Err("texel offset on a cube image".into());
            }
            let sx = |v: u32, sh: u32| (((v >> sh) & 0x3f) as i32) << 26 >> 26;
            let reg = match o {
                Opnd::Vgpr(n) => n,
                _ => return Err("bad offset operand".into()),
            };
            match self.const_vgpr(idx, reg) {
                Some(v) => {
                    let c: Vec<String> = (0..noff).map(|k| sx(v, 8 * k as u32).to_string()).collect();
                    Some(if noff == 1 { c[0].clone() } else { format!("int{noff}({})", c.join(", ")) })
                }
                None if gather => {
                    // SM5 Gather accepts a run-time offset.
                    let x = r(&o)?;
                    let c: Vec<String> =
                        (0..noff).map(|k| format!("(asint({x} << {}) >> 26)", 26 - 8 * k)).collect();
                    Some(if noff == 1 { c[0].clone() } else { format!("int{noff}({})", c.join(", ")) })
                }
                None => return Err("non-constant texel offset on a sample op".into()),
            }
        } else {
            None
        };
        let bias = if matches!(modi, 5 | 6) {
            let o = take(self);
            Some(if a16 { format!("f16tof32({})", r(&o)?) } else { f(&o)? })
        } else {
            None
        };
        let zref = if cmp { Some(f(&take(self))?) } else { None };
        let mut grads = if matches!(modi, 2 | 3) {
            if g16 {
                if nder != 2 {
                    return Err(format!("{} with {nder}-component 16-bit gradients is not lowered (only the 2-component packing is verified)", ins.mnemonic));
                }
                let mut halves: Vec<String> = Vec::with_capacity(2 * nder);
                for _ in 0..nder {
                    let o = take(self);
                    halves.push(format!("f16tof32({} >> {}u)", r(&o)?, 0));
                    halves.push(format!("f16tof32({} >> {}u)", r(&o)?, 16));
                }
                let dy = halves.split_off(nder);
                Some((halves, dy))
            } else {
                let mut dx = vec![];
                for _ in 0..nder {
                    dx.push(f(&take(self))?);
                }
                let mut dy = vec![];
                for _ in 0..nder {
                    dy.push(f(&take(self))?);
                }
                Some((dx, dy))
            }
        } else {
            None
        };
        let base = next;
        let mut half = 0usize;
        let mut takef = |em: &mut Self| -> Result<String, String> {
            let k = half;
            half += 1;
            if a16 {
                let o = addr(base + k / 2);
                em.touch(&o);
                Ok(format!("f16tof32({} >> {}u)", r(&o)?, 16 * (k % 2)))
            } else {
                let o = addr(base + k);
                em.touch(&o);
                f(&o)
            }
        };
        let mut coords = vec![];
        for _ in 0..ncoord {
            coords.push(takef(self)?);
        }
        let lod = if modi == 4 { Some(takef(self)?) } else { None };
        let clamp = if matches!(modi, 1 | 3 | 6) { Some(takef(self)?) } else { None };
        if unorm {
            let (outs, n) = match dim {
                0 => ("w, l", 1),
                4 => ("w, e, l", 1),
                1 => ("w, h, l", 2),
                5 => ("w, h, e, l", 2),
                2 => ("w, h, d, l", 3),
                _ => return Err("unnormalised coordinates on a cube image unsupported".into()),
            };
            let td = self.tmp;
            self.tmp += 1;
            let decl: Vec<String> = outs.split(", ").map(|v| format!("{v}{td}")).collect();
            self.line(&format!("uint {}; tex{tslot}.GetDimensions(0u, {});", decl.join(", "), decl.join(", ")));
            for (k, sz) in ["w", "h", "d"].iter().take(n).enumerate() {
                coords[k] = format!("({} / (float){sz}{td})", coords[k]);
                if let Some((dx, dy)) = &mut grads {
                    if k < dx.len() {
                        dx[k] = format!("({} / (float){sz}{td})", dx[k]);
                        dy[k] = format!("({} / (float){sz}{td})", dy[k]);
                    }
                }
            }
        }
        let grads = grads.map(|(dx, dy)| {
            let v = |c: &Vec<String>| if c.len() == 1 { c[0].clone() } else { format!("float{}({})", c.len(), c.join(", ")) };
            (v(&dx), v(&dy))
        });
        // Cube images are addressed by hardware (s, t, face) coordinates (see tb_cube_dir).
        let cvec = if dim == 3 {
            format!("tb_cube_dir({})", coords.join(", "))
        } else if ncoord == 1 {
            coords[0].clone()
        } else {
            format!("float{ncoord}({})", coords.join(", "))
        };
        let tex = format!("tex{tslot}");
        let smp = if cmp { format!("scmp{sslot}") } else { format!("samp{sslot}") };
        let zero_off = if noff == 1 { "0".to_string() } else { format!("int{}({})", noff.max(1), vec!["0"; noff.max(1)].join(", ")) };
        // Optional trailing ", offset[, clamp]" arguments.
        let tail = |offset: &Option<String>, clamp: &Option<String>| -> Result<String, String> {
            Ok(match (offset, clamp) {
                (None, None) => String::new(),
                (Some(o), None) => format!(", {o}"),
                (o, Some(c)) => {
                    if noff == 0 {
                        return Err("LOD clamp on a cube image unsupported".into());
                    }
                    format!(", {}, {c}", o.clone().unwrap_or_else(|| zero_off.clone()))
                }
            })
        };
        let t = self.tmp;
        self.tmp += 1;
        let vd = match ins.dst[0] {
            Opnd::Vgpr(n) => n,
            _ => return Err("bad vdata".into()),
        };
        if gather {
            // gather4: dmask picks the channel; the four texels fill vdata..vdata+3.
            if !matches!(modi, 0 | 4 | 7) {
                return Err("gather4 with LOD bias or clamp unsupported".into());
            }
            let chan = match ins.aux.mask {
                1 => "Red",
                2 => "Green",
                4 => "Blue",
                8 => "Alpha",
                m => return Err(format!("gather4 with dmask {m:#x} unsupported")),
            };
            if modi == 4 {
                if !matches!(dim, 1 | 5) {
                    return Err(format!("{} on image dim {dim} unsupported", ins.mnemonic));
                }
                if unorm {
                    return Err(format!("{} with unnormalised coordinates unsupported", ins.mnemonic));
                }
                let l = lod.clone().ok_or("missing lod")?;
                let lim = match self.gather_ssamp.get(&idx).copied().flatten() {
                    Some((w1, w2)) => Some(gather_lod_fields(w1, w2).map_err(|e| format!("{} at {:#x}: {e}", ins.mnemonic, ins.offset))?),
                    None => {
                        self.gather_blind.insert(ins.offset as u32);
                        None
                    }
                };
                let (l, back) = match lim {
                    Some((b, lo, hi)) => (format!("clamp({l} + {b:?}, {lo:?}, {hi:?})"), format!(" - {b:?}")),
                    None => (l, String::new()),
                };
                let k = XYZW[ins.aux.mask.trailing_zeros() as usize];
                let ext = if dim == 5 { format!("t{t}e, ") } else { String::new() };
                self.line(&format!("uint t{t}w, t{t}h, t{t}e, t{t}n;"));
                self.line(&format!("{tex}.GetDimensions(0u, t{t}w, t{t}h, {ext}t{t}n);"));
                self.line(&format!("float t{t}l = {l};"));
                self.line(&format!("uint t{t}m = min(t{t}l > 0.5 ? (uint)(ceil(min(t{t}l, 16.0) + 0.5) - 1.0) : 0u, t{t}n - 1u);"));
                self.line(&format!("{tex}.GetDimensions(t{t}m, t{t}w, t{t}h, {ext}t{t}n);"));
                self.line(&format!("float2 t{t}s = float2(t{t}w, t{t}h);"));
                let o = offset.map(|o| format!(" + {o}")).unwrap_or_default();
                self.line(&format!("int2 t{t}i = int2(floor(float2({}, {}) * t{t}s - 0.5)){o};", coords[0], coords[1]));
                if zref.is_some() {
                    self.sm6_minor = self.sm6_minor.max(7);
                }
                let mut vals = Vec::new();
                for (n, (dx, dy)) in [(0, 1), (1, 1), (1, 0), (0, 0)].into_iter().enumerate() {
                    let c = format!("(float2(t{t}i + int2({dx}, {dy})) + 0.5) / t{t}s");
                    let c = if dim == 5 { format!("float3({c}, {})", coords[2]) } else { c };
                    let call = match &zref {
                        Some(z) => format!("{tex}.SampleCmpLevel({smp}, {c}, {z}, (float)t{t}m{back})"),
                        None => format!("{tex}.SampleLevel({smp}, {c}, (float)t{t}m{back}).{k}"),
                    };
                    self.line(&format!("float t{t}g{n} = {call};"));
                    vals.push(format!("asuint(t{t}g{n})"));
                }
                return self.mimg_write_lanes(ins, vd, vals, Lane::Float);
            }
            let off = offset.map(|o| format!(", {o}")).unwrap_or_default();
            let gtex = if modi == 0 {
                if self.ex.stage != Stage::Pixel {
                    return Err(format!("{} (implicit LOD) outside a pixel shader", ins.mnemonic));
                }
                if tslot & BINDLESS != 0 {
                    return Err(format!("{} on a bindless texture: per-mip views need a heap-mirrored descriptor range", ins.mnemonic));
                }
                self.tex_mips.insert(tslot);
                if zref.is_some() {
                    self.sm6_minor = self.sm6_minor.max(8);
                }
                self.line(&format!("float t{t}l = {tex}.CalculateLevelOfDetail({smp}, {cvec});"));
                self.line(&format!("uint t{t}m = t{t}l <= 0.5 ? 0u : (uint)(ceil(t{t}l + 0.5) - 1.0);"));
                format!("tex{tslot}_m[NonUniformResourceIndex(min(t{t}m, 15u))]")
            } else {
                tex.clone()
            };
            let call = match &zref {
                Some(z) => format!("{gtex}.GatherCmp{chan}({smp}, {cvec}, {z}{off})"),
                None => format!("{gtex}.Gather{chan}({smp}, {cvec}{off})"),
            };
            self.line(&format!("float4 t{t} = {call};"));
            let vals = (0..4).map(|k| format!("asuint(t{t}.{})", XYZW[k])).collect();
            return self.mimg_write_lanes(ins, vd, vals, Lane::Float);
        }
        // FXC (SM5) refuses implicit-derivative samples inside a loop it cannot unroll
        // (X3570/X3511). The console GPU computes those derivatives per quad regardless of
        // control flow; there is no SM5 equivalent, so inside emitted loops such samples
        // are APPROXIMATED with LOD 0 (exact for single-mip textures, too sharp otherwise).
        let in_loop = self.loop_depth > 0;
        let call = if let Some(z) = &zref {
            // Depth-compare sampling returns a single value.
            match modi {
                0 | 1 if in_loop => format!("{tex}.SampleCmpLevelZero({smp}, {cvec}, {z}{})", tail(&offset, &None)?),
                0 | 1 => format!("{tex}.SampleCmp({smp}, {cvec}, {z}{})", tail(&offset, &clamp)?),
                7 => format!("{tex}.SampleCmpLevelZero({smp}, {cvec}, {z}{})", tail(&offset, &None)?),
                4 => {
                    self.sm6_minor = self.sm6_minor.max(7);
                    format!("{tex}.SampleCmpLevel({smp}, {cvec}, {z}, {}{})", lod.clone().ok_or("missing lod")?, tail(&offset, &None)?)
                }
                5 | 6 if in_loop => {
                    self.sm6_minor = self.sm6_minor.max(7);
                    format!("{tex}.SampleCmpLevel({smp}, {cvec}, {z}, {}{})", bias.clone().ok_or("missing bias")?, tail(&offset, &None)?)
                }
                5 | 6 => {
                    self.sm6_minor = self.sm6_minor.max(8);
                    format!("{tex}.SampleCmpBias({smp}, {cvec}, {z}, {}{})", bias.clone().ok_or("missing bias")?, tail(&offset, &clamp)?)
                }
                2 | 3 => {
                    self.sm6_minor = self.sm6_minor.max(8);
                    let (dx, dy) = grads.clone().ok_or("missing gradients")?;
                    format!("{tex}.SampleCmpGrad({smp}, {cvec}, {z}, {dx}, {dy}{})", tail(&offset, &clamp)?)
                }
                _ => return Err("depth-compare sample with an unknown LOD modifier".into()),
            }
        } else {
            match modi {
                0 | 1 if in_loop => format!("{tex}.SampleLevel({smp}, {cvec}, 0.0{})", tail(&offset, &None)?),
                5 | 6 if in_loop => format!("{tex}.SampleLevel({smp}, {cvec}, {}{})", bias.clone().ok_or("missing bias")?, tail(&offset, &None)?),
                0 | 1 => format!("{tex}.Sample({smp}, {cvec}{})", tail(&offset, &clamp)?),
                2 | 3 => {
                    let (dx, dy) = grads.clone().ok_or("missing gradients")?;
                    format!("{tex}.SampleGrad({smp}, {cvec}, {dx}, {dy}{})", tail(&offset, &clamp)?)
                }
                4 => format!("{tex}.SampleLevel({smp}, {cvec}, {}{})", lod.clone().ok_or("missing lod")?, tail(&offset, &None)?),
                5 | 6 => format!("{tex}.SampleBias({smp}, {cvec}, {}{})", bias.clone().ok_or("missing bias")?, tail(&offset, &clamp)?),
                _ => format!("{tex}.SampleLevel({smp}, {cvec}, 0.0{})", tail(&offset, &None)?),
            }
        };
        if zref.is_some() {
            self.line(&format!("float t{t} = {call};"));
            // The compare result is replicated to every requested channel.
            let vals = (0..4).filter(|ch| (ins.aux.mask >> ch) & 1 != 0).map(|_| format!("asuint(t{t})")).collect();
            return self.mimg_write_lanes(ins, vd, vals, Lane::Float);
        }
        if in_loop && matches!(modi, 0 | 1 | 5 | 6) && self.ex.stage == Stage::Pixel {
            let sw = match (nder, ncoord) {
                (1, 1) => "",
                (1, _) => ".x",
                (2, 2) => "",
                (2, _) => ".xy",
                _ => "",
            };
            let fty = if nder == 1 { "float".to_string() } else { format!("float{nder}") };
            let cty = if ncoord == 1 { "float".to_string() } else { format!("float{ncoord}") };
            let scale = match &bias {
                Some(b) if matches!(modi, 5 | 6) => format!(" * exp2({b})"),
                _ => String::new(),
            };
            self.line("#if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6");
            self.line(&format!("{cty} tq{t} = {cvec};"));
            self.line(&format!("{fty} tqx{t} = (QuadReadLaneAt(tq{t}{sw}, 1) - QuadReadLaneAt(tq{t}{sw}, 0)){scale};"));
            self.line(&format!("{fty} tqy{t} = (QuadReadLaneAt(tq{t}{sw}, 2) - QuadReadLaneAt(tq{t}{sw}, 0)){scale};"));
            let cl = if matches!(modi, 1 | 6) { clamp.clone() } else { None };
            self.line(&format!("float4 t{t} = {tex}.SampleGrad({smp}, tq{t}, tqx{t}, tqy{t}{});", tail(&offset, &cl)?));
            self.line("#else");
            self.line(&format!("float4 t{t} = {call};"));
            self.line("#endif");
        } else {
            self.line(&format!("float4 t{t} = {call};"));
        }
        let vals = (0..4).filter(|ch| (ins.aux.mask >> ch) & 1 != 0).map(|ch| format!("asuint(t{t}.{})", XYZW[ch])).collect();
        self.mimg_write_lanes(ins, vd, vals, Lane::Float)
    }

    /// Statically known value of a VGPR at instruction `idx`: the most recent write in
    /// program order must be a `v_mov_b32` of a constant. Used where HLSL demands a
    /// literal (texel offsets).
    fn const_vgpr(&self, idx: usize, reg: u8) -> Option<u32> {
        for ins in self.insns[..idx].iter().rev() {
            if ins.dst.iter().any(|d| matches!(d, Opnd::Vgpr(n) if *n == reg)) {
                if ins.mnemonic != "v_mov_b32" {
                    return None;
                }
                return match ins.src.first() {
                    Some(Opnd::ConstInt(v)) => Some(*v as i32 as u32),
                    Some(Opnd::Literal(v)) => Some(*v),
                    Some(Opnd::ConstF32(f)) => Some(f.to_bits()),
                    _ => None,
                };
            }
        }
        None
    }

    fn emit_exp(&mut self, ins: &Insn, is_ps: bool) -> Result<(), String> {
        if self.is_ms() && self.mesh_exp(ins)? {
            return Ok(());
        }
        let target = ins.aux.imm as u32;
        let en = ins.aux.mask;
        let compr = ins.aux.imm2 & 1 != 0;
        let mut comp = vec![];
        if compr {
            for i in 0..4usize {
                let src = &ins.src[i / 2];
                let on = (en >> (i / 2 * 2)) & 1 != 0 || (en >> i) & 1 != 0;
                comp.push(if on {
                    format!("f16tof32(({} >> {}) & 0xffffu)", r(src)?, (i % 2) * 16)
                } else {
                    "0.0".into()
                });
            }
        } else {
            for i in 0..4usize {
                comp.push(if (en >> i) & 1 != 0 { f(&ins.src[i])? } else { "0.0".into() });
            }
        }
        let v4 = format!("float4({})", comp.join(", "));
        // The exported registers as raw 32-bit values, when they are available unpacked.
        let raw: Option<Vec<String>> = if compr {
            self.epilog_exp.get(&self.cur_idx).copied().map(|(a, b)| {
                (0..4usize)
                    .map(|i| {
                        let id = if i < 2 { a } else { b };
                        let on = (en >> (i / 2 * 2)) & 1 != 0 || (en >> i) & 1 != 0;
                        if on && id != usize::MAX { format!("tb_pk{id}{}", if i % 2 == 0 { "a" } else { "b" }) } else { "0u".to_string() }
                    })
                    .collect()
            })
        } else {
            let mut v = vec![];
            for i in 0..4usize {
                v.push(if (en >> i) & 1 != 0 { r(&ins.src[i])? } else { "0u".into() });
            }
            Some(v)
        };
        if is_ps && ins.aux.imm2 & 4 != 0 {
            // Valid-mask export: lanes that are inactive here are killed (pixel discard).
            self.line("if ((exec_lo & 1u) == 0u) discard;");
        }
        if is_ps && target == 9 {
            return Ok(()); // null export (no colour written)
        }
        if is_ps && target == 8 {
            // MRTZ: x = depth.
            let depth = self.ex.output_sig.iter().any(|e| matches!(e.sysval, 65 | 67 | 68));
            let stencil = self.ex.output_sig.iter().position(|e| e.sysval == 69);
            let coverage = self.ex.output_sig.iter().position(|e| e.sysval == 66);
            if depth && en & 1 == 0 {
                return Err("MRTZ export leaves depth unwritten although the output signature has SV_Depth".into());
            }
            if !depth && stencil.is_none() && coverage.is_none() {
                return Err("MRTZ export but the output signature has no SV_Depth, SV_StencilRef or SV_Coverage".into());
            }
            if depth {
                self.line(&format!("if (exec_lo & 1u) o.od = {};", f(&ins.src[0])?));
            }
            if let Some(si) = stencil.filter(|_| en & 2 != 0) {
                self.line(&format!("if (exec_lo & 1u) o.o{si} = {};", r(&ins.src[1])?));
            }
            if let Some(ci) = coverage.filter(|_| en & 4 != 0) {
                self.line(&format!("if (exec_lo & 1u) o.o{ci} = {};", r(&ins.src[2])?));
            }
            return Ok(());
        }
        let max_params = self.out_params.len() + if self.is_ms() { self.prim_params.len() } else { 0 };
        if !is_ps && (32..64).contains(&target) && (target - 32) as usize >= max_params {
            // Hardware needs at least one parameter export; with no matching output
            // element nothing downstream can read it.
            self.line("// (parameter export with no output-signature element: ignored)");
            return Ok(());
        }
        // (element index, swizzle of the exported register that feeds it)
        let osig = &self.ex.output_sig;
        let mut assigns: Vec<(usize, String)> = Vec::new();
        if is_ps {
            if target > 7 {
                return Err(format!("PS export target {target} unsupported"));
            }
            // MRT export k feeds the k-th declared SV_Target (outputs are compacted:
            // a shader writing SV_Target0 and SV_Target4 exports MRT 0 and MRT 1).
            let ei = osig
                .iter()
                .enumerate()
                .filter(|(_, e)| e.sysval == 64)
                .nth(target as usize)
                .map(|(i, _)| i)
                .ok_or_else(|| format!("MRT export {target} has no SV_Target output element"))?;
            assigns.push((ei, "xyzw"[..sig_type(&osig[ei]).1].to_string()));
        } else if (12..=15).contains(&target) {
            let misc: Vec<(usize, &str)> = osig
                .iter()
                .enumerate()
                .filter_map(|(ei, e)| match e.sysval {
                    4 => Some((ei, "z")),
                    5 => Some((ei, "w")),
                    _ => None,
                })
                .collect();
            if target == 13 && !misc.is_empty() {
                let packed_view = self.durango.is_none() && osig.iter().any(|e| e.sysval == 5);
                if packed_view && compr {
                    return Err("position export 13 packs SV_ViewportArrayIndex into the layer dword, and a compressed export has no integer dword".into());
                }
                for (ei, c) in misc {
                    if packed_view && !matches!(osig[ei].comp_type, 1 | 2) {
                        return Err(format!("position export 13 packs {} as an integer field of the layer dword", osig[ei].name));
                    }
                    let lane = if packed_view { 2 } else if c == "z" { 2 } else { 3 };
                    if (en >> lane) & 1 == 0 {
                        let which = if lane == 2 { "z" } else { "w" };
                        return Err(format!("position export 13 (misc vector) leaves the {which} lane of {} disabled", osig[ei].name));
                    }
                    let sw = if !packed_view {
                        c.to_string()
                    } else if osig[ei].sysval == 5 {
                        "view".into()
                    } else {
                        "layer".into()
                    };
                    assigns.push((ei, sw));
                }
            } else if target != 12 {
                let mut regs: Vec<u32> = osig.iter().filter(|e| matches!(e.sysval, 2 | 3)).map(|e| e.register).collect();
                regs.sort();
                regs.dedup();
                let first = if misc.is_empty() { 13 } else { 14 };
                let Some(&reg) = regs.get(target.checked_sub(first).unwrap_or(u32::MAX) as usize) else {
                    return Ok(());
                };
                for (ei, e) in osig.iter().enumerate() {
                    if matches!(e.sysval, 2 | 3) && e.register == reg {
                        let sw: String = (0..4).filter(|c| (e.mask >> c) & 1 != 0).map(|c| XYZW[c]).collect();
                        assigns.push((ei, sw));
                    }
                }
            } else {
            match osig.iter().position(|e| e.sysval == 1) {
                Some(ei) => assigns.push((ei, "xyzw".to_string())),
                None => {
                    // A vertex shader feeding a geometry shader (entry "MainForGS") still
                    // carries a position export, but its D3D output signature has no
                    // SV_Position: the next stage consumes only the parameter exports.
                    self.line("// (position export with no SV_Position output element: ignored)");
                    return Ok(());
                }
            }
            }
        } else if (32..64).contains(&target) {
            let p = (target - 32) as usize;
            if p < self.out_params.len() {
                let reg = self.out_params[p];
                for (ei, e) in osig.iter().enumerate() {
                    if e.sysval == 0 && e.register == reg {
                        let sw: String = (0..4).filter(|c| (e.mask >> c) & 1 != 0).map(|c| XYZW[c]).collect();
                        assigns.push((ei, sw));
                    }
                }
            } else if self.is_ms() && p - self.out_params.len() < self.prim_params.len() {
                let reg = self.prim_params[p - self.out_params.len()];
                let mut plines = Vec::new();
                for (ei, e) in self.ex.prim_sig.iter().enumerate() {
                    if e.sysval == 0 && e.register == reg {
                        let sw: String = (0..4).filter(|c| (e.mask >> c) & 1 != 0).map(|c| XYZW[c]).collect();
                        let (ty, _) = sig_type(e);
                        let val = match (e.comp_type, &raw) {
                            (1, Some(rc)) => format!("uint4({})", rc.join(", ")),
                            (2, Some(rc)) => format!("int4(uint4({}))", rc.join(", ")),
                            (1 | 2, None) => return Err("integer output element fed by a packed export without a recognised pack epilogue".into()),
                            (_, Some(rc)) if compr => format!("float4({})", rc.iter().map(|x| format!("asfloat({x})")).collect::<Vec<_>>().join(", ")),
                            _ => v4.clone(),
                        };
                        plines.push(format!("if (exec_lo & 1u) tb_pout.p{ei} = ({ty})({val}.{sw});"));
                    }
                }
                for l in plines {
                    self.line(&l);
                }
                return Ok(());
            } else {
                return Err(format!("export target {target} beyond output parameters"));
            }
        } else {
            return Err(format!("export target {target} unsupported"));
        }
        let mut lines = vec![];
        for (ei, sw) in assigns {
            let (ty, _) = sig_type(&osig[ei]);
            if matches!(sw.as_str(), "layer" | "view") {
                let z = match &raw {
                    Some(rc) => rc[2].clone(),
                    None => return Err("position export 13 packs SV_ViewportArrayIndex into the layer dword, which this export does not carry unpacked".into()),
                };
                let expr = if sw == "layer" { format!("({z}) & 0x7ffu") } else { format!("(({z}) >> 16) & 0xfu") };
                lines.push(format!("if (exec_lo & 1u) o.o{ei} = ({ty})({expr});"));
                continue;
            }
            // Integer elements carry the register bits themselves; converting them through
            // float (or half) would destroy them.
            let val = match (osig[ei].comp_type, &raw) {
                (1, Some(rc)) => format!("uint4({})", rc.join(", ")),
                (2, Some(rc)) => format!("int4(uint4({}))", rc.join(", ")),
                (1 | 2, None) => return Err("integer output element fed by a packed export without a recognised pack epilogue".into()),
                (_, Some(rc)) if compr && is_ps => format!(
                    "float4({})",
                    rc.iter().enumerate().map(|(c, x)| format!("tb_rtfmt(asfloat({x}), TB_RTFMT{}, {c}u)", osig[ei].index)).collect::<Vec<_>>().join(", ")
                ),
                (_, Some(rc)) if compr => format!("float4({})", rc.iter().map(|x| format!("asfloat({x})")).collect::<Vec<_>>().join(", ")),
                _ if compr && is_ps => format!(
                    "float4({})",
                    comp.iter().enumerate().map(|(c, x)| format!("tb_rtfmt({x}, TB_RTFMT{}, {c}u)", osig[ei].index)).collect::<Vec<_>>().join(", ")
                ),
                _ => v4.clone(),
            };
            lines.push(format!("if (exec_lo & 1u) o.o{ei} = ({ty})({val}.{sw});"));
        }
        for l in lines {
            self.line(&l);
        }
        Ok(())
    }

    fn reg_decls(&self) -> String {
        let is_v = |r: &str| r.len() > 1 && r.starts_with('v') && r[1..].bytes().all(|b| b.is_ascii_digit());
        let mut out = String::new();
        let scalars: Vec<String> =
            self.regs.iter().filter(|r| !(self.vreg_array && is_v(r))).map(|r| format!("{r} = 0u")).collect();
        if !scalars.is_empty() {
            out.push_str(&format!("    uint {};", scalars.join(", ")));
        }
        if self.vreg_array {
            let all: Vec<String> = (0..256).map(|n| format!("v{n} = 0u")).collect();
            out.push_str(&format!("\n    uint {};", all.join(", ")));
        }
        out
    }

    fn finish(&mut self, is_ps: bool) -> Result<String, String> {
        let dg_init = self.dg_sgpr_init();
        let static_init = self.static_sgpr_init();
        let ex = self.ex;
        let mut h = String::new();
        let _ = writeln!(h, "// Generated by tombox-xshader from RDNA2 microcode ({:?}, {} instrs).", ex.stage, self.insns.len());
        h.push_str(CUBE_HELPERS);
        if self.uses_f64 {
            h.push_str(F64_HELPERS);
        }
        if self.uses_d16 {
            h.push_str(mtbuf::D16_HELPERS);
        }
        if self.uses_trans16 {
            h.push_str(&crate::trans16::hlsl());
        }
        if self.uses_trans32 {
            h.push_str(&crate::trans32::hlsl());
        }
        if self.uses_global {
            h.push_str(&global::helpers());
        }
        if self.uses_scratch {
            h.push_str(&scratch::helpers());
        }
        if self.uses_bvh {
            h.push_str(&BVH_HELPERS.replace("TB_GMEM_SPACE", &TB_GMEM_SPACE.to_string()));
        }
        for id in &self.hw_regs {
            let _ = writeln!(h, "static uint tb_hw{id} = {:#x}u;", if *id == 1 { self.init_mode } else { 0 });
        }
        if self.mode_use.sp || self.mode_use.dp || mode::uses_mad(&self.insns) {
            h.push_str(mode::MAD_HELPERS);
        }
        if self.mode_use.sp || self.mode_use.dp {
            h.push_str(mode::MODE_HELPERS);
            if self.uses_f64 {
                h.push_str(mode::MODE_F64_HELPERS);
            }
        }
        let mut used: Vec<(String, u8, usize)> = Vec::new();
        for n in 0..self.bl.len() {
            let _ = writeln!(h, "static uint tb_bi{n};");
        }
        for (slot, dwords) in &self.cb_size {
            let nv = dwords.div_ceil(4).max(1);
            if slot & BINDLESS == 0 {
                let _ = writeln!(h, "cbuffer CB{slot} : register({}) {{ uint4 cb{slot}[{nv}]; }};", Self::reg_of('b', *slot));
            } else {
                let _ = writeln!(h, "struct TbBlCb{slot} {{ uint4 v[{nv}]; }};");
                let d = self.decl(&mut used, &format!("ConstantBuffer<TbBlCb{slot}>"), &format!("cb{slot}_"), *slot, 'b', 2, 32)?;
                h.push_str(&d);
                let _ = writeln!(h, "#define cb{slot} cb{slot}_.v");
            }
        }
        if self.uses_smem_bounds {
            h.push_str(smem_bounds::HELPERS);
        }
        for (slot, dim) in &self.tex {
            let ty = match dim {
                0 => "Texture1D",
                4 => "Texture1DArray",
                1 => "Texture2D",
                2 => "Texture3D",
                3 => "TextureCube",
                5 => "Texture2DArray",
                6 => "Texture2DMS",
                _ => return Err(format!("texture dim {dim} unsupported")),
            };
            let e = match self.img_ty.get(&(false, *slot)) {
                Some(2) => "int4",
                Some(_) => "uint4",
                None => "float4",
            };
            h.push_str(&self.decl(&mut used, &format!("{ty}<{e}>"), &format!("tex{slot}"), *slot, 't', 0, 32)?);
            if self.tex_mips.contains(slot) {
                let _ = writeln!(h, "{ty}<{e}> tex{slot}_m[16] : register(t{}, space{});", (slot & 0xffff) * 16, TB_MIP_SPACE + (slot >> 16));
            }
        }
        for slot in &self.samp {
            h.push_str(&self.decl(&mut used, "SamplerState", &format!("samp{slot}"), *slot, 's', 3, 16)?);
        }
        for slot in &self.samp_cmp {
            if self.samp.contains(slot) {
                return Err(format!("sampler s{slot} used both as a regular and a comparison sampler"));
            }
            h.push_str(&self.decl(&mut used, "SamplerComparisonState", &format!("scmp{slot}"), *slot, 's', 3, 16)?);
        }
        for (is_uav, slot) in self.sbuf.keys() {
            if *is_uav && self.sbuf_u64.contains(slot) {
                let gc = if self.is_coherent(*slot) { "globallycoherent " } else { "" };
                h.push_str(&self.decl(&mut used, &format!("{gc}RWStructuredBuffer<uint64_t>"), &format!("ubuf{slot}"), *slot, 'u', 1, 32)?);
                continue;
            }
            let (ty, nm, reg) = match (*is_uav, self.is_coherent(*slot)) {
                (true, true) => ("globallycoherent RWStructuredBuffer", "ubuf", 'u'),
                (true, false) => ("RWStructuredBuffer", "ubuf", 'u'),
                _ => ("StructuredBuffer", "sbuf", 't'),
            };
            if self.buf_is_raw(*is_uav, *slot) {
                let raw_ty = match (*is_uav, self.is_coherent(*slot)) {
                    (true, true) => "globallycoherent RWByteAddressBuffer",
                    (true, false) => "RWByteAddressBuffer",
                    _ => "ByteAddressBuffer",
                };
                h.push_str(&self.decl(&mut used, raw_ty, &format!("{nm}{slot}"), *slot, reg, if *is_uav { 1 } else { 0 }, 32)?);
                if self.sbuf_flat.contains(&(*is_uav, *slot)) {
                    let _ = writeln!(h, "uint tb_sbw_{nm}{slot}(uint off) {{ return {nm}{slot}.Load(off & ~3u); }}");
                }
                continue;
            }
            let _ = writeln!(h, "struct TbElem_{nm}{slot} {{ uint w[{}]; }};
", self.sbuf_words((*is_uav, *slot)));
            h.push_str(&self.decl(&mut used, &format!("{ty}<TbElem_{nm}{slot}>"), &format!("{nm}{slot}"), *slot, reg, if *is_uav { 1 } else { 0 }, 32)?);
            if self.sbuf_flat.contains(&(*is_uav, *slot)) {
                // Word at a byte offset: the element stride is a property of the view, not of
                // the shader, so it is queried.
                let _ = writeln!(h, "uint tb_sbw_{nm}{slot}(uint off) {{ uint n, st; {nm}{slot}.GetDimensions(n, st); st = max(st, 4u); uint e = off / st; return {nm}{slot}[e].w[(off - e * st) >> 2]; }}");
            }
        }
        for (is_uav, slot) in &self.tbuf {
            let gc = if self.is_coherent(*slot) { "globallycoherent " } else { "" };
            let _ = if *is_uav && self.tbuf_u64.contains(slot) {
                self.decl(&mut used, &format!("{gc}RWBuffer<uint64_t>"), &format!("utb{slot}"), *slot, 'u', 1, 32).map(|d| h.push_str(&d))
            } else if *is_uav && self.tbuf_uint.contains(slot) {
                self.decl(&mut used, &format!("{gc}RWBuffer<uint>"), &format!("utb{slot}"), *slot, 'u', 1, 32).map(|d| h.push_str(&d))
            } else {
                let e = match self.tbuf_ty.get(&(*is_uav, *slot)) {
                    Some(2) => "int4",
                    Some(_) => "uint4",
                    None => "float4",
                };
                if *is_uav {
                    self.decl(&mut used, &format!("{gc}RWBuffer<{e}>"), &format!("utb{slot}"), *slot, 'u', 1, 32).map(|d| h.push_str(&d))
                } else {
                    self.decl(&mut used, &format!("Buffer<{e}>"), &format!("stb{slot}"), *slot, 't', 0, 32).map(|d| h.push_str(&d))
                }
            };
        }
        for (slot, dim) in &self.uav {
            // Textures used with image atomics hold one uint per texel.
            let e = match self.img_ty.get(&(true, *slot)) {
                Some(2) => "int4",
                Some(_) => "uint4",
                None => "float4",
            };
            let elem = if self.uav_u64.contains(slot) {
                "uint64_t"
            } else if self.uav_uint.contains(slot) {
                "uint"
            } else {
                e
            };
            let ty = match dim {
                0 => format!("RWTexture1D<{elem}>"),
                4 => format!("RWTexture1DArray<{elem}>"),
                1 => format!("RWTexture2D<{elem}>"),
                2 => format!("RWTexture3D<{elem}>"),
                5 => format!("RWTexture2DArray<{elem}>"),
                _ => return Err(format!("UAV dim {dim} unsupported")),
            };
            let ty = if self.is_coherent(*slot) { format!("globallycoherent {ty}") } else { ty };
            h.push_str(&self.decl(&mut used, &ty, &format!("uav{slot}"), *slot, 'u', 1, 32)?);
            if self.uav_mips.contains(slot) {
                let _ = writeln!(h, "{ty} uav{slot}_m[16] : register(u{}, space{});", (slot & 0xffff) * 16, TB_MIP_SPACE + (slot >> 16));
            }
        }
        if self.uses_code {
            // Constant tables embedded after the program in the code section. Code pointers
            // are byte offsets from the program start, so readers subtract the program
            // length (in dwords) before indexing. Only the tail is emitted: FXC allows at
            // most 4096 indexable literals.
            let words: Vec<String> = ex
                .isa_data
                .get(ex.isa.len()..)
                .unwrap_or(&[])
                .chunks_exact(4)
                .map(|c| format!("{:#x}", u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
                .collect();
            let _ = writeln!(h, "static const uint tb_code[{}] = {{ {} }};", words.len().max(1), if words.is_empty() { "0".to_string() } else { words.join(", ") });
        }
        let lds64 = self.lds64_dwords()?;
        if self.is_cs && self.uses_lds {
            match lds64 {
                Some(n) => {
                    let _ = writeln!(h, "groupshared uint tb_lds[{n}];
groupshared uint tb_lds64_lock; // held by every 64-bit LDS atomic: they are serialised against each other only, not against a 32-bit atomic or a plain write to the same dwords");
                }
                None => {
                    let _ = writeln!(h, "groupshared uint tb_lds[8192];");
                }
            }
        }
        if let Some(x) = self.xlane_header()? {
            let _ = writeln!(h, "{x}");
        }
        if self.body.contains("tb_gws_counter_first") {
            let [_, x, y, z] = ex.isa_preamble;
            let _ = writeln!(h, "groupshared uint tb_gws_counter_first[{}];", (x * y * z).div_ceil(32));
        }
        if let Some(x) = gds::gds_header(&self.body) {
            let _ = writeln!(h, "{x}");
        }
        if let Some(x) = self.rm_header() {
            let _ = write!(h, "{x}");
        }
        if let Some(x) = self.ud_header() {
            h.push_str(&x);
        }
        if let Some(x) = centroid::header(ex) {
            h.push_str(&x);
        }
        if self.is_ms() {
            return self.finish_mesh(h, &static_init);
        }
        if self.is_as() {
            return self.finish_amp(h, &static_init);
        }
        if self.is_cs {
            // Compute ABI (from ClearCS + FSSM metadata): local thread id in v0..v2, the
            // thread-group id in the three SGPRs following the user-data (root-parameter)
            // registers, thread-group size in the ISA preamble dwords 1..3.
            let p = ex.isa_preamble;
            let (tx, ty_, tz) = if p[1] != 0 || p[2] != 0 || p[3] != 0 {
                (p[1].max(1), p[2].max(1), p[3].max(1))
            } else {
                ex.psv_numthreads.map(|t| (t[0], t[1], t[2])).unwrap_or((1, 1, 1))
            };
            let nparams = ex.root_sig.as_ref().map(|r| r.params.len()).unwrap_or(4);
            let g = self.cs_tgid_base(nparams);
            let _ = writeln!(h, "[numthreads({tx}, {ty_}, {tz})]
void main(uint3 gid : SV_GroupID, uint3 gtid : SV_GroupThreadID) {{");
            let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
            for r in ["v0", "v1", "v2"] {
                self.regs.insert(r.to_string());
            }
            for k in 0..3 {
                self.regs.insert(format!("s{}", g + k));
            }
            let _ = writeln!(h, "{}", self.reg_decls());
            h.push_str(&self.ud_init());
            let _ = writeln!(h, "    v0 = gtid.x; v1 = gtid.y; v2 = gtid.z;");
            let _ = writeln!(h, "    s{} = gid.x; s{} = gid.y; s{} = gid.z;", g, g + 1, g + 2);
            h.push_str(&dg_init);
            h.push_str(&static_init);
            if lds64.is_some() {
                h.push_str("    tb_lds64_lock = 0u;
    GroupMemoryBarrierWithGroupSync();
");
            }
            h.push_str(&self.body);
            h.push_str("}
");
            return Ok(h);
        }
        let _ = writeln!(h, "struct SIn {{");
        let sysgen = |e: &SigElement| is_ps && !matches!(e.sysval, 0..=5);
        let pos_field = match ex.input_sig.iter().position(|e| e.sysval == 1) {
            Some(ei) => format!("i.i{ei}"),
            None => "i.tb_pos".to_string(),
        };
        for pass in 0..2 {
            if pass == 1 && is_ps && self.uses_quad && pos_field == "i.tb_pos" {
                let _ = writeln!(h, "    float4 tb_pos : SV_Position;");
            }
            if pass == 1 {
                h.push_str(&centroid::sys_fields(ex, pos_field == "i.tb_pos" && !self.uses_quad));
            }
            for (ei, e) in ex.input_sig.iter().enumerate() {
                if sysgen(e) != (pass == 1) {
                    continue;
                }
                let (ty, _) = sig_type(e);
                let interp = if is_ps { ps_input_modifier(ex, ei, e) } else { "" };
                let _ = writeln!(h, "    {interp}{ty} i{ei} : {};", semantic(e));
            }
        }
        let _ = writeln!(h, "}};\nstruct SOut {{");
        for (ei, e) in ex.output_sig.iter().enumerate() {
            let (ty, _) = sig_type(e);
            if matches!(e.sysval, 65 | 67 | 68) {
                let _ = writeln!(h, "    float od : {};", semantic(e).trim_end_matches('0'));
            } else if e.sysval == 1 && !is_ps {
                // `precise`: no fast-math on the position computation. The title relies on a
                // depth-only pass and the base pass producing identical depth (the base pass
                // tests without writing); the two are separate shaders here, and without this
                // the compiler may contract or reorder them differently (observed as holes
                // in the character where the base pass failed the depth test).
                let _ = writeln!(h, "    precise {ty} o{ei} : {};", semantic(e));
            } else {
                let interp = if is_ps { None } else { self.out_interp.for_output(e) };
                let interp = interp.map(|m| format!("{m} ")).unwrap_or_default();
                let _ = writeln!(h, "    {interp}{ty} o{ei} : {};", semantic(e));
            }
        }
        let no_out = ex.output_sig.is_empty();
        if no_out {
            // No outputs (e.g. depth-only / kill-only pixel shader): HLSL forbids empty structs.
            let cut = h.rfind("struct SOut {").unwrap_or(h.len());
            h.truncate(cut);
            let _ = writeln!(h, "void main(SIn i) {{");
        } else if self.is_gs {
            let (nv, _, _, prim, stream) = self.gs_dims()?;
            let _ = writeln!(
                h,
                "}};\n[maxvertexcount({})]\nvoid main({prim} SIn i[{nv}], inout {stream}<SOut> stream) {{",
                self.gs_max_vertices()
            );
        } else if self.is_ds() {
            let _ = write!(h, "}};\n{}", self.ds_entry()?);
        } else {
            let _ = writeln!(h, "}};\nSOut main(SIn i) {{\n    SOut o = (SOut)0;");
        }
        let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
        // Vertex attributes arrive after the fetch call in consecutive groups of four VGPRs
        // from v4, one group per input register the shader reads. A register none of whose
        // elements is read (read mask 0 in the input signature) gets no group: in the skinned
        // depth-only shader d15c18830a4616f9 ATTRIBUTE1/2 (tangents) are unread and the bone
        // indices of ATTRIBUTE3 arrive in v8..v11, not v16..v19.
        let mut init = self.ud_init();
        if self.is_gs {
            // GS entry ABI (from WriteToSliceMainGS): s3 carries the wave's primitive count in
            // bits [15:8]; v0/v1 hold the vertex offsets (v0 = offset0 | offset1 << 16, v1 =
            // offset2), in dwords. One invocation = one primitive.
            let (nv, in_dw, out_dw, _, _) = self.gs_dims()?;
            let _ = writeln!(init, "    uint tb_in[{nv}][{in_dw}]; uint tb_gs[{}]; uint tb_emit = 0u;", out_dw * self.gs_max_vertices());
            let _ = writeln!(init, "    {{ for (uint a = 0u; a < {}u; a++) tb_gs[a] = 0u; }}", out_dw * self.gs_max_vertices());
            let _ = writeln!(init, "    {{ for (uint k = 0u; k < {nv}u; k++) for (uint a = 0u; a < {in_dw}u; a++) tb_in[k][a] = 0u; }}");
            for k in 0..nv {
                for (ei, e) in ex.input_sig.iter().enumerate() {
                    let mut n = 0;
                    for c in 0..4usize {
                        if (e.mask >> c) & 1 != 0 {
                            let acc = format!("i[{k}].i{ei}.{}", XYZW[n]);
                            let val = if e.comp_type == 3 { format!("asuint({acc})") } else { format!("(uint)({acc})") };
                            let _ = writeln!(init, "    tb_in[{k}][{}] = {val};", e.register as usize * 4 + c);
                            n += 1;
                        }
                    }
                }
            }
            for rname in ["s3", "v0", "v1"] {
                self.regs.insert(rname.to_string());
            }
            let _ = if nv <= 3 {
                writeln!(init, "    s3 = 0x100u; v0 = 0x04000000u; v1 = 0x800u;")
            } else {
                let pack = |a: u32, b: u32| (a * 0x400) | ((b * 0x400) << 16);
                if nv > 4 {
                    self.regs.insert("v2".to_string());
                    writeln!(init, "    s3 = 0x100u; v0 = {:#x}u; v1 = {:#x}u; v2 = {:#x}u;", pack(0, 1), pack(2, 3), pack(4, 5))
                } else {
                    writeln!(init, "    s3 = 0x100u; v0 = {:#x}u; v1 = {:#x}u;", pack(0, 1), pack(2, 3))
                }
            };
        }
        if self.is_ds() {
            let ds_init = self.ds_init();
            init.push_str(&ds_init);
        }
        if !is_ps && !self.is_gs && !self.is_ds() {
            let is_durango = self.durango.is_some() || crate::gcn::is_durango(&self.ex.fssm);
            let merged = ex.stage == Stage::Vertex && self.merged_es();
            if merged {
                self.regs.insert("s3".to_string());
                let _ = writeln!(init, "    s3 = {}u;", if self.wave64 { "0x40" } else { "0x20" });
            }
            let attr_base = if merged { 9 } else { 4 };
            let mut group = 0usize;
            for reg in distinct_regs(ex.input_sig.iter().filter(|e| e.sysval == 0)) {
                let read = ex.input_sig.iter().any(|e| e.sysval == 0 && e.register == reg && (e.rw_mask != 0 || (is_durango && e.mask != 0)));
                if ex.stage == Stage::Vertex && !read {
                    continue;
                }
                let first = if ex.stage == Stage::Vertex { attr_base + 4 * group } else { 4 + 4 * reg as usize };
                group += 1;
                for c in 0..4usize {
                    let v = format!("v{}", first + c);
                    self.regs.insert(v.clone());
                    let val = match sig_lookup(&ex.input_sig, reg, c) {
                        Some((ei, k)) => {
                            let acc = format!("i.i{ei}.{}", XYZW[k]);
                            if ex.input_sig[ei].comp_type == 3 { format!("asuint({acc})") } else { format!("(uint)({acc})") }
                        }
                        // Unsupplied components default to (0,0,0,1) like the hardware fetch.
                        None => (if c == 3 { "0x3f800000u" } else { "0u" }).to_string(),
                    };
                    let _ = writeln!(init, "    {v} = {val};");
                }
            }
            if ex.stage == Stage::Vertex && (merged || !self.vs_lds_out) && (self.durango.is_none() || dg_vertex_id_on()) {
                for (ei, e) in ex.input_sig.iter().enumerate() {
                    let v = match (e.sysval, merged) {
                        (6, false) => "v0",
                        (8, false) => "v3",
                        (6, true) => "v5",
                        (8, true) => "v8",
                        _ => continue,
                    };
                    if !self.live.iter().any(|l| l == v) {
                        continue;
                    }
                    self.regs.insert(v.to_string());
                    let _ = writeln!(init, "    {v} = (uint)(i.i{ei});");
                }
            }
        }
        if !self.regs.is_empty() {
            let _ = writeln!(h, "{}", self.reg_decls());
        }
        if is_ps && self.uses_quad {
            let _ = writeln!(init, "    uint tb_quad = ((uint){pos_field}.x & 1u) | (((uint){pos_field}.y & 1u) << 1);");
        }
        if is_ps {
            // PS entry ABI (observed): v0/v1 = perspective-centre barycentrics, then the
            // fragment position x, y, z, 1/w in v2..v5 when the shader reads it.
            if let Some(ei) = ex.input_sig.iter().position(|e| e.sysval == 1) {
                let p = format!("i.i{ei}");
                let exprs = [format!("{p}.x"), format!("{p}.y"), format!("{p}.z"), format!("1.0 / {p}.w")];
                for (k, expr) in exprs.iter().enumerate() {
                    let v = format!("v{}", 2 + k);
                    if self.live.contains(&v) {
                        let _ = writeln!(init, "    {v} = asuint({expr});");
                    }
                }
            }
        }
        init.push_str(&dg_init);
        init.push_str(&static_init);
        let (init, body) = centroid::lower(ex, &pos_field, &init, &self.body);
        h.push_str(&init);
        h.push_str(&body);
        if guard_mark() && !no_out && !self.is_gs && body.contains("uint tb_guard = 0u;") && h.contains("float4 o0 : SV_Target0;") {
            let _ = writeln!(h, "    if (tb_guard >= {}u) o.o0 = float4(1000.0, 0.0, 1000.0, 1.0);", loop_guard(1000000).saturating_sub(1));
        }
        if is_ps && !no_out && !self.is_gs {
            h.push_str(&ps_clamp_lines(&ex.output_sig));
        }
        h.push_str(if no_out || self.is_gs { "}\n" } else { "    return o;\n}\n" });
        Ok(h)
    }
}

fn f16_native() -> bool {
    static N: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("TOMBOX_XS_F16_NATIVE").map(|v| v.trim() == "1").unwrap_or(false))
}

fn f32_native() -> bool {
    static N: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("TOMBOX_XS_F32_NATIVE").map(|v| v.trim() == "1").unwrap_or(false))
}

fn f64_native() -> bool {
    static N: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("TOMBOX_XS_F64_NATIVE").map(|v| v.trim() == "1").unwrap_or(false))
}

#[derive(Default)]
struct Structure {
    regions: BTreeMap<usize, (usize, Option<usize>)>,
    breaks: BTreeSet<usize>,
    elses: BTreeSet<usize>,
}

fn structure_enabled() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("TOMBOX_XLAT_STRUCTURE").map(|v| v.trim() != "0").unwrap_or(true))
}

fn plan_structure(fwd: &HashMap<usize, usize>, loops: &[(usize, usize)], cont: &BTreeSet<usize>, is_cond: impl Fn(usize) -> bool, is_jump: impl Fn(usize) -> bool) -> Structure {
    let mut st = Structure::default();
    let mut edges: Vec<(usize, usize)> = fwd.iter().map(|(&b, &t)| (b, t)).collect();
    edges.sort_unstable();
    let entered = |s: usize, e: usize, own: (usize, usize)| edges.iter().any(|&(b, t)| t >= s && t < e && !(b >= s && b < e) && (b, t) != own);
    let loop_ok = |i: usize, t: usize| loops.iter().all(|&(l, b)| if l <= i && i <= b { t <= b } else if i < l { t <= l || t > b } else { true });
    let nest = |acc: &[(usize, usize)], i: usize, t: usize| {
        acc.iter().all(|&(a, at)| {
            let (lt, hi, ht) = if a < i { (at, i, t) } else { (t, a, at) };
            hi >= lt || ht <= lt
        })
    };
    let mut acc: Vec<(usize, usize)> = vec![];
    for &(i, t) in &edges {
        if cont.contains(&i) || !is_cond(i) || st.elses.contains(&i) {
            continue;
        }
        let inner = loops.iter().filter(|&&(l, b)| l <= i && i < b).max_by_key(|&&(l, _)| l);
        if let Some(&(_, b)) = inner {
            if t == b + 1 {
                st.breaks.insert(i);
                continue;
            }
        }
        if !loop_ok(i, t) || !nest(&acc, i, t) || entered(i + 1, t, (i, t)) {
            continue;
        }
        let j = t - 1;
        if j > i && is_jump(j) && !cont.contains(&j) {
            if let Some(&t2) = fwd.get(&j) {
                if t2 > t && loop_ok(j, t2) && nest(&acc, j, t2) && !entered(t, t2, (i, t)) {
                    st.regions.insert(i, (t2, Some(t)));
                    st.elses.insert(j);
                    acc.push((i, t));
                    acc.push((j, t2));
                    continue;
                }
            }
        }
        st.regions.insert(i, (t, None));
        acc.push((i, t));
    }
    st
}

pub fn loop_guard(default: u32) -> u32 {
    static G: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    G.get_or_init(|| std::env::var("TOMBOX_XS_LOOP_GUARD").ok().and_then(|v| v.trim().parse().ok()).filter(|&n: &u32| n > 0)).map_or(default, |n| n.min(default))
}

pub fn guard_mark() -> bool {
    static M: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("TOMBOX_XS_GUARD_MARK").map(|v| v == "1").unwrap_or(false))
}
