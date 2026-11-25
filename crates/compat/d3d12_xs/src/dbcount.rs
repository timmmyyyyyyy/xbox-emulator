use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use compat_common::record;

use crate::cl2_state::VariantKey;
use crate::device_wrap::Guid;

#[path = "dbcount_strip.rs"]
pub mod strip;

pub const X_RESET: usize = 10;
pub const X_DRAW: usize = 12;
pub const X_DRAW_INDEXED: usize = 13;
pub const X_PSO: usize = 25;
pub const X_EXEC_BUNDLE: usize = 27;
pub const X_BEGIN_QUERY: usize = 28;
pub const X_END_QUERY: usize = 29;
pub const X_RESOLVE_QUERY: usize = 30;
pub const X_RT: usize = 50;
pub const X_EXEC_INDIRECT: usize = 59;
pub const X_SET_DB_COUNT_CONTROL: usize = 64;
pub const X_SET_PERFECT_ZPASS: usize = 65;
pub const X_DRAW_INDEXED_X: usize = 81;
pub const X_DISPATCH_MESH_X: usize = 98;
pub const X_DISPATCH_MESH: usize = 127;

pub const DEFAULT_CONTROL: u32 = 0x1100_0100;
pub const RATE_MASK: u32 = 0x70;
pub const RATE_AUTO_NIBBLE: u32 = 0x70;
pub const RESET_KEEP: u32 = 0xffff_ff8d;
pub const PERFECT: u32 = 0x2;
pub const DB_COUNT_CONTROL_REG: u32 = 0xa001;

const PC_DRAW: usize = 12;
const PC_DRAW_INDEXED: usize = 13;
const PC_DISPATCH: usize = 14;
const PC_COPY_BUFFER: usize = 15;
const PC_SET_PSO: usize = 25;
const PC_BARRIER: usize = 26;
const PC_SET_CROOT: usize = 29;
const PC_SET_CCONSTS: usize = 35;
const PC_SET_CUAV: usize = 41;
const PC_SO_TARGETS: usize = 45;
const PC_BEGIN_QUERY: usize = 52;
const PC_END_QUERY: usize = 53;
const PC_RESOLVE_QUERY: usize = 54;
const CHILD_GET_DEVICE: usize = 7;
const DEV_CREATE_GFX_PSO: usize = 10;
const DEV_CREATE_COMPUTE_PSO: usize = 11;
const DEV_CREATE_ROOT_SIGNATURE: usize = 16;
const DEV_INCREMENT_SIZE: usize = 15;
const DEV_CREATE_DSV: usize = 21;
const DEV_COPY_SIMPLE: usize = 24;
const HEAP_TYPE_DSV: u32 = 3;
const DEV_CREATE_COMMITTED: usize = 27;
const DEV_CREATE_QUERY_HEAP: usize = 39;
const DEV2_CREATE_PIPELINE_STATE: usize = 47;
const RES_GET_VA: usize = 11;

const QUERY_OCCLUSION: u32 = 0;
const QUERY_BINARY: u32 = 1;
const ST_COMMON: u32 = 0;
const ST_UAV: u32 = 0x8;
const ST_COPY_DEST: u32 = 0x400;
const ST_COPY_SOURCE: u32 = 0x800;

pub const AUX_CHUNK: u32 = 4096;
pub const BLOCK_WORDS: u32 = 256;
pub const BLOCK_ENTRIES: u32 = BLOCK_WORDS - 2;
pub const BLOCKS_PER_CHUNK: u32 = 64;

const D_VS: usize = 0x08;
const D_PS: usize = 0x18;
const D_DS_STAGE: usize = 0x28;
const D_HS: usize = 0x38;
const D_GS: usize = 0x48;
const D_SO_ENTRIES: usize = 0x60;
const D_BLEND: usize = 0x78;
const D_DS: usize = 0x1f0;
const D_SAMPLE_COUNT: usize = 0x268;
const D_LEN: usize = 0x290;

const DS_DEPTH_ENABLE: usize = 0;
const DS_DEPTH_WRITE: usize = 4;
const DS_DEPTH_FUNC: usize = 8;
const DS_STENCIL_ENABLE: usize = 12;
const DS_STENCIL_WRITE_MASK: usize = 17;
const DS_FRONT: usize = 20;
const DS_BACK: usize = 36;
const DS_BOUNDS: usize = 52;
const DS_LEN: usize = 52;
const DS1_LEN: usize = 56;
const BLEND_RT_MASK: usize = 44;
const BLEND_RT_STRIDE: usize = 40;
const BLEND_LEN: usize = 328;
const FUNC_ALWAYS: u32 = 8;
const OP_KEEP: u32 = 1;

pub const SUB_VS: u32 = 1;
pub const SUB_PS: u32 = 2;
pub const SUB_DS: u32 = 3;
pub const SUB_HS: u32 = 4;
pub const SUB_GS: u32 = 5;
pub const SUB_SO: u32 = 7;
pub const SUB_BLEND: u32 = 8;
pub const SUB_DEPTH_STENCIL: u32 = 11;
pub const SUB_INPUT_LAYOUT: u32 = 12;
pub const SUB_SAMPLE_DESC: u32 = 17;
pub const SUB_CACHED_PSO: u32 = 19;
pub const SUB_DEPTH_STENCIL1: u32 = 21;
pub const SUB_VIEW_INSTANCING: u32 = 22;
pub const SUB_AS: u32 = 24;
pub const SUB_MS: u32 = 25;

const SV_RENDER_TARGET_ARRAY_INDEX: u32 = 4;

const IID_DEVICE: Guid = Guid { d1: 0x1898_19F1, d2: 0x1DB6, d3: 0x4B57, d4: [0xBE, 0x54, 0x18, 0x21, 0x33, 0x9B, 0x85, 0xF7] };
const IID_DEVICE2: Guid = Guid { d1: 0x30ba_a41e, d2: 0xb15b, d3: 0x475c, d4: [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28] };
const IID_PSO: Guid = Guid { d1: 0x765a_30f3, d2: 0xf624, d3: 0x4c6f, d4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45] };
const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a_6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };
const IID_RESOURCE: Guid = Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };
const IID_QUERY_HEAP: Guid = Guid { d1: 0x0d96_58ae, d2: 0xed45, d3: 0x469e, d4: [0xa6, 0x1d, 0x97, 0x0e, 0xc5, 0x83, 0xca, 0xb4] };

pub const COMBINE_HLSL: &str = r#"#define RS "RootConstants(num32BitConstants=14, b0), UAV(u0)"
cbuffer C : register(b0) { uint mode; uint acc; uint n; uint shl; uint shr; uint binary; uint o0; uint o1; uint o2; uint o3; int w0; int w1; int w2; int w3; };
RWByteAddressBuffer s : register(u0);
int64_t ld(uint o) { uint2 v = s.Load2(o); return (int64_t)(((uint64_t)v.y << 32) | (uint64_t)v.x); }
void st(uint o, int64_t x) { uint64_t u = (uint64_t)x; s.Store2(o, uint2((uint)u, (uint)(u >> 32))); }
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    if (mode == 2) { st(acc, 0); return; }
    if (mode == 0) {
        int64_t g = 0;
        if (n > 0) g += (int64_t)w0 * ld(o0);
        if (n > 1) g += (int64_t)w1 * ld(o1);
        if (n > 2) g += (int64_t)w2 * ld(o2);
        if (n > 3) g += (int64_t)w3 * ld(o3);
        g = (g << (int64_t)shl) >> (int64_t)shr;
        st(acc, ld(acc) + g);
        return;
    }
    int64_t t = ld(o0);
    int64_t a = ld(acc);
    int64_t pos = a > 0 ? a : 0;
    int64_t r = binary != 0 ? ((t != 0 || a > 0) ? 1 : 0) : t + pos;
    st(o0, r);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub zpass_increment_disable: bool,
    pub perfect_zpass_counts: bool,
    pub disable_conservative: bool,
    pub enhanced_conservative: bool,
    pub sample_rate: u32,
    pub zpass_enable: u32,
    pub zfail_enable: u32,
    pub sfail_enable: u32,
    pub dbfail_enable: u32,
    pub slice_even_enable: u32,
    pub slice_odd_enable: u32,
}

pub fn decode(v: u32) -> Fields {
    Fields {
        zpass_increment_disable: v & 1 != 0,
        perfect_zpass_counts: v & 2 != 0,
        disable_conservative: v & 4 != 0,
        enhanced_conservative: v & 8 != 0,
        sample_rate: (v >> 4) & 7,
        zpass_enable: (v >> 8) & 0xf,
        zfail_enable: (v >> 12) & 0xf,
        sfail_enable: (v >> 16) & 0xf,
        dbfail_enable: (v >> 20) & 0xf,
        slice_even_enable: (v >> 24) & 0xf,
        slice_odd_enable: (v >> 28) & 0xf,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListCount {
    pub control: u32,
    pub log_samples: u32,
    pub fixed_rate: bool,
    pub custom: bool,
}

impl Default for ListCount {
    fn default() -> Self {
        ListCount { control: DEFAULT_CONTROL, log_samples: 0, fixed_rate: false, custom: false }
    }
}

pub fn set_control(s: ListCount, ctrl: Option<u32>) -> ListCount {
    match ctrl {
        None => ListCount { control: DEFAULT_CONTROL, fixed_rate: false, custom: false, ..s },
        Some(v) if v & 0xf0 == RATE_AUTO_NIBBLE => ListCount { control: (v & !RATE_MASK) | ((s.log_samples << 4) & RATE_MASK), fixed_rate: false, custom: true, ..s },
        Some(v) => ListCount { control: v, fixed_rate: true, custom: true, ..s },
    }
}

pub fn set_perfect(s: ListCount, perfect: bool) -> Option<ListCount> {
    if s.custom || ((s.control & PERFECT) != 0) == perfect {
        return None;
    }
    Some(ListCount { control: (s.control & !PERFECT) | if perfect { PERFECT } else { 0 }, ..s })
}

pub fn sample_state(s: ListCount, log_samples: u32) -> ListCount {
    let control = if s.fixed_rate { s.control } else { (s.control & !RATE_MASK) | ((log_samples << 4) & RATE_MASK) };
    ListCount { control, log_samples, ..s }
}

pub fn reset(s: ListCount) -> ListCount {
    ListCount { control: s.control & RESET_KEEP, ..s }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter0 {
    pub zpass: bool,
    pub zfail: bool,
    pub sfail: bool,
    pub dbfail: bool,
    pub even: bool,
    pub odd: bool,
    pub disabled: bool,
    pub rate: u32,
    pub perfect: bool,
}

pub fn counter0(control: u32) -> Counter0 {
    let f = decode(control);
    Counter0 {
        zpass: f.zpass_enable & 1 != 0,
        zfail: f.zfail_enable & 1 != 0,
        sfail: f.sfail_enable & 1 != 0,
        dbfail: f.dbfail_enable & 1 != 0,
        even: f.slice_even_enable & 1 != 0,
        odd: f.slice_odd_enable & 1 != 0,
        disabled: f.zpass_increment_disable,
        rate: f.sample_rate,
        perfect: f.perfect_zpass_counts,
    }
}

pub fn zpass_only_at_rate(c: Counter0, log_samples: u32) -> bool {
    c.zpass && !c.zfail && !c.sfail && !c.dbfail && !c.disabled && c.even && c.odd && c.rate == log_samples
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DsState {
    pub depth_test: bool,
    pub depth_written: bool,
    pub stencil_test: bool,
    pub stencil_written: bool,
    pub bounds_test: bool,
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn put32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(d: &mut [u8], o: usize, v: u64) {
    d[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

pub fn ds_from_block(b: &[u8], bounds: bool) -> DsState {
    let depth = u32_at(b, DS_DEPTH_ENABLE) != 0;
    let stencil = u32_at(b, DS_STENCIL_ENABLE) != 0 && (u32_at(b, DS_FRONT + 12) != FUNC_ALWAYS || u32_at(b, DS_BACK + 12) != FUNC_ALWAYS);
    let ops = [DS_FRONT, DS_FRONT + 4, DS_FRONT + 8, DS_BACK, DS_BACK + 4, DS_BACK + 8];
    DsState {
        depth_test: depth && u32_at(b, DS_DEPTH_FUNC) != FUNC_ALWAYS,
        depth_written: depth && u32_at(b, DS_DEPTH_WRITE) != 0,
        stencil_test: stencil,
        stencil_written: stencil && b[DS_STENCIL_WRITE_MASK] != 0 && ops.iter().any(|&o| u32_at(b, o) != OP_KEEP),
        bounds_test: bounds,
    }
}

pub fn default_ds_block() -> [u8; DS1_LEN] {
    let mut b = [0u8; DS1_LEN];
    put32(&mut b, DS_DEPTH_ENABLE, 1);
    put32(&mut b, DS_DEPTH_WRITE, 1);
    put32(&mut b, DS_DEPTH_FUNC, 2);
    b[16] = 0xff;
    b[DS_STENCIL_WRITE_MASK] = 0xff;
    for f in [DS_FRONT, DS_BACK] {
        put32(&mut b, f, OP_KEEP);
        put32(&mut b, f + 4, OP_KEEP);
        put32(&mut b, f + 8, OP_KEEP);
        put32(&mut b, f + 12, FUNC_ALWAYS);
    }
    b
}

pub fn ds_from_desc(d: &[u8]) -> Option<DsState> {
    if d.len() < D_LEN {
        return None;
    }
    Some(ds_from_block(&d[D_DS..D_DS + DS_LEN], false))
}

pub fn log2_of(n: u32) -> u32 {
    31 - n.max(1).leading_zeros()
}

pub fn log2_samples(d: &[u8]) -> Option<u32> {
    if d.len() < D_LEN {
        return None;
    }
    Some(log2_of(u32_at(d, D_SAMPLE_COUNT)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Plan {
    pub z: i64,
    pub s: i64,
    pub b: i64,
    pub d: i64,
    pub shl: u32,
    pub shr: u32,
    pub order_dependent: bool,
    pub slice_unknown: bool,
}

impl Plan {
    pub fn pc_equivalent(&self) -> bool {
        self.z == 1 && self.s == 0 && self.b == 0 && self.d == 0 && self.shl == 0 && self.shr == 0
    }

    pub fn needs_replay(&self) -> bool {
        self.s != 0 || self.b != 0 || self.d != 0
    }
}

pub fn slice_gate(c: Counter0, slice: Option<u32>) -> bool {
    !c.disabled && match slice {
        Some(k) if k % 2 == 1 => c.odd,
        _ => c.even,
    }
}

pub fn plan(c: Counter0, ds: DsState, log_samples: u32, slice: Option<u32>) -> Plan {
    let slice_unknown = slice.is_none() && !c.disabled && c.even != c.odd;
    if !slice_gate(c, slice) {
        return Plan { slice_unknown, ..Plan::default() };
    }
    let (zp, zf, sf, df) = (c.zpass as i64, c.zfail as i64, c.sfail as i64, c.dbfail as i64);
    let mut w = [zp - zf, zf - sf, sf - df, df];
    let eff = [true, ds.depth_test, ds.stencil_test, ds.bounds_test];
    for i in (1..4).rev() {
        if !eff[i] {
            w[i - 1] += w[i];
            w[i] = 0;
        }
    }
    let (shl, shr) = if c.rate >= log_samples { (c.rate - log_samples, 0) } else { (0, log_samples - c.rate) };
    let order_dependent = (w[1] != 0 && ds.stencil_written) || (ds.bounds_test && ds.depth_written && (w[1] != 0 || w[2] != 0));
    Plan { z: w[0], s: w[1], b: w[2], d: w[3], shl, shr, order_dependent, slice_unknown }
}

pub fn zpass_fallback(p: Plan) -> Plan {
    Plan { z: 1, s: 0, b: 0, d: 0, ..p }
}

pub fn recordable(closed: bool) -> Option<&'static str> {
    if closed { Some("Cannot record commands into a command list that has already been closed.") } else { None }
}

fn parts(code: &[u8]) -> Option<Vec<(&[u8; 4], &[u8])>> {
    if code.len() < 32 || &code[0..4] != b"DXBC" {
        return None;
    }
    let n = u32_at(code, 28) as usize;
    let mut v = Vec::new();
    for i in 0..n {
        let po = 32 + i * 4;
        if po + 4 > code.len() {
            return None;
        }
        let off = u32_at(code, po) as usize;
        if off + 8 > code.len() {
            return None;
        }
        let size = u32_at(code, off + 4) as usize;
        let body = &code[off + 8..(off + 8 + size).min(code.len())];
        v.push((code[off..off + 4].try_into().unwrap(), body));
    }
    Some(v)
}

pub fn ps_has_uav(code: &[u8]) -> Option<bool> {
    let (_, p) = parts(code)?.into_iter().find(|(f, _)| *f == b"PSV0")?;
    if p.len() < 4 {
        return None;
    }
    let info = u32_at(p, 0) as usize;
    let mut o = 4 + info;
    if o + 4 > p.len() {
        return None;
    }
    let count = u32_at(p, o) as usize;
    o += 4;
    if count == 0 {
        return Some(false);
    }
    if o + 4 > p.len() {
        return None;
    }
    let stride = u32_at(p, o) as usize;
    o += 4;
    if stride < 4 || o + count * stride > p.len() {
        return None;
    }
    Some((0..count).any(|k| (6..=9).contains(&u32_at(p, o + k * stride))))
}

pub fn shader_kind(code: &[u8]) -> Option<u32> {
    let (_, p) = parts(code)?.into_iter().find(|(f, _)| *f == b"DXIL")?;
    (p.len() >= 4).then(|| u32_at(p, 0) >> 16)
}

pub fn writes_rt_array_index(code: &[u8]) -> Option<bool> {
    let ps = parts(code)?;
    let mut found = false;
    for (f, body) in ps {
        let (stride, sv) = match f {
            b"OSG1" | b"PSG1" => (32, 12),
            b"OSG5" => (28, 12),
            b"OSGN" => (24, 8),
            _ => continue,
        };
        if body.len() < 8 {
            return None;
        }
        let n = u32_at(body, 0) as usize;
        let base = u32_at(body, 4) as usize;
        if base + n * stride > body.len() {
            return None;
        }
        found |= (0..n).any(|k| u32_at(body, base + k * stride + sv) == SV_RENDER_TARGET_ARRAY_INDEX);
    }
    Some(found)
}

pub fn dsv_first_slice(desc: Option<&[u8]>) -> u32 {
    let Some(d) = desc.filter(|d| d.len() >= 24) else { return 0 };
    match u32_at(d, 4) {
        2 | 4 => u32_at(d, 16),
        6 => u32_at(d, 12),
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pass {
    StencilOnly,
    Coverage,
    Raster,
}

fn variant_ds(b: &mut [u8], pass: Pass) {
    put32(b, DS_DEPTH_WRITE, 0);
    put32(b, DS_DEPTH_FUNC, FUNC_ALWAYS);
    b[DS_STENCIL_WRITE_MASK] = 0;
    if pass != Pass::StencilOnly {
        put32(b, DS_STENCIL_ENABLE, 0);
    }
    if pass == Pass::Raster && b.len() >= DS1_LEN {
        put32(b, DS_BOUNDS, 0);
    }
}

fn mask_blend(b: &mut [u8]) {
    for i in 0..8 {
        b[BLEND_RT_MASK + i * BLEND_RT_STRIDE] = 0;
    }
}

pub fn variant_desc(d: &mut [u8], pass: Pass) -> bool {
    if d.len() < D_LEN {
        return false;
    }
    mask_blend(&mut d[D_BLEND..D_BLEND + BLEND_LEN]);
    variant_ds(&mut d[D_DS..D_DS + DS_LEN], pass);
    true
}

fn align(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

pub fn find_sub(subs: &[(u32, usize)], ty: u32) -> Option<usize> {
    subs.iter().find(|(t, _)| *t == ty).map(|&(_, o)| o)
}

pub fn append_sub(stream: &mut Vec<u8>, subs: &mut Vec<(u32, usize)>, ty: u32, body: &[u8]) -> usize {
    let (a, _) = crate::mesh_pso::inner_layout(ty).unwrap_or((4, body.len()));
    let at = align(stream.len(), 8);
    stream.resize(at, 0);
    stream.extend_from_slice(&ty.to_le_bytes());
    let inner = at + align(4, a);
    stream.resize(inner, 0);
    stream.extend_from_slice(body);
    let end = align(stream.len(), 8);
    stream.resize(end, 0);
    subs.push((ty, inner));
    inner
}

pub fn default_blend() -> Vec<u8> {
    let mut b = vec![0u8; BLEND_LEN];
    for i in 0..8 {
        let r = 8 + i * BLEND_RT_STRIDE;
        put32(&mut b, r + 8, 2);
        put32(&mut b, r + 12, 1);
        put32(&mut b, r + 16, 1);
        put32(&mut b, r + 20, 2);
        put32(&mut b, r + 24, 1);
        put32(&mut b, r + 28, 1);
        put32(&mut b, r + 32, 4);
        b[r + 36] = 0xf;
    }
    b
}

pub fn variant_stream(src: &[u8], subs: &[(u32, usize)], pass: Pass, ps: Option<&[u8]>) -> Vec<u8> {
    let mut s = src.to_vec();
    let mut subs = subs.to_vec();
    let ds = match (find_sub(&subs, SUB_DEPTH_STENCIL1), find_sub(&subs, SUB_DEPTH_STENCIL)) {
        (Some(o), _) => (o, DS1_LEN),
        (None, Some(o)) => (o, DS_LEN),
        (None, None) => (append_sub(&mut s, &mut subs, SUB_DEPTH_STENCIL, &default_ds_block()[..DS_LEN]), DS_LEN),
    };
    variant_ds(&mut s[ds.0..ds.0 + ds.1], pass);
    let bl = find_sub(&subs, SUB_BLEND).unwrap_or_else(|| append_sub(&mut s, &mut subs, SUB_BLEND, &default_blend()));
    mask_blend(&mut s[bl..bl + BLEND_LEN]);
    if let Some(o) = find_sub(&subs, SUB_CACHED_PSO) {
        s[o..o + 16].fill(0);
    }
    if let (Some(code), Some(o)) = (ps, find_sub(&subs, SUB_PS)) {
        put64(&mut s, o, code.as_ptr() as u64);
        put64(&mut s, o + 8, code.len() as u64);
    }
    s
}

pub struct StreamRec {
    pub stream: Vec<u8>,
    pub subs: Vec<(u32, usize)>,
    _shaders: Vec<Arc<Vec<u8>>>,
    _bufs: Vec<Vec<u8>>,
}

unsafe impl Send for StreamRec {}
unsafe impl Sync for StreamRec {}

pub fn snapshot_stream(stream: &[u8], shaders: &[Arc<Vec<u8>>]) -> Option<StreamRec> {
    let subs = crate::mesh_pso::parse(stream).ok()?;
    let mut s = stream.to_vec();
    let mut bufs = Vec::new();
    let mut snap = |s: &mut Vec<u8>, po: usize, no: usize, stride: usize, max: usize, name: Option<usize>| {
        let p = u64_at(s, po) as *const u8;
        let n = u32_at(s, no) as usize;
        if p.is_null() || n == 0 || n > max {
            put64(s, po, 0);
            put32(s, no, 0);
            return;
        }
        let mut v = unsafe { core::slice::from_raw_parts(p, n * stride).to_vec() };
        if let Some(no) = name {
            for k in 0..n {
                let np = u64_at(&v, k * stride + no) as *const core::ffi::c_char;
                if np.is_null() {
                    continue;
                }
                let c = unsafe { core::ffi::CStr::from_ptr(np) }.to_bytes_with_nul().to_vec();
                put64(&mut v, k * stride + no, c.as_ptr() as u64);
                bufs.push(c);
            }
        }
        put64(s, po, v.as_ptr() as u64);
        bufs.push(v);
    };
    for &(ty, o) in &subs {
        match ty {
            0 => {
                let rs = u64_at(&s, o) as *mut c_void;
                if !rs.is_null() {
                    let add: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(rs, 1)) };
                    unsafe { add(rs) };
                }
            }
            SUB_SO => {
                snap(&mut s, o, o + 8, 24, 64, Some(8));
                snap(&mut s, o + 16, o + 24, 4, 4, None);
            }
            SUB_INPUT_LAYOUT => snap(&mut s, o, o + 8, 32, 64, Some(0)),
            SUB_VIEW_INSTANCING => snap(&mut s, o + 8, o, 8, 4, None),
            _ => {}
        }
    }
    Some(StreamRec { stream: s, subs, _shaders: shaders.to_vec(), _bufs: bufs })
}

static STREAMS: Mutex<Option<HashMap<usize, Arc<StreamRec>>>> = Mutex::new(None);

pub fn note_stream_pso(pso: usize, stream: &[u8], shaders: &[Arc<Vec<u8>>]) {
    if let Some(r) = snapshot_stream(stream, shaders) {
        STREAMS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(pso, Arc::new(r));
    }
}

pub(crate) fn stream_rec(pso: usize) -> Option<Arc<StreamRec>> {
    STREAMS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&pso).cloned()
}

impl StreamRec {
    pub fn is_graphics(&self) -> bool {
        self.subs.iter().any(|&(t, o)| matches!(t, SUB_VS | SUB_MS) && u64_at(&self.stream, o) != 0 && u64_at(&self.stream, o + 8) != 0)
    }
}

#[derive(Clone)]
enum Src {
    Desc { desc: Vec<u8>, id: usize, _keep: Arc<dyn Send + Sync> },
    Stream(Arc<StreamRec>),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Info {
    pub ds: DsState,
    pub log: u32,
    pub stages: Vec<(u32, u64, u64)>,
    pub has_so: bool,
    pub view_instancing: bool,
}

pub fn info_from_desc(d: &[u8]) -> Option<Info> {
    let ds = ds_from_desc(d)?;
    let stages = [(SUB_VS, D_VS), (SUB_PS, D_PS), (SUB_DS, D_DS_STAGE), (SUB_HS, D_HS), (SUB_GS, D_GS)]
        .iter()
        .map(|&(t, o)| (t, u64_at(d, o), u64_at(d, o + 8)))
        .filter(|s| s.1 != 0 && s.2 != 0)
        .collect();
    Some(Info { ds, log: log2_samples(d)?, stages, has_so: u32_at(d, D_SO_ENTRIES) != 0, view_instancing: false })
}

pub fn info_from_stream(s: &[u8], subs: &[(u32, usize)]) -> Info {
    let ds = match (find_sub(subs, SUB_DEPTH_STENCIL1), find_sub(subs, SUB_DEPTH_STENCIL)) {
        (Some(o), _) => ds_from_block(&s[o..o + DS_LEN], u32_at(s, o + DS_BOUNDS) != 0),
        (None, Some(o)) => ds_from_block(&s[o..o + DS_LEN], false),
        (None, None) => ds_from_block(&default_ds_block()[..DS_LEN], false),
    };
    let log = find_sub(subs, SUB_SAMPLE_DESC).map(|o| log2_of(u32_at(s, o))).unwrap_or(0);
    let stages = subs
        .iter()
        .filter(|(t, _)| matches!(*t, SUB_VS | SUB_PS | SUB_DS | SUB_HS | SUB_GS | SUB_AS | SUB_MS))
        .map(|&(t, o)| (t, u64_at(s, o), u64_at(s, o + 8)))
        .filter(|s| s.1 != 0 && s.2 != 0)
        .collect();
    let has_so = find_sub(subs, SUB_SO).map(|o| u32_at(s, o + 8) != 0).unwrap_or(false);
    let view_instancing = find_sub(subs, SUB_VIEW_INSTANCING).map(|o| u32_at(s, o) > 1).unwrap_or(false);
    Info { ds, log, stages, has_so, view_instancing }
}

fn source(pso: usize) -> Option<Src> {
    if pso == 0 {
        return None;
    }
    if let Some((d, keep)) = crate::device_wrap::gfx_pso_desc(pso) {
        let id = crate::device_wrap::gfx_pso_identity(pso);
        let keep: Arc<dyn Send + Sync> = keep;
        return Some(Src::Desc { desc: d, id, _keep: keep });
    }
    stream_rec(pso).map(Src::Stream)
}

fn info_of(src: &Src) -> Option<Info> {
    match src {
        Src::Desc { desc, .. } => info_from_desc(desc),
        Src::Stream(r) => Some(info_from_stream(&r.stream, &r.subs)),
    }
}

fn identity(src: &Src) -> usize {
    match src {
        Src::Desc { id, .. } => *id,
        Src::Stream(r) => Arc::as_ptr(r) as usize,
    }
}

unsafe fn code<'a>(stage: (u32, u64, u64)) -> &'a [u8] {
    unsafe { core::slice::from_raw_parts(stage.1 as *const u8, stage.2 as usize) }
}

pub fn rtai_written(info: &Info) -> bool {
    info.stages.iter().filter(|s| s.0 != SUB_PS).any(|&s| writes_rt_array_index(unsafe { code(s) }) != Some(false))
}

#[derive(Default)]
pub struct SlotPool {
    next: u32,
    free: Vec<u32>,
    refs: HashMap<u32, u32>,
    held: HashMap<usize, Vec<u32>>,
    uses: HashMap<u32, HashMap<usize, u64>>,
    pending: Vec<(u32, Vec<(usize, u64)>)>,
}

impl SlotPool {
    pub fn take(&mut self, done: &dyn Fn(usize, u64) -> bool) -> u32 {
        let mut back = Vec::new();
        self.pending.retain(|(s, u)| {
            let ready = u.iter().all(|&(f, v)| done(f, v));
            if ready {
                back.push(*s);
            }
            !ready
        });
        self.free.extend(back);
        self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next - 1
        })
    }

    pub fn untake(&mut self, s: u32) {
        if !self.refs.contains_key(&s) {
            self.free.push(s);
        }
    }

    pub fn hold(&mut self, key: usize, s: u32) {
        *self.refs.entry(s).or_insert(0) += 1;
        self.held.entry(key).or_default().push(s);
    }

    pub fn addref(&mut self, s: u32) {
        *self.refs.entry(s).or_insert(0) += 1;
    }

    pub fn unref(&mut self, s: u32) {
        let Some(r) = self.refs.get_mut(&s) else { return };
        *r -= 1;
        if *r > 0 {
            return;
        }
        self.refs.remove(&s);
        let u: Vec<(usize, u64)> = self.uses.remove(&s).map(|m| m.into_iter().collect()).unwrap_or_default();
        if u.is_empty() {
            self.free.push(s);
        } else {
            self.pending.push((s, u));
        }
    }

    pub fn release(&mut self, key: usize) {
        for s in self.held.remove(&key).unwrap_or_default() {
            self.unref(s);
        }
    }

    pub fn holds_any(&self, keys: &[usize]) -> bool {
        keys.iter().any(|k| self.held.get(k).is_some_and(|v| !v.is_empty()))
    }

    pub fn note_exec(&mut self, keys: &[usize], fence: usize, value: u64) {
        for k in keys {
            for &s in self.held.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
                let e = self.uses.entry(s).or_default().entry(fence).or_insert(0);
                *e = (*e).max(value);
            }
        }
    }

    pub fn is_live(&self, s: u32) -> bool {
        self.refs.contains_key(&s)
    }

    pub fn is_pending(&self, s: u32) -> bool {
        self.pending.iter().any(|p| p.0 == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Group {
    pub aux: Vec<(u32, i64)>,
    pub shl: u32,
    pub shr: u32,
}

#[derive(Default)]
struct ListDb {
    count: ListCount,
    active: Option<(usize, u32, u32)>,
    groups: Vec<Group>,
    dsv: Option<Option<usize>>,
}

static PREV: [AtomicUsize; 128] = [const { AtomicUsize::new(0) }; 128];
static LISTS: Mutex<Option<HashMap<usize, ListDb>>> = Mutex::new(None);
static DONE: Mutex<Option<HashMap<(usize, u32), (u32, Vec<Group>)>>> = Mutex::new(None);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static LOG: AtomicUsize = AtomicUsize::new(0);

fn once(n: usize) -> bool {
    LOG.fetch_add(1, Ordering::Relaxed) < n
}

fn key(list: *mut c_void) -> usize {
    crate::list_split::origin_of(list)
}

fn with<R>(list: *mut c_void, f: impl FnOnce(&mut ListDb) -> R) -> R {
    let mut g = LISTS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new).entry(key(list)).or_default())
}

pub fn list_count(list: *mut c_void) -> ListCount {
    with(list, |l| l.count)
}

fn set_active(l: &mut ListDb, a: Option<(usize, u32, u32)>) {
    match (l.active.is_some(), a.is_some()) {
        (false, true) => {
            ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
        (true, false) => {
            ACTIVE.fetch_sub(1, Ordering::Relaxed);
        }
        _ => {}
    }
    l.active = a;
}

fn group_slots(groups: &[Group]) -> impl Iterator<Item = u32> + '_ {
    groups.iter().flat_map(|g| g.aux.iter().map(|a| a.0))
}

fn done<R>(f: impl FnOnce(&mut HashMap<(usize, u32), (u32, Vec<Group>)>) -> R) -> R {
    f(DONE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

fn drop_done(heap: usize, index: u32) {
    if let Some((_, old)) = done(|m| m.remove(&(heap, index))) {
        pools(|p| group_slots(&old).for_each(|s| p.aux.unref(s)));
    }
}

fn finish(heap: usize, index: u32, ty: u32, groups: Vec<Group>) {
    drop_done(heap, index);
    if groups.is_empty() {
        return;
    }
    pools(|p| group_slots(&groups).for_each(|s| p.aux.addref(s)));
    done(|m| m.insert((heap, index), (ty, groups)));
}

unsafe fn vslot(obj: *mut c_void, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

unsafe fn release(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(obj, 2)) };
    unsafe { f(obj) };
}

unsafe fn device_of(obj: *mut c_void) -> *mut c_void {
    let get: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(obj, CHILD_GET_DEVICE)) };
    let mut d: *mut c_void = core::ptr::null_mut();
    if unsafe { get(obj, &IID_DEVICE, &mut d) } < 0 || d.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { release(d) };
    d
}

fn prev(slot: usize) -> usize {
    PREV[slot].load(Ordering::Relaxed)
}

macro_rules! fwd {
    ($slot:expr, fn($($t:ty),*) -> $r:ty, $($a:expr),*) => {{
        let f: unsafe extern "system" fn(*mut c_void, $($t),*) -> $r = unsafe { core::mem::transmute(prev($slot)) };
        unsafe { f($($a),*) }
    }};
}

#[derive(Default)]
struct Pools {
    dev: usize,
    heaps: Vec<usize>,
    chunks: Vec<(usize, u64)>,
    aux: SlotPool,
    scratch: SlotPool,
}

static POOLS: Mutex<Option<Pools>> = Mutex::new(None);

fn pools<R>(f: impl FnOnce(&mut Pools) -> R) -> R {
    f(POOLS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Pools::default))
}

fn passed(f: usize, v: u64) -> bool {
    unsafe { crate::cl2_state::fence_passed(f, v) }
}

unsafe fn create_heap(dev: *mut c_void) -> Option<usize> {
    let qd = [QUERY_OCCLUSION, AUX_CHUNK, 0];
    let cqh: unsafe extern "system" fn(*mut c_void, *const u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_QUERY_HEAP)) };
    let mut heap: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { cqh(dev, qd.as_ptr(), &IID_QUERY_HEAP, &mut heap) };
    if hr < 0 || heap.is_null() {
        record("d3d12_xs", "dbcount", &format!("counting query heap FAILED hr={hr:#010x}"));
        return None;
    }
    Some(heap as usize)
}

unsafe fn create_chunk(dev: *mut c_void) -> Option<(usize, u64)> {
    let props: [u32; 5] = [1, 0, 0, 1, 1];
    let mut rd = [0u8; 56];
    rd[0..4].copy_from_slice(&1u32.to_le_bytes());
    rd[0x10..0x18].copy_from_slice(&((BLOCK_WORDS * BLOCKS_PER_CHUNK) as u64 * 8).to_le_bytes());
    rd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    rd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    rd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    rd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    rd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    rd[0x30..0x34].copy_from_slice(&4u32.to_le_bytes());
    let cc: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_COMMITTED)) };
    let mut buf: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { cc(dev, props.as_ptr(), 0, rd.as_ptr(), ST_COMMON, core::ptr::null(), &IID_RESOURCE, &mut buf) };
    if hr < 0 || buf.is_null() {
        record("d3d12_xs", "dbcount", &format!("counter combine scratch FAILED hr={hr:#010x}"));
        return None;
    }
    let va: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(vslot(buf, RES_GET_VA)) };
    Some((buf as usize, unsafe { va(buf) }))
}

unsafe fn new_aux(list: *mut c_void) -> Option<(u32, usize, u32)> {
    let dev = unsafe { device_of(list) } as usize;
    pools(|p| {
        if p.dev != dev {
            *p = Pools { dev, ..Pools::default() };
        }
        let s = p.aux.take(&passed);
        if (s / AUX_CHUNK) as usize >= p.heaps.len() {
            match unsafe { create_heap(dev as *mut c_void) } {
                Some(h) => p.heaps.push(h),
                None => {
                    p.aux.untake(s);
                    return None;
                }
            }
        }
        p.aux.hold(key(list), s);
        Some((s, p.heaps[(s / AUX_CHUNK) as usize], s % AUX_CHUNK))
    })
}

fn aux_loc(s: u32) -> Option<(usize, u32)> {
    pools(|p| p.heaps.get((s / AUX_CHUNK) as usize).map(|&h| (h, s % AUX_CHUNK)))
}

unsafe fn new_block(list: *mut c_void) -> Option<(usize, u64, u32)> {
    let dev = unsafe { device_of(list) } as usize;
    pools(|p| {
        if p.dev != dev {
            return None;
        }
        let b = p.scratch.take(&passed);
        if (b / BLOCKS_PER_CHUNK) as usize >= p.chunks.len() {
            match unsafe { create_chunk(dev as *mut c_void) } {
                Some(c) => p.chunks.push(c),
                None => {
                    p.scratch.untake(b);
                    return None;
                }
            }
        }
        p.scratch.hold(key(list), b);
        let (r, va) = p.chunks[(b / BLOCKS_PER_CHUNK) as usize];
        Some((r, va, (b % BLOCKS_PER_CHUNK) * BLOCK_WORDS * 8))
    })
}

pub unsafe fn note_executed(queue: *mut c_void, lists: &[*mut c_void]) {
    let Some(&first) = lists.first() else { return };
    let keys: Vec<usize> = lists.iter().map(|&l| key(l)).collect();
    if !pools(|p| p.aux.holds_any(&keys) || p.scratch.holds_any(&keys)) {
        return;
    }
    let (f, v) = unsafe { crate::cl2_state::signal_queue(queue, first) }.unwrap_or_else(|| {
        if once(16) {
            record("d3d12_xs", "dbcount", "no fence for a queue executing counting queries; their slots are never reused");
        }
        (usize::MAX, u64::MAX)
    });
    pools(|p| {
        p.aux.note_exec(&keys, f, v);
        p.scratch.note_exec(&keys, f, v);
    });
}

#[derive(Clone, Copy)]
struct Gpu {
    dev: usize,
    rs: usize,
    pso: usize,
}

static GPU: Mutex<Option<Option<Gpu>>> = Mutex::new(None);

unsafe fn build_gpu(dev: *mut c_void) -> Option<Gpu> {
    let code = match tombox_xshader::dxc::compile(COMBINE_HLSL, "cs_6_0") {
        Ok(c) => c,
        Err(e) => {
            record("d3d12_xs", "dbcount", &format!("counter combine shader: {e}"));
            return None;
        }
    };
    let crs: unsafe extern "system" fn(*mut c_void, u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_ROOT_SIGNATURE)) };
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { crs(dev, 0, code.as_ptr(), code.len(), &IID_ROOT_SIGNATURE, &mut rs) };
    if hr < 0 || rs.is_null() {
        record("d3d12_xs", "dbcount", &format!("counter combine root signature FAILED hr={hr:#010x}"));
        return None;
    }
    let mut desc = [0u8; 56];
    desc[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    desc[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    desc[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let cps: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_COMPUTE_PSO)) };
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { cps(dev, desc.as_ptr(), &IID_PSO, &mut pso) };
    if hr < 0 || pso.is_null() {
        record("d3d12_xs", "dbcount", &format!("counter combine PSO FAILED hr={hr:#010x}"));
        return None;
    }
    Some(Gpu { dev: dev as usize, rs: rs as usize, pso: pso as usize })
}

unsafe fn gpu(list: *mut c_void) -> Option<Gpu> {
    let mut g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dev = unsafe { device_of(list) } as usize;
    if let Some(Some(p)) = *g {
        if p.dev == dev {
            return Some(p);
        }
    }
    let p = if dev == 0 { None } else { unsafe { build_gpu(dev as *mut c_void) } };
    *g = Some(p);
    p
}

#[derive(Clone)]
enum PsSrc {
    Xbox(Arc<Vec<u8>>, u64, [u8; 8]),
    #[cfg(test)]
    Hlsl(String, String),
}

static PS_SRC: Mutex<Option<HashMap<usize, (usize, u64, PsSrc)>>> = Mutex::new(None);
static PS_STRIP: Mutex<Option<HashMap<(usize, u64), Result<Arc<Vec<u8>>, String>>>> = Mutex::new(None);

fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn note_ps_src(code: &[u8], src: PsSrc) {
    PS_SRC.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(code.as_ptr() as usize, (code.len(), fnv1a(code), src));
}

pub fn note_translated(code: &[u8], blob: &[u8], hash: u64, rtfmt: &[u8; 8]) {
    if shader_kind(code) == Some(0) && ps_has_uav(code) == Some(true) {
        note_ps_src(code, PsSrc::Xbox(Arc::new(blob.to_vec()), hash, *rtfmt));
    }
}

#[cfg(test)]
pub fn note_ps_hlsl(code: &[u8], hlsl: &str, profile: &str) {
    note_ps_src(code, PsSrc::Hlsl(hlsl.to_string(), profile.to_string()));
}

fn strip_ps(code: &[u8]) -> Result<Arc<Vec<u8>>, String> {
    let k = (code.as_ptr() as usize, fnv1a(code));
    if let Some(r) = PS_STRIP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&k).cloned()) {
        return r;
    }
    let src = PS_SRC.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&k.0).cloned()).filter(|e| e.0 == code.len() && e.1 == k.1).map(|e| e.2);
    let r = match src {
        None => Err("its translation source was not kept".to_string()),
        Some(PsSrc::Xbox(blob, hash, rtfmt)) => crate::shader_xlat::translate_edited(&blob, hash, &rtfmt, &strip::strip_uav_writes),
        #[cfg(test)]
        Some(PsSrc::Hlsl(h, profile)) => strip::strip_uav_writes(&h).and_then(|s| tombox_xshader::dxc::compile(&s, &profile)),
    }
    .map(Arc::new);
    PS_STRIP.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(k, r.clone());
    r
}

struct Variant {
    pso: usize,
    _keep: Box<dyn Send + Sync>,
}

static VARIANTS: Mutex<Option<HashMap<(usize, usize, Pass, Option<VariantKey>), Variant>>> = Mutex::new(None);

struct Owned {
    _data: Vec<u8>,
    _ps: Option<Arc<Vec<u8>>>,
    _gs: Option<Arc<Vec<u8>>>,
    _src: Src,
}
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

unsafe fn create_variant(list: *mut c_void, src: &Src, pass: Pass, vk: Option<VariantKey>, ps: Option<Arc<Vec<u8>>>) -> Result<(usize, Owned), String> {
    let real = crate::cl2_state::real_device();
    let dev = if real.is_null() { unsafe { device_of(list) } } else { real };
    if dev.is_null() {
        return Err("no device".into());
    }
    let mut out: *mut c_void = core::ptr::null_mut();
    let mut gs = None;
    let (data, hr) = match src {
        Src::Desc { desc, .. } => {
            let mut d = desc.clone();
            if let Some(k) = vk {
                crate::cl2_state::variant_desc(&mut d, k);
            }
            if !variant_desc(&mut d, pass) {
                return Err("short PSO desc".into());
            }
            if let Some(code) = &ps {
                put64(&mut d, D_PS, code.as_ptr() as u64);
                put64(&mut d, D_PS + 8, code.len() as u64);
            }
            let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_GFX_PSO)) };
            let hr = unsafe { f(dev, d.as_ptr(), &IID_PSO, &mut out) };
            (d, hr)
        }
        Src::Stream(r) => {
            let rect = if vk.is_some_and(|k| k.rect) { crate::rectlist::expand_stream(&r.stream, &r.subs) } else { None };
            let s = match &rect {
                Some((e, subs, _)) => variant_stream(e, subs, pass, ps.as_ref().map(|c| c.as_slice())),
                None => variant_stream(&r.stream, &r.subs, pass, ps.as_ref().map(|c| c.as_slice())),
            };
            gs = rect.map(|v| v.2);
            let hr = unsafe { create_stream_on(dev, &s, &mut out) }.map_err(|e| e.to_string())?;
            (s, hr)
        }
    };
    if hr < 0 || out.is_null() {
        return Err(format!("{pass:?} counting variant FAILED hr={hr:#010x}"));
    }
    Ok((out as usize, Owned { _data: data, _ps: ps, _gs: gs, _src: src.clone() }))
}

unsafe fn create_stream_on(dev: *mut c_void, s: &[u8], out: &mut *mut c_void) -> Result<i32, &'static str> {
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, 0)) };
    let mut d2: *mut c_void = core::ptr::null_mut();
    if unsafe { qi(dev, &IID_DEVICE2, &mut d2) } < 0 || d2.is_null() {
        return Err("no ID3D12Device2 for a pipeline stream");
    }
    let f: unsafe extern "system" fn(*mut c_void, *const [u64; 2], *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(d2, DEV2_CREATE_PIPELINE_STATE)) };
    let desc = [s.len() as u64, s.as_ptr() as u64];
    let hr = unsafe { f(d2, &desc, &IID_PSO, out) };
    unsafe { release(d2) };
    Ok(hr)
}

pub(crate) unsafe fn create_stream_pso(list: *mut c_void, s: &[u8]) -> Result<usize, String> {
    let real = crate::cl2_state::real_device();
    let dev = if real.is_null() { unsafe { device_of(list) } } else { real };
    if dev.is_null() {
        return Err("no device".into());
    }
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { create_stream_on(dev, s, &mut out) }?;
    if hr < 0 || out.is_null() {
        return Err(format!("hr={hr:#010x}"));
    }
    Ok(out as usize)
}

unsafe fn pass_pso(list: *mut c_void, pso: usize, src: &Src, pass: Pass, vk: Option<VariantKey>, ps: Option<Arc<Vec<u8>>>) -> usize {
    let id = identity(src);
    let mut g = VARIANTS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(&(pso, id, pass, vk)) {
        return v.pso;
    }
    match unsafe { create_variant(list, src, pass, vk, ps) } {
        Ok((p, keep)) => {
            m.insert((pso, id, pass, vk), Variant { pso: p, _keep: Box::new(keep) });
            p
        }
        Err(e) => {
            if once(16) {
                record("d3d12_xs", "dbcount", &format!("PSO {pso:#x}: {e}"));
            }
            0
        }
    }
}

unsafe fn pc_set_pso(list: *mut c_void, pso: usize) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SET_PSO)) };
    unsafe { f(list, pso as *mut c_void) };
}

unsafe fn pc_null_so(list: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *const u8) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SO_TARGETS)) };
    unsafe { f(list, 0, 4, core::ptr::null()) };
}

unsafe fn aux_query(list: *mut c_void, heap: usize, begin: bool, index: u32) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, if begin { PC_BEGIN_QUERY } else { PC_END_QUERY })) };
    unsafe { f(list, heap as *mut c_void, QUERY_OCCLUSION, index) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draw {
    Instanced(u32, u32, u32, u32),
    Indexed(u32, u32, u32, i32, u32),
    IndexedX(u64, u32, u32, u32),
    Indirect(usize, u32, usize, u64, usize, u64),
    Bundle(usize),
    MeshX(u32, u64, u64, u64),
    Mesh(u32, u32, u32),
}

impl Draw {
    pub fn rebinds_state(&self) -> bool {
        matches!(self, Draw::Indirect(..))
    }
}

unsafe fn issue(list: *mut c_void, d: Draw, replay: bool) {
    let _ij = if replay { Some(unsafe { crate::inject::scope(list, crate::inject::DRAW) }) } else { None };
    let pc = |i: usize| unsafe { crate::cmdlist_wrap::pc_method(list, i) };
    match d {
        Draw::Instanced(a, b, c, e) => {
            let p = if replay { pc(PC_DRAW) } else { prev(X_DRAW) };
            let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = unsafe { core::mem::transmute(p) };
            unsafe { f(list, a, b, c, e) }
        }
        Draw::Indexed(a, b, c, e, h) => {
            let p = if replay { pc(PC_DRAW_INDEXED) } else { prev(X_DRAW_INDEXED) };
            let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32) = unsafe { core::mem::transmute(p) };
            unsafe { f(list, a, b, c, e, h) }
        }
        Draw::IndexedX(va, fmt, n, inst) => fwd!(X_DRAW_INDEXED_X, fn(u64, u32, u32, u32) -> (), list, va, fmt, n, inst),
        Draw::Indirect(sig, max, args, aoff, cnt, coff) => fwd!(X_EXEC_INDIRECT, fn(*mut c_void, u32, *mut c_void, u64, *mut c_void, u64) -> (), list, sig as *mut c_void, max, args as *mut c_void, aoff, cnt as *mut c_void, coff),
        Draw::Bundle(b) => fwd!(X_EXEC_BUNDLE, fn(*mut c_void) -> (), list, b as *mut c_void),
        Draw::MeshX(n, flags, args, payload) => fwd!(X_DISPATCH_MESH_X, fn(u32, u64, u64, u64) -> (), list, n, flags, args, payload),
        Draw::Mesh(x, y, z) => fwd!(X_DISPATCH_MESH, fn(u32, u32, u32) -> (), list, x, y, z),
    }
}

static NOTE: [AtomicUsize; 12] = [const { AtomicUsize::new(0) }; 12];

fn note(i: usize, msg: &dyn Fn() -> String) {
    if NOTE[i].fetch_add(1, Ordering::Relaxed) < 4 {
        record("d3d12_xs", "dbcount", &msg());
    }
}

static DSVS: Mutex<Option<HashMap<usize, u32>>> = Mutex::new(None);

pub fn note_dsv(handle: usize, desc: Option<&[u8]>) {
    DSVS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(handle, dsv_first_slice(desc));
}

fn dsv_slice(handle: usize) -> Option<u32> {
    DSVS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&handle).copied()
}

fn slice_of(list: *mut c_void, info: Option<&Info>) -> Result<u32, &'static str> {
    let info = info.ok_or("no recorded pipeline")?;
    if info.view_instancing {
        return Err("view instancing selects the slice per view");
    }
    if rtai_written(info) {
        return Err("a geometry stage writes SV_RenderTargetArrayIndex, so the slice varies per primitive");
    }
    match with(list, |l| l.dsv) {
        Some(Some(h)) => dsv_slice(h).ok_or("the bound depth view was not created or copied through this device"),
        Some(None) => Ok(0),
        None => Err("no render targets recorded on this list"),
    }
}

fn fallback_reason(info: Option<&Info>) -> Option<String> {
    let Some(info) = info else { return Some("no recorded PC pipeline for a counting pass".into()) };
    for &st in info.stages.iter().filter(|s| s.0 != SUB_PS) {
        if ps_has_uav(unsafe { code(st) }) != Some(false) {
            return Some(format!("shader stage {} writes UAVs (or has no PSV0); a counting replay would repeat its stores", st.0));
        }
    }
    None
}

fn ps_override(info: &Info) -> Result<Option<Arc<Vec<u8>>>, String> {
    let Some(&ps) = info.stages.iter().find(|s| s.0 == SUB_PS) else { return Ok(None) };
    let c = unsafe { code(ps) };
    match ps_has_uav(c) {
        Some(false) => Ok(None),
        None => Err("pixel shader has no PSV0".into()),
        Some(true) => strip_ps(c).map(Some).map_err(|e| format!("pixel shader writes UAVs and its store-free counting variant failed: {e}")),
    }
}

unsafe fn counted_draw(list: *mut c_void, d: Draw) {
    if ACTIVE.load(Ordering::Relaxed) == 0 {
        return unsafe { issue(list, d, false) };
    }
    let (active, count) = with(list, |l| (l.active, l.count));
    let c = counter0(count.control);
    if active.is_none() || zpass_only_at_rate(c, count.log_samples) {
        return unsafe { issue(list, d, false) };
    }
    let pso = match d {
        Draw::Bundle(b) => crate::cl2_state::bound_pso(crate::cmdlist_wrap::unwrap(b as *mut c_void)).unwrap_or(0),
        _ => crate::cl2_state::bound_pso(list).unwrap_or(0),
    };
    let src = source(pso);
    let info = src.as_ref().and_then(info_of);
    let log = info.as_ref().map(|i| i.log).unwrap_or(count.log_samples);
    let ds = info.as_ref().map(|i| i.ds).unwrap_or(DsState { depth_test: true, stencil_test: true, ..DsState::default() });
    let slice = if !c.disabled && c.even != c.odd {
        match slice_of(list, info.as_ref()) {
            Ok(s) => Some(s),
            Err(why) => {
                note(0, &|| format!("DB_COUNT_CONTROL {:#010x}: counter 0 counts only on {} slices but {why}; this draw is counted as slice 0", count.control, if c.even { "even" } else { "odd" }));
                None
            }
        }
    } else {
        Some(0)
    };
    let mut p = plan(c, ds, log, slice);
    if p.pc_equivalent() {
        return unsafe { issue(list, d, false) };
    }
    if !c.perfect && count.custom {
        note(1, &|| format!("DB_COUNT_CONTROL {:#010x}: PERFECT_ZPASS_COUNTS clear; the console may skip partially covered tiles, the PC count is exact", count.control));
    }
    if p.shr != 0 {
        note(2, &|| format!("DB_COUNT_CONTROL {:#010x}: SAMPLE_RATE {} below the {}x target; the per-draw sample count is scaled down by 2^{} and floored", count.control, c.rate, 1u32 << log, p.shr));
    }
    if p.order_dependent {
        note(3, &|| format!("DB_COUNT_CONTROL {:#010x}: PSO {pso:#x} writes stencil or depth (with depth bounds) while a replayed category is counted; that category is taken against the buffer as it was before the draw", count.control));
    }
    if unsafe { gpu(list) }.is_none() {
        note(4, &|| "no counting resources; draw counted as PC Z-pass".to_string());
        return unsafe { issue(list, d, false) };
    }
    let mut group = Group { aux: Vec::new(), shl: p.shl, shr: p.shr };
    let mut passes: Vec<(usize, i64)> = Vec::new();
    if p.needs_replay() && matches!(d, Draw::Bundle(_)) {
        note(8, &|| format!("DB_COUNT_CONTROL {:#010x}: a bundle draws with its own pipeline state, which a counting pass cannot replace; its draws are counted as Z-pass samples only", count.control));
        p = zpass_fallback(p);
    }
    if p.needs_replay() {
        let ps = match fallback_reason(info.as_ref()) {
            Some(why) => Err(why),
            None => ps_override(info.as_ref().unwrap()),
        };
        match (ps, src.as_ref()) {
            (Ok(ps), Some(s)) => {
                let vk = unsafe { crate::cl2_state::bound_variant_key(list) };
                for (pass, w) in [(Pass::StencilOnly, p.s), (Pass::Coverage, p.b), (Pass::Raster, p.d)] {
                    if w != 0 {
                        passes.push((unsafe { pass_pso(list, pso, s, pass, vk, ps.clone()) }, w));
                    }
                }
                if passes.iter().any(|v| v.0 == 0) {
                    note(6, &|| format!("PSO {pso:#x}: a counting variant could not be built; this draw is counted as Z-pass samples only"));
                    passes.clear();
                    p = zpass_fallback(p);
                }
            }
            (r, _) => {
                let why = match r {
                    Err(w) => w,
                    Ok(_) => "no recorded PC pipeline for a counting pass".to_string(),
                };
                note(5, &|| format!("PSO {pso:#x}: {why}; this draw is counted as Z-pass samples only"));
                p = zpass_fallback(p);
            }
        }
    }
    if !passes.is_empty() {
        let so = info.as_ref().is_some_and(|i| i.has_so);
        let keys = unsafe { crate::cl2_state::pause_queries(list, false) };
        for &(v, w) in &passes {
            let Some((slot, heap, idx)) = (unsafe { new_aux(list) }) else {
                note(9, &|| "counting query heap unavailable; a replayed category is lost".to_string());
                continue;
            };
            unsafe {
                if so {
                    pc_null_so(list);
                }
                pc_set_pso(list, v);
                aux_query(list, heap, true, idx);
                issue(list, d, true);
                aux_query(list, heap, false, idx);
                if d.rebinds_state() {
                    crate::cl2_state::reapply(list);
                }
            }
            group.aux.push((slot, w));
        }
        unsafe {
            if !d.rebinds_state() {
                if so {
                    crate::cl2_state::reapply_so(list);
                }
                crate::cl2_state::rebind_pso(list);
            }
            crate::cl2_state::unpause_queries(list, &keys);
        }
    }
    if p.z == 1 && p.shl == 0 && p.shr == 0 {
        unsafe { issue(list, d, false) };
    } else {
        let keys = unsafe { crate::cl2_state::pause_queries(list, true) };
        match (p.z != 0).then(|| unsafe { new_aux(list) }).flatten() {
            Some((slot, heap, idx)) => {
                unsafe {
                    aux_query(list, heap, true, idx);
                    issue(list, d, false);
                    aux_query(list, heap, false, idx);
                }
                group.aux.push((slot, p.z));
            }
            None => unsafe { issue(list, d, false) },
        }
        unsafe { crate::cl2_state::unpause_queries(list, &keys) };
    }
    if !group.aux.is_empty() {
        with(list, |l| l.groups.push(group));
    }
}

unsafe extern "system" fn w_draw(this: *mut c_void, a: u32, b: u32, c: u32, d: u32) {
    unsafe { counted_draw(this, Draw::Instanced(a, b, c, d)) }
}

unsafe extern "system" fn w_draw_indexed(this: *mut c_void, a: u32, b: u32, c: u32, d: i32, e: u32) {
    unsafe { counted_draw(this, Draw::Indexed(a, b, c, d, e)) }
}

unsafe extern "system" fn w_draw_indexed_x(this: *mut c_void, va: u64, fmt: u32, n: u32, inst: u32) {
    unsafe { counted_draw(this, Draw::IndexedX(va, fmt, n, inst)) }
}

unsafe extern "system" fn w_exec_indirect(this: *mut c_void, sig: *mut c_void, max: u32, args: *mut c_void, aoff: u64, cnt: *mut c_void, coff: u64) {
    unsafe { counted_draw(this, Draw::Indirect(sig as usize, max, args as usize, aoff, cnt as usize, coff)) }
}

unsafe extern "system" fn w_exec_bundle(this: *mut c_void, bundle: *mut c_void) {
    unsafe { counted_draw(this, Draw::Bundle(bundle as usize)) }
}

unsafe extern "system" fn w_dispatch_mesh_x(this: *mut c_void, n: u32, flags: u64, args: u64, payload: u64) {
    unsafe { counted_draw(this, Draw::MeshX(n, flags, args, payload)) }
}

unsafe extern "system" fn w_dispatch_mesh(this: *mut c_void, x: u32, y: u32, z: u32) {
    unsafe { counted_draw(this, Draw::Mesh(x, y, z)) }
}

static PSO_LOG: Mutex<Option<HashMap<usize, (usize, u32)>>> = Mutex::new(None);

fn pso_identity(pso: usize) -> usize {
    match crate::device_wrap::gfx_pso_identity(pso) {
        0 => stream_rec(pso).map(|r| Arc::as_ptr(&r) as usize).unwrap_or(0),
        id => id,
    }
}

fn pso_log(pso: usize) -> Option<u32> {
    let id = if pso == 0 { 0 } else { pso_identity(pso) };
    if id == 0 {
        return None;
    }
    if let Some(&(i, l)) = PSO_LOG.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&pso)) {
        if i == id {
            return Some(l);
        }
    }
    let l = source(pso).as_ref().and_then(info_of)?.log;
    PSO_LOG.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(pso, (id, l));
    Some(l)
}

unsafe extern "system" fn w_pso(this: *mut c_void, pso: *mut c_void) {
    fwd!(X_PSO, fn(*mut c_void) -> (), this, pso);
    let Some(log) = pso_log(pso as usize) else { return };
    with(this, |l| {
        if l.count.log_samples != log {
            l.count = sample_state(l.count, log);
        }
    });
}

unsafe extern "system" fn w_reset(this: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    let hr = fwd!(X_RESET, fn(*mut c_void, *mut c_void) -> i32, this, alloc, pso);
    with(this, |l| {
        l.count = reset(l.count);
        set_active(l, None);
        l.groups.clear();
        l.dsv = None;
    });
    let k = key(this);
    pools(|p| {
        p.aux.release(k);
        p.scratch.release(k);
    });
    hr
}

unsafe extern "system" fn w_begin_query(this: *mut c_void, heap: *mut c_void, ty: u32, index: u32) {
    if ty == QUERY_OCCLUSION || ty == QUERY_BINARY {
        let (old, groups) = with(this, |l| {
            let old = l.active;
            set_active(l, Some((heap as usize, ty, index)));
            (old, core::mem::take(&mut l.groups))
        });
        if let Some((h, t, i)) = old {
            finish(h, i, t, groups);
        }
        drop_done(heap as usize, index);
    }
    fwd!(X_BEGIN_QUERY, fn(*mut c_void, u32, u32) -> (), this, heap, ty, index);
}

unsafe extern "system" fn w_end_query(this: *mut c_void, heap: *mut c_void, ty: u32, index: u32) {
    fwd!(X_END_QUERY, fn(*mut c_void, u32, u32) -> (), this, heap, ty, index);
    if ty == QUERY_OCCLUSION || ty == QUERY_BINARY {
        let got = with(this, |l| {
            if l.active == Some((heap as usize, ty, index)) {
                set_active(l, None);
                Some(core::mem::take(&mut l.groups))
            } else {
                None
            }
        });
        if let Some(groups) = got {
            finish(heap as usize, index, ty, groups);
        }
    }
}

pub fn combine_constants(mode: u32, acc: u32, shl: u32, shr: u32, binary: bool, offs: &[u32], ws: &[i64]) -> [u32; 14] {
    let mut k = [0u32; 14];
    k[0] = mode;
    k[1] = acc;
    k[2] = offs.len().min(4) as u32;
    k[3] = shl;
    k[4] = shr;
    k[5] = binary as u32;
    for (i, &o) in offs.iter().take(4).enumerate() {
        k[6 + i] = o;
    }
    for (i, &w) in ws.iter().take(4).enumerate() {
        k[10 + i] = w as i32 as u32;
    }
    k
}

pub fn combine_cpu(title: u64, groups: &[(Vec<(u64, i64)>, u32, u32)], binary: bool) -> u64 {
    let mut acc: i64 = 0;
    for (aux, shl, shr) in groups {
        let g: i64 = aux.iter().map(|&(c, w)| w * c as i64).sum();
        acc += (g << shl) >> shr;
    }
    if binary { (title != 0 || acc > 0) as u64 } else { title.wrapping_add(acc.max(0) as u64) }
}

pub fn batches(groups: &[Group], cap: usize) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut n = 0;
    for (i, g) in groups.iter().enumerate() {
        if n + g.aux.len() > cap && i > start {
            out.push(start..i);
            start = i;
            n = 0;
        }
        n += g.aux.len();
    }
    if start < groups.len() {
        out.push(start..groups.len());
    }
    out
}

unsafe fn uav_barrier(list: *mut c_void, res: usize) {
    let mut b = [0u8; 32];
    b[0..4].copy_from_slice(&2u32.to_le_bytes());
    b[8..16].copy_from_slice(&(res as u64).to_le_bytes());
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_BARRIER)) };
    unsafe { f(list, 1, b.as_ptr()) };
}

unsafe fn combine(list: *mut c_void, g: &Gpu, heap: *mut c_void, ty: u32, index: u32, dst: *mut c_void, dst_off: u64, groups: &[Group]) -> bool {
    let Some((res, va, base)) = (unsafe { new_block(list) }) else { return false };
    let k = key(list);
    pools(|p| group_slots(groups).for_each(|s| p.aux.hold(k, s)));
    let t = base;
    let acc = base + 8;
    let cp: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *mut c_void, u64, u64) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_COPY_BUFFER)) };
    let rq: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, *mut c_void, u64) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_RESOLVE_QUERY)) };
    let srs: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SET_CROOT)) };
    let sc: unsafe extern "system" fn(*mut c_void, u32, u32, *const u32, u32) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SET_CCONSTS)) };
    let su: unsafe extern "system" fn(*mut c_void, u32, u64) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SET_CUAV)) };
    let dp: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_DISPATCH)) };
    let s = res as *mut c_void;
    unsafe { crate::fill::barrier_pub(list, res, ST_COMMON, ST_COPY_DEST) };
    fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64) -> (), list, heap, ty, index, 1, s, t as u64);
    unsafe {
        let runs = batches(groups, BLOCK_ENTRIES as usize);
        for (bi, run) in runs.iter().enumerate() {
            let mut w = base + 16;
            let mut offs: Vec<Vec<u32>> = Vec::new();
            for grp in &groups[run.clone()] {
                let mut o = Vec::new();
                for &(slot, _) in &grp.aux {
                    if let Some((h, i)) = aux_loc(slot) {
                        rq(list, h as *mut c_void, QUERY_OCCLUSION, i, 1, s, w as u64);
                    }
                    o.push(w);
                    w += 8;
                }
                offs.push(o);
            }
            crate::fill::barrier_pub(list, res, ST_COPY_DEST, ST_UAV);
            if bi == 0 {
                srs(list, g.rs as *mut c_void);
                pc_set_pso(list, g.pso);
                su(list, 1, va);
                let k = combine_constants(2, acc, 0, 0, false, &[], &[]);
                sc(list, 0, 14, k.as_ptr(), 0);
                dp(list, 1, 1, 1);
                uav_barrier(list, res);
            }
            for (grp, o) in groups[run.clone()].iter().zip(&offs) {
                let ws: Vec<i64> = grp.aux.iter().map(|a| a.1).collect();
                let k = combine_constants(0, acc, grp.shl, grp.shr, false, o, &ws);
                sc(list, 0, 14, k.as_ptr(), 0);
                dp(list, 1, 1, 1);
                uav_barrier(list, res);
            }
            if bi + 1 < runs.len() {
                crate::fill::barrier_pub(list, res, ST_UAV, ST_COPY_DEST);
            }
        }
        let k = combine_constants(1, acc, 0, 0, ty == QUERY_BINARY, &[t], &[]);
        sc(list, 0, 14, k.as_ptr(), 0);
        dp(list, 1, 1, 1);
        crate::fill::barrier_pub(list, res, ST_UAV, ST_COPY_SOURCE);
        cp(list, dst, dst_off, s, t as u64, 8);
        crate::fill::barrier_pub(list, res, ST_COPY_SOURCE, ST_COMMON);
    }
    true
}

unsafe extern "system" fn w_resolve(this: *mut c_void, heap: *mut c_void, ty: u32, start: u32, n: u32, dst: *mut c_void, off: u64) {
    let work: Vec<(u32, Vec<Group>)> = if dst.is_null() || (ty != QUERY_OCCLUSION && ty != QUERY_BINARY) {
        Vec::new()
    } else {
        done(|m| (start..start.saturating_add(n)).filter_map(|i| m.get(&(heap as usize, i)).filter(|e| e.0 == ty).map(|e| (i, e.1.clone()))).collect())
    };
    let g = if work.is_empty() { None } else { unsafe { gpu(this) } };
    let Some(g) = g else {
        fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64) -> (), this, heap, ty, start, n, dst, off);
        return;
    };
    let skip: Vec<u32> = work.iter().map(|w| w.0).collect();
    for (s, c) in crate::cl2_state::resolve_runs(start, n, &skip) {
        fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64) -> (), this, heap, ty, s, c, dst, off + (s - start) as u64 * 8);
    }
    for (i, groups) in work {
        if !unsafe { combine(this, &g, heap, ty, i, dst, off + (i - start) as u64 * 8, &groups) } {
            note(10, &|| "counter combine scratch unavailable; query resolved with its PC Z-pass part only".to_string());
            fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64) -> (), this, heap, ty, i, 1, dst, off + (i - start) as u64 * 8);
        }
    }
    unsafe { crate::cl2_state::reapply(this) };
}

fn closed_reject(list: *mut c_void, what: &str) -> bool {
    let Some(e) = recordable(crate::cl2_state::title_closed(list)) else { return false };
    note(7, &|| format!("{what}: {e} (console debug validation); not recorded"));
    true
}

unsafe extern "system" fn w_set_control(this: *mut c_void, ctrl: *const u32) {
    if closed_reject(this, "SetOcclusionQueryControlX") {
        return;
    }
    let v = if ctrl.is_null() { None } else { Some(unsafe { *ctrl }) };
    let s = with(this, |l| {
        l.count = set_control(l.count, v);
        l.count
    });
    if once(8) {
        let f = decode(s.control);
        record("d3d12_xs", "dbcount", &format!("SetDBCountControlX {:#010x} -> reg {DB_COUNT_CONTROL_REG:#x} = {:#010x} {f:?} fixed_rate={} ", v.unwrap_or(DEFAULT_CONTROL), s.control, s.fixed_rate));
    }
}

unsafe extern "system" fn w_set_perfect(this: *mut c_void, perfect: u32) {
    if closed_reject(this, "SetPerfectOcclusionQueriesX") {
        return;
    }
    with(this, |l| {
        if let Some(n) = set_perfect(l.count, perfect != 0) {
            l.count = n;
        }
    });
}

unsafe extern "system" fn w_rt(this: *mut c_void, n: u32, h: *const usize, single: i32, dsv: *const usize) {
    let d = if dsv.is_null() { None } else { Some(unsafe { *dsv }) };
    with(this, |l| l.dsv = Some(d));
    fwd!(X_RT, fn(u32, *const usize, i32, *const usize) -> (), this, n, h, single, dsv)
}

static PREV_DSV: AtomicUsize = AtomicUsize::new(0);
static PREV_COPY_SIMPLE: AtomicUsize = AtomicUsize::new(0);
static DSV_INC: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn w_create_dsv(this: *mut c_void, res: *mut c_void, desc: *const u8, handle: usize) {
    note_dsv(handle, if desc.is_null() { None } else { Some(unsafe { core::slice::from_raw_parts(desc, 24) }) });
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *const u8, usize) = unsafe { core::mem::transmute(PREV_DSV.load(Ordering::Relaxed)) };
    unsafe { f(this, res, desc, handle) }
}

pub fn copy_dsvs(n: u32, dst: usize, src: usize, inc: usize) {
    let mut g = DSVS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    let vals: Vec<Option<u32>> = (0..n as usize).map(|i| m.get(&(src + i * inc)).copied()).collect();
    for (i, v) in vals.into_iter().enumerate() {
        match v {
            Some(s) => m.insert(dst + i * inc, s),
            None => m.remove(&(dst + i * inc)),
        };
    }
}

unsafe extern "system" fn w_copy_simple(this: *mut c_void, n: u32, dst: usize, src: usize, ty: u32) {
    let f: unsafe extern "system" fn(*mut c_void, u32, usize, usize, u32) = unsafe { core::mem::transmute(PREV_COPY_SIMPLE.load(Ordering::Relaxed)) };
    unsafe { f(this, n, dst, src, ty) };
    if ty == HEAP_TYPE_DSV {
        let mut inc = DSV_INC.load(Ordering::Relaxed);
        if inc == 0 {
            let gi: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { core::mem::transmute(vslot(this, DEV_INCREMENT_SIZE)) };
            inc = unsafe { gi(this, HEAP_TYPE_DSV) } as usize;
            DSV_INC.store(inc, Ordering::Relaxed);
        }
        copy_dsvs(n, dst, src, inc);
    }
}

pub fn install_device(v: &mut [usize]) -> bool {
    if v.len() <= DEV_COPY_SIMPLE || v[DEV_CREATE_DSV] == 0 || v[DEV_CREATE_DSV] == w_create_dsv as usize || v[DEV_COPY_SIMPLE] == 0 {
        return false;
    }
    PREV_DSV.store(v[DEV_CREATE_DSV], Ordering::Relaxed);
    v[DEV_CREATE_DSV] = w_create_dsv as usize;
    PREV_COPY_SIMPLE.store(v[DEV_COPY_SIMPLE], Ordering::Relaxed);
    v[DEV_COPY_SIMPLE] = w_copy_simple as usize;
    true
}

pub fn plan_slots() -> Vec<(usize, usize)> {
    vec![
        (X_RESET, w_reset as usize),
        (X_DRAW, w_draw as usize),
        (X_DRAW_INDEXED, w_draw_indexed as usize),
        (X_PSO, w_pso as usize),
        (X_EXEC_BUNDLE, w_exec_bundle as usize),
        (X_BEGIN_QUERY, w_begin_query as usize),
        (X_END_QUERY, w_end_query as usize),
        (X_RESOLVE_QUERY, w_resolve as usize),
        (X_RT, w_rt as usize),
        (X_EXEC_INDIRECT, w_exec_indirect as usize),
        (X_DRAW_INDEXED_X, w_draw_indexed_x as usize),
    ]
}

pub fn mesh_slots(vt: &[AtomicUsize]) -> Vec<(usize, usize)> {
    let console = crate::console_slots_mesh::list_plan(crate::console_slots_dev2::Family::Scarlett);
    let ours = [(X_DISPATCH_MESH_X, w_dispatch_mesh_x as usize), (X_DISPATCH_MESH, w_dispatch_mesh as usize)];
    ours.into_iter().filter(|&(s, _)| console.iter().any(|&(cs, cf)| cs == s && vt.get(s).is_some_and(|v| v.load(Ordering::Relaxed) == cf))).collect()
}

fn chain(vt: &[AtomicUsize], slot: usize, f: usize) -> bool {
    if slot >= vt.len() {
        return false;
    }
    let p = vt[slot].load(Ordering::Relaxed);
    if p == 0 || p == f {
        return false;
    }
    PREV[slot].store(p, Ordering::Relaxed);
    vt[slot].store(f, Ordering::Relaxed);
    true
}

pub fn install(vt: &[AtomicUsize]) -> usize {
    let mut slots = plan_slots();
    slots.extend(mesh_slots(vt));
    let mut n = slots.into_iter().filter(|&(s, f)| chain(vt, s, f)).count();
    for (slot, f) in [(X_SET_DB_COUNT_CONTROL, w_set_control as usize), (X_SET_PERFECT_ZPASS, w_set_perfect as usize)] {
        if slot < vt.len() {
            vt[slot].store(f, Ordering::Relaxed);
            n += 1;
        }
    }
    record("d3d12_xs", "dbcount", &format!("DB_COUNT_CONTROL emulation on {n} slots"));
    n
}

#[cfg(test)]
#[path = "dbcount_tests.rs"]
mod tests;
