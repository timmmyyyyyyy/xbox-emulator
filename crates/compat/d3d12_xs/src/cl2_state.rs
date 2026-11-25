use core::ffi::c_void;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use compat_common::record;

use crate::device_wrap::Guid;

const X_CLOSE: usize = 9;
const X_RESET: usize = 10;
const X_CLEAR_STATE: usize = 11;
const X_TOPOLOGY: usize = 20;
const X_VIEWPORTS: usize = 21;
const X_SCISSORS: usize = 22;
const X_BLEND_FACTOR: usize = 23;
const X_STENCIL_REF: usize = 24;
const X_PSO: usize = 25;
const X_EXEC_BUNDLE: usize = 27;
const X_BEGIN_QUERY: usize = 28;
const X_END_QUERY: usize = 29;
const X_RESOLVE_QUERY: usize = 30;
const X_PREDICATION: usize = 31;
const X_HEAPS: usize = 32;
const X_CROOT: usize = 33;
const X_GROOT: usize = 34;
const X_CTABLE: usize = 35;
const X_GTABLE: usize = 36;
const X_C1: usize = 37;
const X_G1: usize = 38;
const X_CN: usize = 39;
const X_GN: usize = 40;
const X_VIEW_FIRST: usize = 41;
const X_VIEW_LAST: usize = 46;
const X_IB: usize = 47;
const X_VB: usize = 48;
const X_SO: usize = 49;
const X_RT: usize = 50;
const X_DEPTH_BOUNDS: usize = 68;
const X_Z_ORDER: usize = 73;
const X_HI_STENCIL: usize = 79;
const X_MSAA: usize = 80;
const X_COMPUTE_LIMITS: usize = 96;
pub const X_KICKOFF: usize = 93;
pub const X_DEPTH_BIAS: usize = 94;
const X_DEPTH_BOUNDS_LIST1: usize = 106;
const X_SAMPLE_POSITIONS: usize = 107;
const X_SHADING_RATE: usize = 124;
const X_SHADING_RATE_IMAGE: usize = 125;

const PC_CLOSE: usize = 9;
const PC_RESET: usize = 10;
const PC_SET_PSO: usize = 25;
const PC_RS_SET_DEPTH_BIAS: usize = 82;
const CHILD_GET_DEVICE: usize = 7;
const DEV_CREATE_ALLOCATOR: usize = 9;
const DEV_CREATE_GFX_PSO: usize = 10;
const DEV_CHECK_FEATURE: usize = 13;
const DEV_CREATE_FENCE: usize = 36;
const DEV_CREATE_HEAP: usize = 14;
const DEV_INCREMENT: usize = 15;
const DEV_COPY_SIMPLE: usize = 24;
const HEAP_CPU_START: usize = 9;
const HEAP_TYPE_RTV: u32 = 2;
const HEAP_TYPE_DSV: u32 = 3;
const ALLOC_RESET: usize = 8;
const FENCE_COMPLETED: usize = 8;
const QUEUE_EXECUTE: usize = 10;
const QUEUE_SIGNAL: usize = 14;
const FEATURE_OPTIONS16: u32 = 45;
pub const PSO_FLAG_DYNAMIC_DEPTH_BIAS: u32 = 0x4;
const DESC_FLAGS: usize = 0x288;
const DESC_DEPTH_BIAS: usize = 0x1d0;
const DESC_DEPTH_CLIP: usize = 0x1dc;
const LIST_TYPE_BUNDLE: u32 = 1;

const IID_DEVICE: Guid = Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
const IID_ALLOCATOR: Guid = Guid { d1: 0x6102_dee4, d2: 0xaf59, d3: 0x4b09, d4: [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24] };
const IID_FENCE: Guid = Guid { d1: 0x0a75_3dcf, d2: 0xc4d8, d3: 0x4b91, d4: [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76] };
const IID_PSO: Guid = Guid { d1: 0x765a_30f3, d2: 0xf624, d3: 0x4c6f, d4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45] };
const IID_DESCRIPTOR_HEAP: Guid = Guid { d1: 0x8efb_471d, d2: 0x616c, d3: 0x4f49, d4: [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51] };
const IID_LIST9: Guid = Guid { d1: 0x34ed_2808, d2: 0xffe6, d3: 0x4c2b, d4: [0xb1, 0x1a, 0xca, 0xbd, 0x2b, 0x0c, 0x59, 0xe1] };

static PREV: [AtomicUsize; 128] = [const { AtomicUsize::new(0) }; 128];
static DYN_BIAS: AtomicI32 = AtomicI32::new(-1);
static REAL_DEVICE: AtomicUsize = AtomicUsize::new(0);
static LOG: AtomicUsize = AtomicUsize::new(0);

fn once(n: usize) -> bool {
    LOG.fetch_add(1, Ordering::Relaxed) < n
}

unsafe fn vslot(obj: *mut c_void, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

unsafe fn pc_list_slot(list: *mut c_void, slot: usize) -> usize {
    unsafe { crate::cmdlist_wrap::pc_method(list, slot) }
}

unsafe fn release(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(obj, 2)) };
    unsafe { f(obj) };
}

fn prev(slot: usize) -> usize {
    PREV[slot].load(Ordering::Relaxed)
}

pub fn effective_clamp(clamp: f32) -> f32 {
    let bits = if clamp.is_finite() && clamp.abs() < 9.2e18 { clamp.trunc() as i64 as u32 } else { 0 };
    let f = f32::from_bits(bits);
    if f.is_normal() { f } else { 0.0 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthBiasX {
    pub bias: i32,
    pub clamp: f32,
    pub slope: f32,
    pub clip: bool,
}

impl DepthBiasX {
    pub fn pc_values(&self) -> (f32, f32, f32) {
        (self.bias as f32, effective_clamp(self.clamp), self.slope)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PsoBias {
    pub dynamic: bool,
    pub bias: i32,
    pub clamp: f32,
    pub slope: f32,
    pub clip: bool,
}

pub fn pso_bias_from_desc(d: &[u8]) -> Option<PsoBias> {
    if d.len() < DESC_FLAGS + 4 {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    Some(PsoBias {
        dynamic: u(DESC_FLAGS) & PSO_FLAG_DYNAMIC_DEPTH_BIAS != 0,
        bias: u(DESC_DEPTH_BIAS) as i32,
        clamp: f32::from_bits(u(DESC_DEPTH_BIAS + 4)),
        slope: f32::from_bits(u(DESC_DEPTH_BIAS + 8)),
        clip: u(DESC_DEPTH_CLIP) != 0,
    })
}

pub fn patch_desc_flags(desc: &mut [u8], supported: bool) -> bool {
    if !supported || desc.len() < DESC_FLAGS + 4 {
        return false;
    }
    let f = u32::from_le_bytes(desc[DESC_FLAGS..DESC_FLAGS + 4].try_into().unwrap()) | PSO_FLAG_DYNAMIC_DEPTH_BIAS;
    desc[DESC_FLAGS..DESC_FLAGS + 4].copy_from_slice(&f.to_le_bytes());
    true
}

unsafe fn dynamic_bias_supported(real_device: *mut c_void) -> bool {
    let v = DYN_BIAS.load(Ordering::Relaxed);
    if v >= 0 {
        return v == 1;
    }
    let mut data = [0u32; 2];
    let check: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 = unsafe { core::mem::transmute(vslot(real_device, DEV_CHECK_FEATURE)) };
    let hr = unsafe { check(real_device, FEATURE_OPTIONS16, data.as_mut_ptr() as *mut c_void, 8) };
    let ok = hr >= 0 && data[0] != 0;
    DYN_BIAS.store(ok as i32, Ordering::Relaxed);
    record("d3d12_xs", "cl2_state", &format!("OPTIONS16 hr={hr:#010x} DynamicDepthBiasSupported={}: graphics PSOs {} created with DYNAMIC_DEPTH_BIAS", data[0], if ok { "are" } else { "are not" }));
    ok
}

pub unsafe fn patch_gfx_pso_desc(real_device: *mut c_void, desc: &mut [u8]) {
    if real_device.is_null() {
        return;
    }
    REAL_DEVICE.store(real_device as usize, Ordering::Relaxed);
    let sup = unsafe { dynamic_bias_supported(real_device) };
    patch_desc_flags(desc, sup);
}

#[derive(Clone, Debug, PartialEq)]
pub enum RootArg {
    Table(u64),
    View(usize, u64),
    Consts(BTreeMap<u32, u32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Targets {
    pub n: u32,
    pub handles: Vec<usize>,
    pub single: i32,
    pub dsv: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListState {
    pub open: bool,
    pub closed: bool,
    pub queue: usize,
    pub kickoff_pending: bool,
    pub list9: Option<bool>,
    pub topology: Option<u32>,
    pub viewports: Option<Vec<u8>>,
    pub scissors: Option<Vec<u8>>,
    pub blend: Option<Option<[u8; 16]>>,
    pub stencil: Option<u32>,
    pub pso: Option<usize>,
    pub variant_bound: bool,
    pub predication: Option<(usize, u64, u32)>,
    pub plain_predicate: bool,
    pub heaps: Option<Vec<usize>>,
    pub root: [Option<usize>; 2],
    pub args: BTreeMap<(u8, u32), RootArg>,
    pub ib: Option<Option<[u8; 16]>>,
    pub vbs: BTreeMap<u32, Option<[u8; 16]>>,
    pub so: BTreeMap<u32, Option<[u8; 24]>>,
    pub targets: Option<Targets>,
    pub depth_bounds: Option<(f32, f32)>,
    pub sample_positions: Option<(u32, u32, Vec<u8>)>,
    pub shading_rate: Option<(u32, Option<[u32; 2]>)>,
    pub shading_rate_image: Option<usize>,
    pub depth_bias_x: Option<DepthBiasX>,
    pub queries: Vec<ActiveQuery>,
    pub suspended: bool,
    pub xstate: XState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveQuery {
    pub heap: usize,
    pub ty: u32,
    pub index: u32,
    pub parts: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MsaaX {
    pub pos: Option<[u8; 128]>,
    pub centroid: Option<[u8; 16]>,
    pub config: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct XState {
    pub z_order: Option<u32>,
    pub hi_stencil: Option<(usize, Option<[u32; 2]>)>,
    pub msaa: Option<MsaaX>,
    pub compute_limits: Option<Option<[u32; 6]>>,
}

pub fn query_words(ty: u32) -> Option<(u32, u32)> {
    match ty {
        0 => Some((1, 0)),
        1 => Some((1, 1)),
        3 => Some((11, 0)),
        4..=7 => Some((2, 0)),
        8 => Some((14, 0)),
        _ => None,
    }
}

impl ListState {
    pub fn clear_bindings(&mut self) {
        let keep = (self.open, self.queue, self.kickoff_pending, self.list9);
        let q = core::mem::take(&mut self.queries);
        let sus = self.suspended;
        *self = ListState::default();
        (self.open, self.queue, self.kickoff_pending, self.list9) = keep;
        self.queries = q;
        self.suspended = sus;
    }

    pub fn begin_query(&mut self, heap: usize, ty: u32, index: u32) -> Vec<ActiveQuery> {
        let (gone, keep): (Vec<ActiveQuery>, Vec<ActiveQuery>) = core::mem::take(&mut self.queries)
            .into_iter()
            .partition(|q| (ty <= 1 && q.ty <= 1) || (q.heap == heap && q.index == index));
        self.queries = keep;
        self.queries.push(ActiveQuery { heap, ty, index, parts: Vec::new() });
        gone
    }

    pub fn end_query(&mut self, heap: usize, index: u32) -> Option<ActiveQuery> {
        let i = self.queries.iter().position(|q| q.heap == heap && q.index == index)?;
        Some(self.queries.remove(i))
    }

    pub fn set_msaa(&mut self, pos: Option<[u8; 128]>, centroid: Option<[u8; 16]>, config: Option<[u32; 2]>) {
        let old = self.xstate.msaa.take().unwrap_or_default();
        if pos.is_some() {
            self.sample_positions = None;
        }
        self.xstate.msaa = Some(MsaaX { pos: pos.or(old.pos), centroid: centroid.or(old.centroid), config: config.or(old.config) });
    }

    pub fn merge_bundle(&mut self, b: &ListState) {
        if let Some(h) = &b.heaps { self.heaps = Some(h.clone()); }
        for side in 0..2u8 {
            if let Some(rs) = b.root[side as usize] { self.set_root(side, rs); }
        }
        for (k, v) in &b.args {
            match (self.args.get_mut(k), v) {
                (Some(RootArg::Consts(m)), RootArg::Consts(n)) => m.extend(n.iter().map(|(a, c)| (*a, *c))),
                _ => { self.args.insert(*k, v.clone()); }
            }
        }
        if let Some(p) = b.pso {
            self.pso = Some(p);
            self.variant_bound = b.variant_bound;
            self.depth_bias_x = b.depth_bias_x;
        } else if b.depth_bias_x.is_some() {
            self.depth_bias_x = b.depth_bias_x;
        }
        if b.topology.is_some() { self.topology = b.topology; }
        if b.blend.is_some() { self.blend = b.blend; }
        if b.stencil.is_some() { self.stencil = b.stencil; }
        if b.ib.is_some() { self.ib = b.ib; }
        for (k, v) in &b.vbs { self.vbs.insert(*k, *v); }
        if b.depth_bounds.is_some() { self.depth_bounds = b.depth_bounds; }
        if b.xstate.hi_stencil.is_some() { self.xstate.hi_stencil = b.xstate.hi_stencil; }
    }

    pub fn set_root(&mut self, side: u8, rs: usize) {
        if self.root[side as usize] != Some(rs) {
            self.args.retain(|k, _| k.0 != side);
        }
        self.root[side as usize] = Some(rs);
    }

    pub fn set_const(&mut self, side: u8, idx: u32, off: u32, vals: &[u32]) {
        let e = self.args.entry((side, idx)).or_insert_with(|| RootArg::Consts(BTreeMap::new()));
        if !matches!(e, RootArg::Consts(_)) {
            *e = RootArg::Consts(BTreeMap::new());
        }
        if let RootArg::Consts(m) = e {
            for (i, v) in vals.iter().enumerate() {
                m.insert(off + i as u32, *v);
            }
        }
    }

    pub fn set_vbs(&mut self, start: u32, views: &[Option<[u8; 16]>]) {
        for (i, v) in views.iter().enumerate() {
            self.vbs.insert(start + i as u32, *v);
        }
    }

    pub fn set_so(&mut self, start: u32, views: &[Option<[u8; 24]>]) {
        for (i, v) in views.iter().enumerate() {
            self.so.insert(start + i as u32, *v);
        }
    }
}

const SHARDS: usize = 64;
static STATES: [Mutex<Option<HashMap<usize, ListState>>>; SHARDS] = [const { Mutex::new(None) }; SHARDS];

fn shard_of(list: usize) -> usize {
    ((list >> 4) ^ (list >> 10) ^ (list >> 16)) % SHARDS
}

fn with<R>(list: *mut c_void, f: impl FnOnce(&mut ListState) -> R) -> R {
    let mut g = STATES[shard_of(list as usize)].lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new).entry(list as usize).or_default())
}

pub fn on_new_list(list: *mut c_void) {
    if !list.is_null() {
        crate::cl_fw::forget(crate::list_split::origin_of(list));
        with(list, |s| *s = ListState { open: true, ..ListState::default() });
    }
}

pub(crate) fn bound_pso(list: *mut c_void) -> Option<usize> {
    with(list, |s| s.pso)
}

pub(crate) fn predication_of(list: *mut c_void) -> Option<(usize, u64, u32)> {
    with(list, |s| s.predication).filter(|p| p.0 != 0)
}

pub(crate) fn note_predication(list: *mut c_void, res: usize, off: u64, op: u32) {
    rec(list, |s| s.predication = Some((res, off, op)));
}

pub(crate) fn queue_of(list: *mut c_void) -> usize {
    with(list, |s| s.queue)
}

pub(crate) fn is_open(list: *mut c_void) -> bool {
    with(list, |s| s.open)
}

pub(crate) fn title_closed(list: *mut c_void) -> bool {
    let g = STATES[shard_of(list as usize)].lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().and_then(|m| m.get(&(list as usize))).is_some_and(|s| s.closed)
}

pub(crate) fn mark_closed(list: *mut c_void) {
    with(list, |s| s.open = false);
}

pub(crate) fn clone_state(from: *mut c_void, to: *mut c_void) {
    let s = with(from, |s| s.clone());
    with(to, |t| *t = ListState { open: true, closed: false, queue: 0, kickoff_pending: false, ..s });
    if from != to {
        crate::query_x::carry_nest(from as usize, to as usize);
    }
}

pub fn raster_snapshot(list: usize) -> Option<(Vec<u8>, Vec<u8>, Option<u32>, Option<usize>)> {
    let g = STATES[shard_of(list)].lock().unwrap_or_else(|e| e.into_inner());
    let s = g.as_ref()?.get(&list)?;
    Some((s.viewports.clone().unwrap_or_default(), s.scissors.clone().unwrap_or_default(), s.stencil, s.pso))
}

fn rec<R>(list: *mut c_void, f: impl FnOnce(&mut ListState) -> R) -> R {
    with(list, |s| {
        s.open = true;
        f(s)
    })
}

unsafe fn bytes(p: *const u8, n: usize) -> Vec<u8> {
    if p.is_null() || n == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(p, n).to_vec() } }
}

unsafe fn arr<const N: usize>(p: *const u8, i: usize) -> [u8; N] {
    unsafe { core::ptr::read_unaligned(p.add(i * N) as *const [u8; N]) }
}

macro_rules! fwd {
    ($slot:expr, fn($($t:ty),*), $($a:expr),*) => {{
        let p = prev($slot);
        if p != 0 {
            let f: unsafe extern "system" fn(*mut c_void, $($t),*) = unsafe { core::mem::transmute(p) };
            unsafe { f($($a),*) }
        }
    }};
}

unsafe extern "system" fn w_topology(this: *mut c_void, t: u32) {
    let was = is_rect(this);
    rec(this, |s| s.topology = Some(t));
    crate::present_probe::note_topology(t);
    fwd!(X_TOPOLOGY, fn(u32), this, crate::rectlist::forwarded(t));
    if t == crate::rectlist::RECTLIST {
        unsafe { apply_rect(this) };
    } else if was {
        unsafe { rebind_pso(this) };
    }
}

unsafe extern "system" fn w_viewports(this: *mut c_void, n: u32, p: *const u8) {
    let v = unsafe { bytes(p, n as usize * 24) };
    rec(this, |s| s.viewports = Some(v));
    fwd!(X_VIEWPORTS, fn(u32, *const u8), this, n, p)
}

unsafe extern "system" fn w_scissors(this: *mut c_void, n: u32, p: *const u8) {
    let v = unsafe { bytes(p, n as usize * 16) };
    rec(this, |s| s.scissors = Some(v));
    fwd!(X_SCISSORS, fn(u32, *const u8), this, n, p)
}

unsafe extern "system" fn w_blend(this: *mut c_void, p: *const u8) {
    let v = if p.is_null() { None } else { Some(unsafe { arr::<16>(p, 0) }) };
    rec(this, |s| s.blend = Some(v));
    fwd!(X_BLEND_FACTOR, fn(*const u8), this, p)
}

unsafe extern "system" fn w_stencil(this: *mut c_void, r: u32) {
    rec(this, |s| s.stencil = Some(r));
    fwd!(X_STENCIL_REF, fn(u32), this, r)
}

pub(crate) fn plain_null_is_noop(plain_active: bool, res: usize) -> bool {
    res == 0 && !plain_active
}

unsafe extern "system" fn w_predication(this: *mut c_void, res: *mut c_void, off: u64, op: u32) {
    if plain_null_is_noop(with(this, |s| s.plain_predicate), res as usize) {
        if once(8) {
            record("d3d12_xs", "cl2_state", &format!("SetPredication(NULL) on list {this:p} with no SetPredication resource active: the console UMD emits no packet (xgs12 0x56fd1 / xgs12_10 0x5e672 test flag bit 1), so the current predicate, including one from SetPredicationBufferX, stays in force"));
        }
        return;
    }
    rec(this, |s| s.plain_predicate = !res.is_null());
    unsafe { reapply_predication(this, res, off, op) }
}

unsafe fn reapply_predication(this: *mut c_void, res: *mut c_void, off: u64, op: u32) {
    rec(this, |s| s.predication = Some((res as usize, off, op)));
    fwd!(X_PREDICATION, fn(*mut c_void, u64, u32), this, res, off, op)
}

unsafe extern "system" fn w_heaps(this: *mut c_void, n: u32, p: *const usize) {
    let v: Vec<usize> = if p.is_null() { Vec::new() } else { (0..n as usize).map(|i| unsafe { *p.add(i) }).collect() };
    rec(this, |s| s.heaps = Some(v));
    fwd!(X_HEAPS, fn(u32, *const usize), this, n, p)
}

unsafe extern "system" fn w_croot(this: *mut c_void, rs: *mut c_void) {
    rec(this, |s| s.set_root(0, rs as usize));
    fwd!(X_CROOT, fn(*mut c_void), this, rs)
}

unsafe extern "system" fn w_groot(this: *mut c_void, rs: *mut c_void) {
    rec(this, |s| s.set_root(1, rs as usize));
    fwd!(X_GROOT, fn(*mut c_void), this, rs)
}

unsafe extern "system" fn w_ctable(this: *mut c_void, i: u32, h: u64) {
    rec(this, |s| { s.args.insert((0, i), RootArg::Table(h)); });
    fwd!(X_CTABLE, fn(u32, u64), this, i, h)
}

unsafe extern "system" fn w_gtable(this: *mut c_void, i: u32, h: u64) {
    rec(this, |s| { s.args.insert((1, i), RootArg::Table(h)); });
    fwd!(X_GTABLE, fn(u32, u64), this, i, h)
}

unsafe extern "system" fn w_c1(this: *mut c_void, i: u32, v: u32, off: u32) {
    rec(this, |s| s.set_const(0, i, off, &[v]));
    fwd!(X_C1, fn(u32, u32, u32), this, i, v, off)
}

unsafe extern "system" fn w_g1(this: *mut c_void, i: u32, v: u32, off: u32) {
    rec(this, |s| s.set_const(1, i, off, &[v]));
    fwd!(X_G1, fn(u32, u32, u32), this, i, v, off)
}

unsafe fn dwords(p: *const u32, n: u32) -> Vec<u32> {
    if p.is_null() { Vec::new() } else { (0..n as usize).map(|i| unsafe { core::ptr::read_unaligned(p.add(i)) }).collect() }
}

unsafe extern "system" fn w_cn(this: *mut c_void, i: u32, n: u32, p: *const u32, off: u32) {
    let v = unsafe { dwords(p, n) };
    rec(this, |s| s.set_const(0, i, off, &v));
    fwd!(X_CN, fn(u32, u32, *const u32, u32), this, i, n, p, off)
}

unsafe extern "system" fn w_gn(this: *mut c_void, i: u32, n: u32, p: *const u32, off: u32) {
    let v = unsafe { dwords(p, n) };
    rec(this, |s| s.set_const(1, i, off, &v));
    fwd!(X_GN, fn(u32, u32, *const u32, u32), this, i, n, p, off)
}

macro_rules! view_wrappers {
    ($(($name:ident, $slot:expr)),*) => {
        $(
            unsafe extern "system" fn $name(this: *mut c_void, i: u32, va: u64) {
                let side = (($slot - X_VIEW_FIRST) % 2) as u8;
                rec(this, |s| { s.args.insert((side, i), RootArg::View($slot, va)); });
                fwd!($slot, fn(u32, u64), this, i, va)
            }
        )*
        fn view_wrapper(slot: usize) -> usize {
            match slot { $($slot => $name as usize,)* _ => 0 }
        }
    };
}
view_wrappers!((w_v41, 41), (w_v42, 42), (w_v43, 43), (w_v44, 44), (w_v45, 45), (w_v46, 46));

unsafe extern "system" fn w_ib(this: *mut c_void, p: *const u8) {
    let v = if p.is_null() { None } else { Some(unsafe { arr::<16>(p, 0) }) };
    rec(this, |s| s.ib = Some(v));
    fwd!(X_IB, fn(*const u8), this, p)
}

unsafe extern "system" fn w_vb(this: *mut c_void, start: u32, n: u32, p: *const u8) {
    let v: Vec<Option<[u8; 16]>> = (0..n as usize).map(|i| if p.is_null() { None } else { Some(unsafe { arr::<16>(p, i) }) }).collect();
    rec(this, |s| s.set_vbs(start, &v));
    fwd!(X_VB, fn(u32, u32, *const u8), this, start, n, p)
}

unsafe extern "system" fn w_so(this: *mut c_void, start: u32, n: u32, p: *const u8) {
    let v: Vec<Option<[u8; 24]>> = (0..n as usize).map(|i| if p.is_null() { None } else { Some(unsafe { arr::<24>(p, i) }) }).collect();
    rec(this, |s| s.set_so(start, &v));
    fwd!(X_SO, fn(u32, u32, *const u8), this, start, n, p)
}

unsafe extern "system" fn w_rt(this: *mut c_void, n: u32, h: *const usize, single: i32, dsv: *const usize) {
    let cnt = if single != 0 { n.min(1) } else { n } as usize;
    let handles: Vec<usize> = if h.is_null() { Vec::new() } else { (0..cnt).map(|i| unsafe { *h.add(i) }).collect() };
    let d = if dsv.is_null() { None } else { Some(unsafe { *dsv }) };
    let t = unsafe { snapshot_targets(this, Targets { n, handles, single, dsv: d }) };
    rec(this, |s| s.targets = Some(t));
    fwd!(X_RT, fn(u32, *const usize, i32, *const usize), this, n, h, single, dsv)
}

unsafe extern "system" fn w_depth_bounds(this: *mut c_void, lo: f32, hi: f32) {
    rec(this, |s| s.depth_bounds = Some((lo, hi)));
    fwd!(X_DEPTH_BOUNDS, fn(f32, f32), this, lo, hi)
}

unsafe extern "system" fn w_depth_bounds1(this: *mut c_void, lo: f32, hi: f32) {
    rec(this, |s| s.depth_bounds = Some((lo, hi)));
    fwd!(X_DEPTH_BOUNDS_LIST1, fn(f32, f32), this, lo, hi)
}

unsafe extern "system" fn w_sample_positions(this: *mut c_void, samples: u32, pixels: u32, p: *const u8) {
    let v = unsafe { bytes(p, samples as usize * pixels as usize * 2) };
    rec(this, |s| s.sample_positions = Some((samples, pixels, v)));
    fwd!(X_SAMPLE_POSITIONS, fn(u32, u32, *const u8), this, samples, pixels, p)
}

unsafe extern "system" fn w_shading_rate(this: *mut c_void, rate: u32, comb: *const u32) {
    let c = if comb.is_null() { None } else { Some(unsafe { [core::ptr::read_unaligned(comb), core::ptr::read_unaligned(comb.add(1))] }) };
    rec(this, |s| s.shading_rate = Some((rate, c)));
    fwd!(X_SHADING_RATE, fn(u32, *const u32), this, rate, comb)
}

unsafe extern "system" fn w_shading_rate_image(this: *mut c_void, img: *mut c_void) {
    rec(this, |s| s.shading_rate_image = Some(img as usize));
    fwd!(X_SHADING_RATE_IMAGE, fn(*mut c_void), this, img)
}

const MAX_RT: usize = 8;
const TARGET_LISTS_PER_HEAP: u32 = 128;

struct TargetChunk {
    heap: usize,
    start: usize,
    inc: usize,
    used: u32,
    cap: u32,
}

#[derive(Default)]
struct TargetSlots {
    device: usize,
    regions: HashMap<usize, (usize, usize)>,
    chunks: [Option<TargetChunk>; 2],
    heaps: Vec<usize>,
}

static TSLOTS: Mutex<Option<TargetSlots>> = Mutex::new(None);

unsafe fn heap_cpu_start(h: *mut c_void) -> usize {
    let f: unsafe extern "system" fn(*mut c_void, *mut u64) -> u64 = unsafe { core::mem::transmute(vslot(h, HEAP_CPU_START)) };
    let mut out = 0u64;
    let p = &mut out as *mut u64;
    let rax = unsafe { f(h, p) };
    (if rax == p as u64 { out } else { rax }) as usize
}

pub fn chunk_fits(used: u32, count: u32, cap: u32) -> bool {
    used.checked_add(count).map(|e| e <= cap).unwrap_or(false)
}

unsafe fn region(dev: *mut c_void, ts: &mut TargetSlots, kind: usize, count: u32) -> Option<usize> {
    let fits = ts.chunks[kind].as_ref().map(|c| chunk_fits(c.used, count, c.cap)).unwrap_or(false);
    if !fits {
        let ty = if kind == 0 { HEAP_TYPE_RTV } else { HEAP_TYPE_DSV };
        let cap = count * TARGET_LISTS_PER_HEAP;
        let desc = [ty, cap, 0, 0];
        let create: unsafe extern "system" fn(*mut c_void, *const u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_HEAP)) };
        let mut h: *mut c_void = core::ptr::null_mut();
        let hr = unsafe { create(dev, desc.as_ptr(), &IID_DESCRIPTOR_HEAP, &mut h) };
        if hr < 0 || h.is_null() {
            if once(16) {
                record("d3d12_xs", "cl2_state", &format!("render-target snapshot heap type {ty} FAILED hr={hr:#010x}"));
            }
            return None;
        }
        let inc: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { core::mem::transmute(vslot(dev, DEV_INCREMENT)) };
        let c = TargetChunk { heap: h as usize, start: unsafe { heap_cpu_start(h) }, inc: unsafe { inc(dev, ty) } as usize, used: 0, cap };
        ts.heaps.push(c.heap);
        ts.chunks[kind] = Some(c);
    }
    let c = ts.chunks[kind].as_mut()?;
    let at = c.start + c.used as usize * c.inc;
    c.used += count;
    Some(at)
}

unsafe fn snapshot_targets(list: *mut c_void, t: Targets) -> Targets {
    let mut g = TSLOTS.lock().unwrap_or_else(|e| e.into_inner());
    let ts = g.get_or_insert_with(TargetSlots::default);
    if ts.device == 0 {
        ts.device = unsafe { device_of(list) } as usize;
    }
    let dev = ts.device as *mut c_void;
    if dev.is_null() {
        return t;
    }
    let reg = match ts.regions.get(&(list as usize)) {
        Some(r) => *r,
        None => {
            let (Some(r), Some(d)) = (unsafe { region(dev, ts, 0, MAX_RT as u32) }, unsafe { region(dev, ts, 1, 1) }) else { return t };
            ts.regions.insert(list as usize, (r, d));
            (r, d)
        }
    };
    let rinc = ts.chunks[0].as_ref().map(|c| c.inc).unwrap_or(0);
    let copy: unsafe extern "system" fn(*mut c_void, u32, usize, usize, u32) = unsafe { core::mem::transmute(vslot(dev, DEV_COPY_SIMPLE)) };
    let n = (t.n as usize).min(MAX_RT);
    let mut out = Targets { n: t.n, handles: Vec::new(), single: t.single, dsv: None };
    if !t.handles.is_empty() && n > 0 {
        if t.single != 0 {
            if t.handles[0] != reg.0 {
                unsafe { copy(dev, n as u32, reg.0, t.handles[0], HEAP_TYPE_RTV) };
            }
        } else {
            for (i, &h) in t.handles.iter().take(n).enumerate() {
                if h != reg.0 + i * rinc {
                    unsafe { copy(dev, 1, reg.0 + i * rinc, h, HEAP_TYPE_RTV) };
                }
            }
        }
        out.handles = vec![reg.0];
        out.single = 1;
    }
    if let Some(d) = t.dsv {
        if d != reg.1 {
            unsafe { copy(dev, 1, reg.1, d, HEAP_TYPE_DSV) };
        }
        out.dsv = Some(reg.1);
    }
    out
}

static PSO_BIAS: Mutex<Option<HashMap<usize, (usize, Option<PsoBias>)>>> = Mutex::new(None);

fn pso_bias(pso: usize) -> Option<PsoBias> {
    if pso == 0 {
        return None;
    }
    let id = crate::device_wrap::gfx_pso_identity(pso);
    if id == 0 {
        return None;
    }
    let mut g = PSO_BIAS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some((i, v)) = m.get(&pso) {
        if *i == id {
            return *v;
        }
    }
    let v = crate::device_wrap::gfx_pso_desc(pso).and_then(|(d, _)| pso_bias_from_desc(&d));
    m.insert(pso, (id, v));
    v
}

unsafe fn has_list9(list: *mut c_void) -> bool {
    if let Some(v) = with(list, |s| s.list9) {
        return v;
    }
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(list, 0)) };
    let mut p: *mut c_void = core::ptr::null_mut();
    let ok = unsafe { qi(list, &IID_LIST9, &mut p) } >= 0 && !p.is_null();
    if ok {
        unsafe { release(p) };
    }
    with(list, |s| s.list9 = Some(ok));
    ok
}

unsafe fn pc_depth_bias(list: *mut c_void, v: (f32, f32, f32)) -> bool {
    if !unsafe { has_list9(list) } || unsafe { crate::fill::list_type(list) } == LIST_TYPE_BUNDLE {
        return false;
    }
    let f: unsafe extern "system" fn(*mut c_void, f32, f32, f32) = unsafe { core::mem::transmute(pc_list_slot(list, PC_RS_SET_DEPTH_BIAS)) };
    unsafe { f(list, v.0, v.1, v.2) };
    true
}

unsafe fn pc_set_pso(list: *mut c_void, pso: usize) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_PSO)) };
    unsafe { f(list, pso as *mut c_void) };
}

struct Variant {
    pso: usize,
    id: usize,
    _keep: Box<dyn Send + Sync>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VariantKey {
    pub clip: bool,
    pub dynamic: bool,
    pub bias: Option<(i32, u32, u32)>,
    pub rect: bool,
}

pub fn variant_desc(d: &mut [u8], k: VariantKey) -> bool {
    d[DESC_DEPTH_CLIP..DESC_DEPTH_CLIP + 4].copy_from_slice(&(k.clip as u32).to_le_bytes());
    let f = u32::from_le_bytes(d[DESC_FLAGS..DESC_FLAGS + 4].try_into().unwrap());
    let f = if k.dynamic { f | PSO_FLAG_DYNAMIC_DEPTH_BIAS } else { f & !PSO_FLAG_DYNAMIC_DEPTH_BIAS };
    d[DESC_FLAGS..DESC_FLAGS + 4].copy_from_slice(&f.to_le_bytes());
    if let Some((b, c, s)) = k.bias {
        d[DESC_DEPTH_BIAS..DESC_DEPTH_BIAS + 4].copy_from_slice(&b.to_le_bytes());
        d[DESC_DEPTH_BIAS + 4..DESC_DEPTH_BIAS + 8].copy_from_slice(&c.to_le_bytes());
        d[DESC_DEPTH_BIAS + 8..DESC_DEPTH_BIAS + 12].copy_from_slice(&s.to_le_bytes());
    }
    !k.rect || crate::rectlist::expand(d)
}

pub fn static_key(x: DepthBiasX) -> VariantKey {
    let (_, c, s) = x.pc_values();
    VariantKey { clip: x.clip, dynamic: false, bias: Some((x.bias, c.to_bits(), s.to_bits())), rect: false }
}

pub fn static_matches(pb: &PsoBias, x: DepthBiasX) -> bool {
    let (_, c, s) = x.pc_values();
    !pb.dynamic && pb.clip == x.clip && pb.bias == x.bias && pb.clamp.to_bits() == c.to_bits() && pb.slope.to_bits() == s.to_bits()
}

static VARIANTS: Mutex<Option<HashMap<(usize, VariantKey), Variant>>> = Mutex::new(None);
static RECT_FAILED: Mutex<Option<HashMap<(usize, VariantKey), usize>>> = Mutex::new(None);

unsafe fn pso_variant(pso: usize, k: VariantKey) -> usize {
    let id = crate::device_wrap::gfx_pso_identity(pso);
    if k.rect && RECT_FAILED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&(pso, k))) == Some(&id) {
        return 0;
    }
    let mut g = VARIANTS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(&(pso, k)) {
        if v.id == id {
            return v.pso;
        }
    }
    let dev = REAL_DEVICE.load(Ordering::Relaxed) as *mut c_void;
    let Some((mut d, keep)) = crate::device_wrap::gfx_pso_desc(pso) else { return 0 };
    if dev.is_null() || d.len() < DESC_FLAGS + 4 {
        return 0;
    }
    let fail = |m: &mut HashMap<(usize, VariantKey), usize>| if k.rect { m.insert((pso, k), id); };
    if !variant_desc(&mut d, k) {
        fail(RECT_FAILED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new));
        return 0;
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_GFX_PSO)) };
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { f(dev, d.as_ptr(), &IID_PSO, &mut out) };
    if hr < 0 || out.is_null() {
        fail(RECT_FAILED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new));
        if k.rect {
            crate::rectlist::refuse_desc(&d, &format!("the expanded pipeline of PSO {pso:#x} was refused by the driver (hr={hr:#010x})"));
        }
        if once(16) {
            record("d3d12_xs", "cl2_state", &format!("PSO variant {k:?} of {pso:#x} FAILED hr={hr:#010x}"));
        }
        return 0;
    }
    m.insert((pso, k), Variant { pso: out as usize, id, _keep: Box::new(keep) });
    out as usize
}

fn is_bundle(list: *mut c_void) -> bool {
    unsafe { crate::fill::list_type(list) == LIST_TYPE_BUNDLE }
}

unsafe fn dynamic_usable(list: *mut c_void) -> bool {
    !is_bundle(list) && unsafe { has_list9(list) }
}

unsafe fn apply_pso_bias(list: *mut c_void, pso: usize) {
    let Some(b) = pso_bias(pso).filter(|b| b.dynamic) else { return };
    if unsafe { dynamic_usable(list) } {
        unsafe { pc_depth_bias(list, (b.bias as f32, b.clamp, b.slope)) };
        return;
    }
    let v = unsafe { pso_variant(pso, VariantKey { clip: b.clip, dynamic: false, bias: None, rect: false }) };
    if v != 0 {
        unsafe { pc_set_pso(list, v) };
        with(list, |s| s.variant_bound = true);
    }
}

unsafe extern "system" fn w_pso(this: *mut c_void, pso: *mut c_void) {
    let graphics = pso_bias(pso as usize).is_some();
    let stream_graphics = !graphics && crate::dbcount::stream_rec(pso as usize).is_some_and(|r| r.is_graphics());
    rec(this, |s| {
        s.pso = Some(pso as usize);
        if graphics || stream_graphics {
            s.variant_bound = false;
        }
        if graphics {
            s.depth_bias_x = None;
        }
    });
    fwd!(X_PSO, fn(*mut c_void), this, pso);
    unsafe { apply_pso_bias(this, pso as usize) };
    unsafe { apply_rect(this) };
}

fn is_rect(list: *mut c_void) -> bool {
    with(list, |s| s.topology == Some(crate::rectlist::RECTLIST))
}

unsafe fn wanted_key(list: *mut c_void, pso: usize) -> Option<VariantKey> {
    let pb = pso_bias(pso)?;
    let x = with(list, |s| s.depth_bias_x);
    let dynamic = pb.dynamic && unsafe { dynamic_usable(list) };
    let plain = VariantKey { clip: pb.clip, dynamic: pb.dynamic, bias: None, rect: false };
    let k = match x {
        Some(x) if !dynamic => if static_matches(&pb, x) { plain } else { static_key(x) },
        Some(x) => VariantKey { clip: x.clip, dynamic: true, bias: None, rect: false },
        None if pb.dynamic && !dynamic => VariantKey { clip: pb.clip, dynamic: false, bias: None, rect: false },
        None => plain,
    };
    Some(VariantKey { rect: is_rect(list), ..k })
}

unsafe fn apply_rect(list: *mut c_void) {
    if !is_rect(list) {
        return;
    }
    let pso = with(list, |s| s.pso).unwrap_or(0);
    if pso == 0 {
        return;
    }
    let v = match unsafe { wanted_key(list, pso) } {
        Some(k) => unsafe { pso_variant(pso, k) },
        None => match crate::dbcount::stream_rec(pso) {
            Some(r) if r.is_graphics() => unsafe { stream_rect_variant(list, pso, &r) },
            Some(_) => 0,
            None if crate::device_x::is_compute(pso) || crate::pso_async::is_compute_placeholder(pso) => 0,
            None => {
                crate::rectlist::refuse(0, pso as u64, &format!("PSO {pso:#x} has no recorded PC description or pipeline stream (unknown pipeline)"));
                0
            }
        },
    };
    if v != 0 {
        unsafe { pc_set_pso(list, v) };
        with(list, |s| s.variant_bound = true);
    }
}

struct StreamVariant {
    id: usize,
    pso: usize,
    _keep: Option<Box<dyn Send + Sync>>,
}

static STREAM_RECT: Mutex<Option<HashMap<usize, StreamVariant>>> = Mutex::new(None);

unsafe fn stream_rect_variant(list: *mut c_void, pso: usize, r: &std::sync::Arc<crate::dbcount::StreamRec>) -> usize {
    let id = std::sync::Arc::as_ptr(r) as usize;
    let mut g = STREAM_RECT.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(&pso).filter(|v| v.id == id) {
        return v.pso;
    }
    let v = match crate::rectlist::expand_stream(&r.stream, &r.subs) {
        None => StreamVariant { id, pso: 0, _keep: None },
        Some((s, subs, gs)) => match unsafe { crate::dbcount::create_stream_pso(list, &s) } {
            Ok(p) => StreamVariant { id, pso: p, _keep: Some(Box::new((s, gs, r.clone()))) },
            Err(e) => {
                crate::rectlist::refuse_stream(&s, &subs, &format!("the expanded pipeline stream of PSO {pso:#x} was refused by the driver ({e})"));
                StreamVariant { id, pso: 0, _keep: None }
            }
        },
    };
    let p = v.pso;
    m.insert(pso, v);
    p
}

unsafe fn apply_depth_bias_x(this: *mut c_void, x: DepthBiasX) {
    let pso = with(this, |s| s.pso).unwrap_or(0);
    let Some(pb) = pso_bias(pso) else {
        if pso != 0 && once(16) {
            record("d3d12_xs", "cl2_state", &format!("RSSetDepthBiasX: bound PSO {pso:#x} has no recorded PC desc; values kept for the next PSO-less reapply"));
        }
        return;
    };
    let bound = with(this, |s| s.variant_bound);
    if !(pb.dynamic && unsafe { dynamic_usable(this) }) {
        if static_matches(&pb, x) {
            if bound {
                unsafe { pc_set_pso(this, pso) };
                with(this, |s| s.variant_bound = false);
            }
            return;
        }
        let v = unsafe { pso_variant(pso, static_key(x)) };
        if v != 0 {
            unsafe { pc_set_pso(this, v) };
            with(this, |s| s.variant_bound = true);
        }
        return;
    }
    if x.clip != pb.clip {
        let v = unsafe { pso_variant(pso, VariantKey { clip: x.clip, dynamic: true, bias: None, rect: false }) };
        if v != 0 {
            unsafe { pc_set_pso(this, v) };
            with(this, |s| s.variant_bound = true);
        }
    } else if bound {
        unsafe { pc_set_pso(this, pso) };
        with(this, |s| s.variant_bound = false);
    }
    unsafe { pc_depth_bias(this, x.pc_values()) };
}

unsafe extern "system" fn w_depth_bias_x(this: *mut c_void, bias: i32, clamp: f32, slope: f32, clip: u32) {
    let x = DepthBiasX { bias, clamp, slope, clip: clip != 0 };
    rec(this, |s| s.depth_bias_x = Some(x));
    unsafe { apply_depth_bias_x(this, x) };
    unsafe { apply_rect(this) };
}

pub static QUERY_DXIL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/query_accum.dxil"));

pub const CHUNK_SLOTS: u32 = 4096;
pub const PART_BYTES: u64 = 128;
const ST_COMMON: u32 = 0;
const ST_UAV: u32 = 0x8;
const ST_COPY_DEST: u32 = 0x400;
const ST_COPY_SOURCE: u32 = 0x800;
const PC_BARRIER: usize = 26;
const PC_DISPATCH: usize = 14;
const PC_COPY_BUFFER: usize = 15;
const PC_SET_CROOT: usize = 29;
const PC_SET_CCONSTS: usize = 35;
const PC_SET_CUAV: usize = 41;
const PC_RESOLVE_QUERY: usize = 54;
const PC_SET_PREDICATION: usize = 55;
const DEV_CREATE_COMPUTE_PSO: usize = 11;
const DEV_CREATE_ROOT_SIGNATURE: usize = 16;
const DEV_CREATE_COMMITTED: usize = 27;
const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a_6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };
const IID_RESOURCE: Guid = Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };

#[derive(Default)]
pub struct PartStore {
    pub chunks: Vec<(usize, u64)>,
    next: u32,
    free: Vec<u32>,
    refs: HashMap<u32, u32>,
    held: HashMap<usize, Vec<u32>>,
    uses: HashMap<u32, HashMap<usize, u64>>,
    pending: Vec<(u32, Vec<(usize, u64)>)>,
    pub completed: HashMap<(usize, u32), (u32, Vec<u32>)>,
}

impl PartStore {
    pub fn take(&mut self, done: &dyn Fn(usize, u64) -> bool) -> (u32, bool) {
        let mut back = Vec::new();
        self.pending.retain(|(s, u)| {
            let ready = u.iter().all(|&(f, v)| done(f, v));
            if ready {
                back.push(*s);
            }
            !ready
        });
        self.free.extend(back);
        let s = self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next - 1
        });
        (s, (s / CHUNK_SLOTS) as usize >= self.chunks.len())
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

    fn drop_ref(&mut self, s: u32) {
        if let Some(r) = self.refs.get_mut(&s) {
            *r -= 1;
            if *r == 0 {
                self.refs.remove(&s);
                let u: Vec<(usize, u64)> = self.uses.remove(&s).map(|m| m.into_iter().collect()).unwrap_or_default();
                if u.is_empty() {
                    self.free.push(s);
                } else {
                    self.pending.push((s, u));
                }
            }
        }
    }

    pub fn holds_any(&self, keys: &[usize]) -> bool {
        keys.iter().any(|k| self.held.get(k).map(|v| !v.is_empty()).unwrap_or(false))
    }

    pub fn note_exec(&mut self, keys: &[usize], fence: usize, value: u64) {
        for k in keys {
            for &s in self.held.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
                let e = self.uses.entry(s).or_default().entry(fence).or_insert(0);
                *e = (*e).max(value);
            }
        }
    }

    pub fn is_pending(&self, s: u32) -> bool {
        self.pending.iter().any(|p| p.0 == s)
    }

    pub fn release(&mut self, key: usize) {
        for s in self.held.remove(&key).unwrap_or_default() {
            self.drop_ref(s);
        }
    }

    pub fn complete(&mut self, q: ActiveQuery) {
        for &s in &q.parts {
            *self.refs.entry(s).or_insert(0) += 1;
        }
        let old = if q.parts.is_empty() { self.completed.remove(&(q.heap, q.index)) } else { self.completed.insert((q.heap, q.index), (q.ty, q.parts)) };
        for s in old.map(|o| o.1).unwrap_or_default() {
            self.drop_ref(s);
        }
    }

    pub fn is_live(&self, s: u32) -> bool {
        self.refs.contains_key(&s)
    }

    pub fn loc(&self, s: u32) -> Option<(usize, u64, u64)> {
        self.chunks.get((s / CHUNK_SLOTS) as usize).map(|&(r, va)| (r, va, (s % CHUNK_SLOTS) as u64 * PART_BYTES))
    }

    pub fn plan(&self, heap: usize, ty: u32, start: u32, n: u32, off: u64) -> Vec<(u32, u64, u32, u32, Vec<u32>)> {
        let Some((words, mode)) = query_words(ty) else { return Vec::new() };
        (start..start.saturating_add(n))
            .filter_map(|i| self.completed.get(&(heap, i)).filter(|e| e.0 == ty).map(|e| (i, off + (i - start) as u64 * words as u64 * 8, words, mode, e.1.clone())))
            .collect()
    }
}

static STORE: Mutex<Option<PartStore>> = Mutex::new(None);

fn store<R>(f: impl FnOnce(&mut PartStore) -> R) -> R {
    f(STORE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(PartStore::default))
}

#[derive(Clone, Copy)]
struct QueryPipe {
    rs: usize,
    pso: usize,
}

static QPIPE: Mutex<Option<Option<QueryPipe>>> = Mutex::new(None);

unsafe fn query_pipe(list: *mut c_void) -> Option<QueryPipe> {
    let mut g = QPIPE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = *g {
        return p;
    }
    let p = unsafe { build_query_pipe(device_of(list)) };
    *g = Some(p);
    p
}

unsafe fn build_query_pipe(dev: *mut c_void) -> Option<QueryPipe> {
    if dev.is_null() {
        return None;
    }
    let code = QUERY_DXIL;
    let crs: unsafe extern "system" fn(*mut c_void, u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_ROOT_SIGNATURE)) };
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { crs(dev, 0, code.as_ptr(), code.len(), &IID_ROOT_SIGNATURE, &mut rs) };
    if hr < 0 || rs.is_null() {
        record("d3d12_xs", "cl2_state", &format!("query accumulation root signature FAILED hr={hr:#010x}"));
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
        record("d3d12_xs", "cl2_state", &format!("query accumulation PSO FAILED hr={hr:#010x}"));
        return None;
    }
    Some(QueryPipe { rs: rs as usize, pso: pso as usize })
}

unsafe fn create_chunk(dev: *mut c_void) -> Option<(usize, u64)> {
    let heap: [u32; 5] = [1, 0, 0, 1, 1];
    let mut rd = [0u8; 56];
    rd[0..4].copy_from_slice(&1u32.to_le_bytes());
    rd[0x10..0x18].copy_from_slice(&(CHUNK_SLOTS as u64 * PART_BYTES).to_le_bytes());
    rd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    rd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    rd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    rd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    rd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    rd[0x30..0x34].copy_from_slice(&4u32.to_le_bytes());
    let cc: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_COMMITTED)) };
    let mut buf: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { cc(dev, heap.as_ptr(), 0, rd.as_ptr(), ST_COMMON, core::ptr::null(), &IID_RESOURCE, &mut buf) };
    if hr < 0 || buf.is_null() {
        record("d3d12_xs", "cl2_state", &format!("query partial-result chunk FAILED hr={hr:#010x}"));
        return None;
    }
    let va: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(vslot(buf, 11)) };
    Some((buf as usize, unsafe { va(buf) }))
}

unsafe fn new_slot(list: *mut c_void, key: usize) -> Option<(u32, usize, u64, u64)> {
    store(|st| {
        let (s, grow) = st.take(&|f, v| unsafe { fence_done(f, v) });
        if grow {
            match unsafe { create_chunk(device_of(list)) } {
                Some(c) => st.chunks.push(c),
                None => {
                    st.untake(s);
                    return None;
                }
            }
        }
        st.hold(key, s);
        st.loc(s).map(|(r, va, off)| (s, r, va, off))
    })
}

unsafe fn uav_barrier(list: *mut c_void, res: usize) {
    let mut b = [0u8; 32];
    b[0..4].copy_from_slice(&2u32.to_le_bytes());
    b[8..16].copy_from_slice(&(res as u64).to_le_bytes());
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = unsafe { core::mem::transmute(pc_list_slot(list, PC_BARRIER)) };
    unsafe { f(list, 1, b.as_ptr()) };
}

unsafe fn pc_no_predication(list: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u32) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_PREDICATION)) };
    unsafe { f(list, core::ptr::null_mut(), 0, 0) };
}

unsafe fn pc_resolve(list: *mut c_void, heap: usize, ty: u32, index: u32, dst: usize, off: u64) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, *mut c_void, u64) = unsafe { core::mem::transmute(pc_list_slot(list, PC_RESOLVE_QUERY)) };
    unsafe { f(list, heap as *mut c_void, ty, index, 1, dst as *mut c_void, off) };
}

unsafe fn suspend_queries(list: *mut c_void) {
    let _ij = unsafe { crate::inject::scope(list, crate::inject::QUERY) };
    let qs = with(list, |s| s.queries.clone());
    if qs.is_empty() {
        return;
    }
    for q in &qs {
        fwd!(X_END_QUERY, fn(*mut c_void, u32, u32), list, q.heap as *mut c_void, q.ty, q.index);
    }
    let key = crate::list_split::origin_of(list);
    unsafe { pc_no_predication(list) };
    let mut slots = Vec::new();
    for q in &qs {
        let got = unsafe { new_slot(list, key) };
        if let Some((_, res, _, off)) = got {
            unsafe {
                crate::fill::barrier_pub(list, res, ST_COMMON, ST_COPY_DEST);
                pc_resolve(list, q.heap, q.ty, q.index, res, off);
                crate::fill::barrier_pub(list, res, ST_COPY_DEST, ST_COMMON);
            }
        } else if once(16) {
            record("d3d12_xs", "cl2_state", &format!("query {}#{} spanning a split: no partial-result slot, the part before the split is lost", q.heap, q.index));
        }
        slots.push(got.map(|g| g.0));
    }
    with(list, |s| {
        for (q, slot) in s.queries.iter_mut().zip(slots) {
            if let Some(x) = slot {
                q.parts.push(x);
            }
        }
        s.suspended = true;
    });
}

unsafe fn resume_queries(list: *mut c_void) {
    let qs = with(list, |s| {
        let sus = core::mem::replace(&mut s.suspended, false);
        if sus { s.queries.clone() } else { Vec::new() }
    });
    for q in qs {
        fwd!(X_BEGIN_QUERY, fn(*mut c_void, u32, u32), list, q.heap as *mut c_void, q.ty, q.index);
    }
}

pub(crate) unsafe fn pause_queries(list: *mut c_void, occlusion_only: bool) -> Vec<(usize, u32, u32)> {
    let _ij = unsafe { crate::inject::scope(list, crate::inject::QUERY) };
    let qs: Vec<ActiveQuery> = with(list, |s| if s.suspended { Vec::new() } else { s.queries.iter().filter(|q| !occlusion_only || q.ty <= 1).cloned().collect() });
    if qs.is_empty() {
        return Vec::new();
    }
    for q in &qs {
        fwd!(X_END_QUERY, fn(*mut c_void, u32, u32), list, q.heap as *mut c_void, q.ty, q.index);
    }
    let key = crate::list_split::origin_of(list);
    let pred = predication_of(list);
    unsafe { pc_no_predication(list) };
    for q in &qs {
        match unsafe { new_slot(list, key) } {
            Some((slot, res, _, off)) => {
                unsafe {
                    crate::fill::barrier_pub(list, res, ST_COMMON, ST_COPY_DEST);
                    pc_resolve(list, q.heap, q.ty, q.index, res, off);
                    crate::fill::barrier_pub(list, res, ST_COPY_DEST, ST_COMMON);
                }
                with(list, |s| {
                    if let Some(a) = s.queries.iter_mut().find(|a| a.heap == q.heap && a.index == q.index) {
                        a.parts.push(slot);
                    }
                });
            }
            None => {
                if once(16) {
                    record("d3d12_xs", "cl2_state", &format!("query {}#{} paused: no partial-result slot, the part before the pause is lost", q.heap, q.index));
                }
            }
        }
    }
    if let Some((r, o, op)) = pred {
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u32) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_PREDICATION)) };
        unsafe { f(list, r as *mut c_void, o, op) };
    }
    qs.iter().map(|q| (q.heap, q.ty, q.index)).collect()
}

pub(crate) unsafe fn unpause_queries(list: *mut c_void, keys: &[(usize, u32, u32)]) {
    for &(h, t, i) in keys {
        fwd!(X_BEGIN_QUERY, fn(*mut c_void, u32, u32), list, h as *mut c_void, t, i);
    }
}

pub(crate) fn real_device() -> *mut c_void {
    REAL_DEVICE.load(Ordering::Relaxed) as *mut c_void
}

pub(crate) unsafe fn bound_variant_key(list: *mut c_void) -> Option<VariantKey> {
    let (pso, bound, x) = with(list, |s| (s.pso.unwrap_or(0), s.variant_bound, s.depth_bias_x));
    if !bound {
        return None;
    }
    let rect = is_rect(list);
    let Some(pb) = pso_bias(pso) else {
        return (rect && crate::dbcount::stream_rec(pso).is_some()).then_some(VariantKey { clip: false, dynamic: false, bias: None, rect: true });
    };
    let dynamic = pb.dynamic && unsafe { dynamic_usable(list) };
    Some(match x {
        Some(x) if !dynamic => VariantKey { rect, ..static_key(x) },
        Some(x) => VariantKey { clip: x.clip, dynamic: true, bias: None, rect },
        None if rect => return unsafe { wanted_key(list, pso) },
        None => VariantKey { clip: pb.clip, dynamic: false, bias: None, rect },
    })
}

pub(crate) unsafe fn rebind_pso(list: *mut c_void) {
    let (pso, x) = with(list, |s| (s.pso, s.depth_bias_x));
    let Some(p) = pso else { return };
    unsafe { pc_set_pso(list, p) };
    with(list, |s| s.variant_bound = false);
    unsafe { apply_pso_bias(list, p) };
    if let Some(x) = x {
        unsafe { apply_depth_bias_x(list, x) };
    }
    unsafe { apply_rect(list) };
}

#[cfg(test)]
pub(crate) static TEST_PREV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_install(vt: &[AtomicUsize]) -> Vec<usize> {
    let saved: Vec<usize> = PREV.iter().map(|p| p.swap(0, Ordering::Relaxed)).collect();
    install(vt, &[]);
    saved
}

#[cfg(test)]
pub(crate) fn test_restore(saved: Vec<usize>) {
    for (i, v) in saved.into_iter().enumerate() {
        PREV[i].store(v, Ordering::Relaxed);
    }
}

unsafe fn accumulate(list: *mut c_void, heap: usize, ty: u32, dst: *mut c_void, plan: &[(u32, u64, u32, u32, Vec<u32>)]) {
    let _ij = unsafe { crate::inject::scope(list, crate::inject::QUERY) };
    let Some(p) = (unsafe { query_pipe(list) }) else { return };
    let key = crate::list_split::origin_of(list);
    let cp: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *mut c_void, u64, u64) = unsafe { core::mem::transmute(pc_list_slot(list, PC_COPY_BUFFER)) };
    let srs: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_CROOT)) };
    let sc: unsafe extern "system" fn(*mut c_void, u32, u32, *const u32, u32) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_CCONSTS)) };
    let su: unsafe extern "system" fn(*mut c_void, u32, u64) = unsafe { core::mem::transmute(pc_list_slot(list, PC_SET_CUAV)) };
    let d: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = unsafe { core::mem::transmute(pc_list_slot(list, PC_DISPATCH)) };
    unsafe {
        pc_no_predication(list);
        for (index, off, words, mode, parts) in plan {
            let Some((_, sres, sva, soff)) = new_slot(list, key) else { continue };
            let locs: Vec<(usize, u64, u64)> = store(|st| {
                parts.iter().filter_map(|&s| {
                    st.hold(key, s);
                    st.loc(s)
                }).collect()
            });
            let len = *words as u64 * 8;
            crate::fill::barrier_pub(list, sres, ST_COMMON, ST_COPY_DEST);
            pc_resolve(list, heap, ty, *index, sres, soff);
            crate::fill::barrier_pub(list, sres, ST_COPY_DEST, ST_UAV);
            srs(list, p.rs as *mut c_void);
            pc_set_pso(list, p.pso);
            su(list, 2, sva);
            for (pres, pva, poff) in locs {
                if pres != sres {
                    crate::fill::barrier_pub(list, pres, ST_COMMON, ST_UAV);
                }
                su(list, 1, pva);
                let k = [poff as u32, soff as u32, *words, *mode];
                sc(list, 0, 4, k.as_ptr(), 0);
                d(list, 1, 1, 1);
                uav_barrier(list, sres);
                if pres != sres {
                    crate::fill::barrier_pub(list, pres, ST_UAV, ST_COMMON);
                }
            }
            crate::fill::barrier_pub(list, sres, ST_UAV, ST_COPY_SOURCE);
            cp(list, dst, *off, sres as *mut c_void, soff, len);
            crate::fill::barrier_pub(list, sres, ST_COPY_SOURCE, ST_COMMON);
        }
        reapply(list);
    }
}

unsafe extern "system" fn w_begin_query(this: *mut c_void, heap: *mut c_void, ty: u32, index: u32) {
    fwd!(X_BEGIN_QUERY, fn(*mut c_void, u32, u32), this, heap, ty, index);
    if query_words(ty).is_some() {
        let gone = rec(this, |s| s.begin_query(heap as usize, ty, index));
        store(|st| {
            for q in gone {
                st.complete(q);
            }
        });
    }
}

unsafe extern "system" fn w_end_query(this: *mut c_void, heap: *mut c_void, ty: u32, index: u32) {
    fwd!(X_END_QUERY, fn(*mut c_void, u32, u32), this, heap, ty, index);
    if let Some(q) = rec(this, |s| s.end_query(heap as usize, index)) {
        store(|st| st.complete(q));
    }
}

unsafe extern "system" fn w_resolve_query(this: *mut c_void, heap: *mut c_void, ty: u32, start: u32, n: u32, dst: *mut c_void, off: u64) {
    let plan = if dst.is_null() { Vec::new() } else { store(|st| st.plan(heap as usize, ty, start, n, off)) };
    if plan.is_empty() {
        fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64), this, heap, ty, start, n, dst, off);
        return;
    }
    let stride = query_words(ty).map(|w| w.0 as u64 * 8).unwrap_or(0);
    let skip: Vec<u32> = plan.iter().map(|e| e.0).collect();
    for (s, c) in resolve_runs(start, n, &skip) {
        fwd!(X_RESOLVE_QUERY, fn(*mut c_void, u32, u32, u32, *mut c_void, u64), this, heap, ty, s, c, dst, off + (s - start) as u64 * stride);
    }
    unsafe { accumulate(this, heap as usize, ty, dst, &plan) };
}

pub fn resolve_runs(start: u32, n: u32, skip: &[u32]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    for i in start..start.saturating_add(n) {
        if skip.contains(&i) {
            out.extend(run.take());
        } else {
            run = Some(run.map(|(s, c)| (s, c + 1)).unwrap_or((i, 1)));
        }
    }
    out.extend(run);
    out
}

pub(crate) unsafe fn signal_queue(queue: *mut c_void, list: *mut c_void) -> Option<(usize, u64)> {
    let (fence, value) = unsafe { queue_fence(queue, list) }?;
    let signal: unsafe extern "system" fn(*mut c_void, *mut c_void, u64) -> i32 = unsafe { core::mem::transmute(vslot(queue, QUEUE_SIGNAL)) };
    unsafe { signal(queue, fence as *mut c_void, value) };
    Some((fence, value))
}

pub(crate) unsafe fn fence_passed(fence: usize, value: u64) -> bool {
    unsafe { fence_done(fence, value) }
}

pub(crate) unsafe fn reapply_so(list: *mut c_void) {
    let so = with(list, |s| s.so.clone());
    for (&slot, v) in &so {
        unsafe { w_so(list, slot, 1, v.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null())) };
    }
}

pub unsafe fn note_executed(queue: *mut c_void, lists: &[*mut c_void]) {
    unsafe { crate::cbv_copy::note_executed(queue, lists) };
    unsafe { crate::dbcount::note_executed(queue, lists) };
    let Some(&first) = lists.first() else { return };
    let keys: Vec<usize> = lists.iter().map(|&l| crate::list_split::origin_of(l)).collect();
    if !store(|st| st.holds_any(&keys)) {
        return;
    }
    let Some((fence, value)) = (unsafe { queue_fence(queue, first) }) else {
        if once(16) {
            record("d3d12_xs", "cl2_state", "no fence for a queue executing query partial slots; those slots are never reused");
        }
        store(|st| st.note_exec(&keys, usize::MAX, u64::MAX));
        return;
    };
    let signal: unsafe extern "system" fn(*mut c_void, *mut c_void, u64) -> i32 = unsafe { core::mem::transmute(vslot(queue, QUEUE_SIGNAL)) };
    unsafe { signal(queue, fence as *mut c_void, value) };
    store(|st| st.note_exec(&keys, fence, value));
}

pub fn release_list_parts(list: *mut c_void) {
    store(|st| st.release(list as usize));
}

unsafe extern "system" fn w_exec_bundle(this: *mut c_void, bundle: *mut c_void) {
    let bundle = crate::cmdlist_wrap::unwrap(bundle);
    fwd!(X_EXEC_BUNDLE, fn(*mut c_void), this, bundle);
    unsafe { after_bundle(this, bundle) };
}

pub unsafe fn after_bundle(this: *mut c_void, bundle: *mut c_void) {
    if bundle.is_null() {
        return;
    }
    let bundle = crate::cmdlist_wrap::unwrap(bundle);
    let b = with(bundle, |s| s.clone());
    let was = is_rect(this);
    rec(this, |s| s.merge_bundle(&b));
    if b.pso.is_none() {
        if let Some(x) = b.depth_bias_x {
            unsafe { apply_depth_bias_x(this, x) };
        }
        if was && !is_rect(this) {
            unsafe { rebind_pso(this) };
        }
    }
    unsafe { apply_rect(this) };
}

unsafe extern "system" fn w_z_order(this: *mut c_void, enable: u32) {
    rec(this, |s| s.xstate.z_order = Some(enable));
    fwd!(X_Z_ORDER, fn(u32), this, enable)
}

unsafe extern "system" fn w_hi_stencil(this: *mut c_void, res: *mut c_void, ctrl: *const [u32; 2]) {
    let c = if ctrl.is_null() { None } else { Some(unsafe { *ctrl }) };
    rec(this, |s| s.xstate.hi_stencil = Some((res as usize, c)));
    fwd!(X_HI_STENCIL, fn(*mut c_void, *const [u32; 2]), this, res, ctrl)
}

unsafe extern "system" fn w_msaa(this: *mut c_void, pos: *const [u8; 128], centroid: *const [u8; 16], config: *const [u32; 2]) {
    let p = if pos.is_null() { None } else { Some(unsafe { *pos }) };
    let c = if centroid.is_null() { None } else { Some(unsafe { *centroid }) };
    let k = if config.is_null() { None } else { Some(unsafe { *config }) };
    rec(this, |s| s.set_msaa(p, c, k));
    fwd!(X_MSAA, fn(*const [u8; 128], *const [u8; 16], *const [u32; 2]), this, pos, centroid, config)
}

unsafe extern "system" fn w_compute_limits(this: *mut c_void, desc: *const [u32; 6]) {
    let d = if desc.is_null() { None } else { Some(unsafe { *desc }) };
    rec(this, |s| s.xstate.compute_limits = Some(d));
    fwd!(X_COMPUTE_LIMITS, fn(*const [u32; 6]), this, desc)
}

unsafe fn reapply_xstate(list: *mut c_void, x: &XState) {
    fn p<T>(o: &Option<T>) -> *const T {
        o.as_ref().map(|v| v as *const T).unwrap_or(core::ptr::null())
    }
    unsafe {
        if let Some(e) = x.z_order { w_z_order(list, e); }
        if let Some((r, c)) = &x.hi_stencil { w_hi_stencil(list, *r as *mut c_void, p(c)); }
        if let Some(m) = &x.msaa { w_msaa(list, p(&m.pos), p(&m.centroid), p(&m.config)); }
        if let Some(d) = &x.compute_limits { w_compute_limits(list, p(d)); }
    }
}

pub(crate) fn set_queue(list: *mut c_void, queue: usize) {
    with(list, |s| s.queue = queue);
}

pub(crate) fn sets_inherited_state(bundle: *mut c_void) -> bool {
    with(bundle, |s| s.heaps.is_some() || s.root.iter().any(Option::is_some) || !s.args.is_empty())
}

pub(crate) fn query_parts(list: *mut c_void) -> Vec<Vec<u32>> {
    with(list, |s| s.queries.iter().map(|q| q.parts.clone()).collect())
}

pub(crate) unsafe fn close_part_into(list: *mut c_void, slots: &[Option<u32>]) -> i32 {
    let _ij = unsafe { crate::inject::scope(list, crate::inject::QUERY) };
    let qs = with(list, |s| if s.open { s.queries.clone() } else { Vec::new() });
    if !qs.is_empty() {
        for q in &qs {
            fwd!(X_END_QUERY, fn(*mut c_void, u32, u32), list, q.heap as *mut c_void, q.ty, q.index);
        }
        unsafe { pc_no_predication(list) };
        for (q, s) in qs.iter().zip(slots) {
            if let Some((res, _, off)) = s.and_then(|s| store(|st| st.loc(s))) {
                unsafe {
                    crate::fill::barrier_pub(list, res, ST_COMMON, ST_COPY_DEST);
                    pc_resolve(list, q.heap, q.ty, q.index, res, off);
                    crate::fill::barrier_pub(list, res, ST_COPY_DEST, ST_COMMON);
                }
            }
        }
        with(list, |s| s.suspended = true);
    }
    let p = prev(X_CLOSE);
    let p = if p != 0 { p } else { unsafe { vslot(list, PC_CLOSE) } };
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let hr = unsafe { f(list) };
    if hr >= 0 {
        with(list, |s| s.open = false);
    }
    hr
}

pub(crate) unsafe fn reapply_bindings(list: *mut c_void) {
    let s = with(list, |s| s.clone());
    unsafe { apply_bindings(list, &s) };
}

unsafe fn apply_bindings(list: *mut c_void, s: &ListState) {
    unsafe {
        if let Some(h) = &s.heaps { w_heaps(list, h.len() as u32, h.as_ptr()); }
        if let Some(r) = s.root[0] { w_croot(list, r as *mut c_void); }
        if let Some(r) = s.root[1] { w_groot(list, r as *mut c_void); }
        for (&(side, idx), a) in &s.args {
            match a {
                RootArg::Table(h) => if side == 0 { w_ctable(list, idx, *h) } else { w_gtable(list, idx, *h) },
                RootArg::View(slot, va) => {
                    let f: unsafe extern "system" fn(*mut c_void, u32, u64) = core::mem::transmute(view_wrapper(*slot));
                    f(list, idx, *va)
                }
                RootArg::Consts(m) => for (&off, &v) in m {
                    if side == 0 { w_c1(list, idx, v, off) } else { w_g1(list, idx, v, off) }
                },
            }
        }
    }
}

pub(crate) fn active_queries(list: *mut c_void) -> usize {
    with(list, |s| if s.suspended { 0 } else { s.queries.len() })
}

pub(crate) unsafe fn reapply(list: *mut c_void) {
    let s = with(list, |s| s.clone());
    unsafe {
        if let Some(h) = &s.heaps { w_heaps(list, h.len() as u32, h.as_ptr()); }
        if let Some(r) = s.root[0] { w_croot(list, r as *mut c_void); }
        if let Some(r) = s.root[1] { w_groot(list, r as *mut c_void); }
        for (&(side, idx), a) in &s.args {
            match a {
                RootArg::Table(h) => if side == 0 { w_ctable(list, idx, *h) } else { w_gtable(list, idx, *h) },
                RootArg::View(slot, va) => {
                    let f: unsafe extern "system" fn(*mut c_void, u32, u64) = core::mem::transmute(view_wrapper(*slot));
                    f(list, idx, *va)
                }
                RootArg::Consts(m) => for (&off, &v) in m {
                    if side == 0 { w_c1(list, idx, v, off) } else { w_g1(list, idx, v, off) }
                },
            }
        }
        if let Some(p) = s.pso { w_pso(list, p as *mut c_void); crate::dred_marks::remark(list, p as *mut c_void); }
        if let Some(t) = s.topology { w_topology(list, t); }
        if let Some(v) = &s.viewports { w_viewports(list, (v.len() / 24) as u32, v.as_ptr()); }
        if let Some(v) = &s.scissors { w_scissors(list, (v.len() / 16) as u32, v.as_ptr()); }
        if let Some(b) = s.blend { w_blend(list, b.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null())); }
        if let Some(r) = s.stencil { w_stencil(list, r); }
        if let Some(ib) = s.ib { w_ib(list, ib.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null())); }
        for (&slot, v) in &s.vbs {
            w_vb(list, slot, 1, v.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null()));
        }
        for (&slot, v) in &s.so {
            w_so(list, slot, 1, v.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null()));
        }
        if let Some(t) = &s.targets {
            let dsv = t.dsv;
            let dp = dsv.as_ref().map(|d| d as *const usize).unwrap_or(core::ptr::null());
            w_rt(list, t.n, if t.handles.is_empty() { core::ptr::null() } else { t.handles.as_ptr() }, t.single, dp);
        }
        if let Some((lo, hi)) = s.depth_bounds { w_depth_bounds(list, lo, hi); }
        reapply_xstate(list, &s.xstate);
        if let Some((sp, px, v)) = &s.sample_positions { w_sample_positions(list, *sp, *px, if v.is_empty() { core::ptr::null() } else { v.as_ptr() }); }
        if let Some((r, c)) = s.shading_rate { w_shading_rate(list, r, c.as_ref().map(|x| x.as_ptr()).unwrap_or(core::ptr::null())); }
        if let Some(i) = s.shading_rate_image { w_shading_rate_image(list, i as *mut c_void); }
        if let Some(p) = s.predication { reapply_predication(list, p.0 as *mut c_void, p.1, p.2); }
        if let Some(x) = s.depth_bias_x {
            with(list, |st| st.depth_bias_x = Some(x));
            apply_depth_bias_x(list, x);
        }
        resume_queries(list);
    }
}

struct PoolAlloc {
    alloc: usize,
    ty: u32,
    owner: usize,
    fence: usize,
    value: u64,
}

static POOL: Mutex<Vec<PoolAlloc>> = Mutex::new(Vec::new());
static QFENCES: Mutex<Option<HashMap<usize, (usize, u64)>>> = Mutex::new(None);

unsafe fn device_of(obj: *mut c_void) -> *mut c_void {
    let get: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(obj, CHILD_GET_DEVICE)) };
    let mut d: *mut c_void = core::ptr::null_mut();
    if unsafe { get(obj, &IID_DEVICE, &mut d) } < 0 || d.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { release(d) };
    d
}

unsafe fn fence_done(fence: usize, value: u64) -> bool {
    if fence == 0 {
        return true;
    }
    if fence == usize::MAX {
        return false;
    }
    let f: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(vslot(fence as *mut c_void, FENCE_COMPLETED)) };
    (unsafe { f(fence as *mut c_void) }) >= value
}

unsafe fn take_allocator(list: *mut c_void) -> *mut c_void {
    let ty = unsafe { crate::fill::list_type(list) };
    {
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        for p in pool.iter_mut() {
            if p.ty == ty && p.owner == 0 && unsafe { fence_done(p.fence, p.value) } {
                let reset: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(p.alloc as *mut c_void, ALLOC_RESET)) };
                if unsafe { reset(p.alloc as *mut c_void) } >= 0 {
                    p.owner = list as usize;
                    p.fence = 0;
                    return p.alloc as *mut c_void;
                }
            }
        }
    }
    let dev = unsafe { device_of(list) };
    if dev.is_null() {
        return core::ptr::null_mut();
    }
    let create: unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_ALLOCATOR)) };
    let mut a: *mut c_void = core::ptr::null_mut();
    if unsafe { create(dev, ty, &IID_ALLOCATOR, &mut a) } < 0 || a.is_null() {
        return core::ptr::null_mut();
    }
    POOL.lock().unwrap_or_else(|e| e.into_inner()).push(PoolAlloc { alloc: a as usize, ty, owner: list as usize, fence: 0, value: 0 });
    a
}

unsafe fn queue_fence(queue: *mut c_void, list: *mut c_void) -> Option<(usize, u64)> {
    let mut g = QFENCES.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if !m.contains_key(&(queue as usize)) {
        let dev = unsafe { device_of(list) };
        if dev.is_null() {
            return None;
        }
        let create: unsafe extern "system" fn(*mut c_void, u64, u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(dev, DEV_CREATE_FENCE)) };
        let mut f: *mut c_void = core::ptr::null_mut();
        if unsafe { create(dev, 0, 0, &IID_FENCE, &mut f) } < 0 || f.is_null() {
            return None;
        }
        m.insert(queue as usize, (f as usize, 0));
    }
    let e = m.get_mut(&(queue as usize))?;
    e.1 += 1;
    Some(*e)
}

static PRESUBMITTED: Mutex<Option<std::collections::HashSet<usize>>> = Mutex::new(None);

fn mark_presubmitted(list: *mut c_void) {
    PRESUBMITTED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(list as usize);
}

pub fn filter_presubmitted(lists: Vec<*mut c_void>, done: &std::collections::HashSet<usize>) -> Vec<*mut c_void> {
    lists.into_iter().filter(|l| !done.contains(&(*l as usize))).collect()
}

pub fn skip_presubmitted(lists: Vec<*mut c_void>) -> Vec<*mut c_void> {
    let g = PRESUBMITTED.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(s) if !s.is_empty() => filter_presubmitted(lists, s),
        _ => lists,
    }
}

fn clear_presubmitted(chain: &[*mut c_void]) {
    if let Some(s) = PRESUBMITTED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        for l in chain {
            s.remove(&(*l as usize));
        }
    }
}

pub(crate) unsafe fn submit_part(list: *mut c_void, queue: *mut c_void) {
    let exec: crate::gpu_wait::ExecFn = unsafe { core::mem::transmute(vslot(queue, QUEUE_EXECUTE)) };
    let l = [list];
    unsafe { crate::title_heap::sync_for_execute(&l) };
    crate::title_shadow::before_execute();
    let sig = if crate::gpu_wait::signal_fn() != 0 { crate::gpu_wait::signal_fn() } else { unsafe { vslot(queue, 14) } };
    unsafe { crate::placed_offset::before_submit(queue, &l, exec as usize, sig) };
    if crate::gpu_wait::has_queue_fns() {
        unsafe { crate::gpu_wait::submit(queue, &l, exec, crate::gpu_wait::signal_fn(), crate::gpu_wait::wait_fn()) };
    } else {
        unsafe { exec(queue, 1, l.as_ptr()) };
    }
    unsafe { crate::placed_offset::after_submit(queue, &l, exec as usize, sig) };
    unsafe { crate::title_shadow::after_execute(queue, crate::gpu_wait::signal_fn(), crate::gpu_wait::wait_fn()) };
    unsafe { crate::list_split::after_submit(queue, &l) };
    unsafe { note_executed(queue, &l) };
    unsafe { retire_allocators(list, queue) };
    mark_presubmitted(list);
}

unsafe fn retire_allocators(list: *mut c_void, queue: *mut c_void) {
    let owned = POOL.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p.owner == list as usize);
    if !owned {
        return;
    }
    let Some((fence, value)) = (unsafe { queue_fence(queue, list) }) else { return };
    let signal: unsafe extern "system" fn(*mut c_void, *mut c_void, u64) -> i32 = unsafe { core::mem::transmute(vslot(queue, QUEUE_SIGNAL)) };
    unsafe { signal(queue, fence as *mut c_void, value) };
    for p in POOL.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().filter(|p| p.owner == list as usize) {
        p.owner = 0;
        p.fence = fence;
        p.value = value;
    }
}

pub(crate) unsafe fn close_part(list: *mut c_void) -> i32 {
    if with(list, |s| s.open) {
        unsafe { suspend_queries(list) };
    }
    let p = prev(X_CLOSE);
    let p = if p != 0 { p } else { unsafe { vslot(list, PC_CLOSE) } };
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let hr = unsafe { f(list) };
    if hr >= 0 {
        with(list, |s| s.open = false);
    }
    hr
}

pub(crate) unsafe fn reopen(list: *mut c_void) -> bool {
    let queue = with(list, |s| s.queue);
    let seg = unsafe { crate::list_split::continue_on_segment(list) };
    if !seg.is_null() {
        clear_presubmitted(&[seg]);
        with(seg, |s| {
            s.queue = queue;
            s.open = true;
        });
        unsafe { reapply(seg) };
        return true;
    }
    let a = unsafe { take_allocator(list) };
    if a.is_null() {
        record("d3d12_xs", "cl2_state", &format!("list split {list:p}: no command allocator for the continuation"));
        return false;
    }
    let reset: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(list, PC_RESET)) };
    let hr = unsafe { reset(list, a, core::ptr::null_mut()) };
    if hr < 0 {
        record("d3d12_xs", "cl2_state", &format!("list split {list:p}: PC Reset FAILED hr={hr:#010x}"));
        return false;
    }
    crate::dred_marks::begin_recording(list as usize);
    with(list, |s| s.open = true);
    unsafe { reapply(list) };
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KickoffAction {
    RejectBundle,
    Defer,
    Split,
}

pub fn kickoff_action(list_type: u32, queue: usize) -> KickoffAction {
    if list_type == LIST_TYPE_BUNDLE {
        KickoffAction::RejectBundle
    } else if queue == 0 {
        KickoffAction::Defer
    } else {
        KickoffAction::Split
    }
}

unsafe extern "system" fn w_kickoff(this: *mut c_void) {
    let ty = unsafe { crate::fill::list_type(this) };
    let queue = with(this, |s| s.queue);
    match kickoff_action(ty, queue) {
        KickoffAction::RejectBundle => {
            if once(16) {
                record("d3d12_xs", "cl2_state", "KickoffX cannot be used with draw bundles (console validation)");
            }
        }
        KickoffAction::Defer => with(this, |s| s.kickoff_pending = true),
        KickoffAction::Split => unsafe {
            let hr = close_part(this);
            if hr < 0 {
                record("d3d12_xs", "cl2_state", &format!("KickoffX {this:p}: Close FAILED hr={hr:#010x}"));
                return;
            }
            submit_part(this, queue as *mut c_void);
            reopen(this);
        },
    }
}

unsafe extern "system" fn w_close(this: *mut c_void) -> i32 {
    let queue = with(this, |s| s.queue);
    let p = prev(X_CLOSE);
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let hr = unsafe { f(this) };
    with(this, |s| {
        s.open = false;
        s.closed = hr >= 0;
        s.queue = 0;
        s.kickoff_pending = false;
    });
    if queue != 0 && hr >= 0 {
        unsafe { submit_part(this, queue as *mut c_void) };
    }
    clear_presubmitted(&crate::list_split::expand(&[this]));
    hr
}

unsafe extern "system" fn w_reset(this: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    let p = prev(X_RESET);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let hr = unsafe { f(this, alloc, pso) };
    clear_presubmitted(&[this]);
    release_list_parts(this);
    if hr >= 0 {
        crate::cl_fw::forget(crate::list_split::origin_of(this));
        with(this, |s| {
            *s = ListState { open: true, list9: s.list9, pso: (!pso.is_null()).then_some(pso as usize), ..ListState::default() };
        });
        unsafe { apply_pso_bias(this, pso as usize) };
    }
    hr
}

unsafe extern "system" fn w_clear_state(this: *mut c_void, pso: *mut c_void) {
    with(this, |s| {
        s.clear_bindings();
        s.pso = (!pso.is_null()).then_some(pso as usize);
    });
    fwd!(X_CLEAR_STATE, fn(*mut c_void), this, pso);
    unsafe { apply_pso_bias(this, pso as usize) };
}

pub unsafe fn before_execute(queue: *mut c_void, lists: &[*mut c_void]) -> Vec<*mut c_void> {
    let mut split = Vec::new();
    for &l in lists {
        if l.is_null() || !with(l, |s| s.open) || unsafe { crate::fill::list_type(l) } == LIST_TYPE_BUNDLE {
            continue;
        }
        let hr = unsafe { close_part(l) };
        if hr < 0 {
            record("d3d12_xs", "cl2_state", &format!("ExecuteCommandLists of open list {l:p}: Close FAILED hr={hr:#010x}"));
            continue;
        }
        with(l, |s| {
            s.queue = queue as usize;
            s.kickoff_pending = false;
        });
        split.push(l);
        if once(16) {
            record("d3d12_xs", "cl2_state", &format!("ExecuteCommandLists of open list {l:p}: recorded part submitted, recording continues on a fresh allocator"));
        }
    }
    split
}

pub unsafe fn after_execute(queue: *mut c_void, split: &[*mut c_void]) {
    for &l in split {
        mark_presubmitted(l);
        unsafe { retire_allocators(l, queue) };
        unsafe { reopen(l) };
    }
}

fn plan() -> Vec<(usize, usize)> {
    let mut v = vec![
        (X_CLOSE, w_close as usize), (X_RESET, w_reset as usize), (X_CLEAR_STATE, w_clear_state as usize),
        (X_TOPOLOGY, w_topology as usize), (X_VIEWPORTS, w_viewports as usize), (X_SCISSORS, w_scissors as usize),
        (X_BLEND_FACTOR, w_blend as usize), (X_STENCIL_REF, w_stencil as usize), (X_PSO, w_pso as usize),
        (X_PREDICATION, w_predication as usize), (X_HEAPS, w_heaps as usize), (X_CROOT, w_croot as usize),
        (X_GROOT, w_groot as usize), (X_CTABLE, w_ctable as usize), (X_GTABLE, w_gtable as usize),
        (X_C1, w_c1 as usize), (X_G1, w_g1 as usize), (X_CN, w_cn as usize), (X_GN, w_gn as usize),
        (X_IB, w_ib as usize), (X_VB, w_vb as usize), (X_SO, w_so as usize), (X_RT, w_rt as usize),
        (X_DEPTH_BOUNDS, w_depth_bounds as usize), (X_DEPTH_BOUNDS_LIST1, w_depth_bounds1 as usize),
        (X_SAMPLE_POSITIONS, w_sample_positions as usize), (X_SHADING_RATE, w_shading_rate as usize),
        (X_SHADING_RATE_IMAGE, w_shading_rate_image as usize), (X_EXEC_BUNDLE, w_exec_bundle as usize),
        (X_BEGIN_QUERY, w_begin_query as usize), (X_END_QUERY, w_end_query as usize), (X_RESOLVE_QUERY, w_resolve_query as usize),
        (X_Z_ORDER, w_z_order as usize), (X_HI_STENCIL, w_hi_stencil as usize), (X_MSAA, w_msaa as usize),
        (X_COMPUTE_LIMITS, w_compute_limits as usize),
    ];
    for s in X_VIEW_FIRST..=X_VIEW_LAST {
        v.push((s, view_wrapper(s)));
    }
    v
}

pub fn install(vt: &[AtomicUsize], defaults: &[usize]) -> usize {
    let mut n = 0;
    for (slot, f) in plan() {
        if slot >= vt.len() {
            continue;
        }
        let p = vt[slot].load(Ordering::Relaxed);
        if p == 0 || p == f {
            continue;
        }
        PREV[slot].store(p, Ordering::Relaxed);
        vt[slot].store(f, Ordering::Relaxed);
        n += 1;
    }
    for (slot, f) in [(X_KICKOFF, w_kickoff as usize), (X_DEPTH_BIAS, w_depth_bias_x as usize)] {
        if slot < vt.len() && slot < defaults.len() && vt[slot].load(Ordering::Relaxed) == defaults[slot] {
            vt[slot].store(f, Ordering::Relaxed);
            n += 1;
        }
    }
    record("d3d12_xs", "cl2_state", &format!("state tracking, KickoffX list split and RSSetDepthBiasX on {n} slots"));
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combine_query(mode: u32, a: &[u64], b: &[u64]) -> Vec<u64> {
        a.iter().zip(b).map(|(&x, &y)| if mode == 1 { ((x | y) != 0) as u64 } else { x.wrapping_add(y) }).collect()
    }

    #[test]
    fn queries_spanning_a_split_collect_parts_and_resolve_accumulates() {
        assert_eq!(query_words(0), Some((1, 0)));
        assert_eq!(query_words(1), Some((1, 1)));
        assert_eq!(query_words(2), None);
        assert_eq!(query_words(3), Some((11, 0)));
        assert_eq!(query_words(5), Some((2, 0)));
        assert_eq!(query_words(8), Some((14, 0)));
        let mut s = ListState::default();
        assert!(s.begin_query(0xa0, 0, 3).is_empty());
        s.queries[0].parts.push(5);
        assert!(s.begin_query(0xb0, 3, 1).is_empty());
        let gone = s.begin_query(0xa0, 1, 4);
        assert_eq!(gone, vec![ActiveQuery { heap: 0xa0, ty: 0, index: 3, parts: vec![5] }]);
        let mut n = crate::query_x::Nest::default();
        assert_eq!(n.begin((0xa0, 3)), None);
        assert_eq!(n.begin((0xa0, 4)), Some((0xa0, 3)));
        assert!(n.end((0xa0, 4)));
        let mut seg = crate::query_x::Nest::default();
        assert_eq!(seg.begin((0xa0, 4)), None);
        n.carry_into(&mut seg);
        assert!(seg.end((0xa0, 4)));
        assert!(!seg.end((0xa0, 3)));
        assert!(seg.end((0xa0, 3)));
        assert_eq!(s.queries.iter().map(|q| (q.heap, q.index)).collect::<Vec<_>>(), vec![(0xb0, 1), (0xa0, 4)]);
        s.queries[1].parts.push(7);
        s.queries[1].parts.push(9);
        s.clear_bindings();
        assert_eq!(s.queries.len(), 2);
        let q = s.end_query(0xa0, 4).unwrap();
        assert_eq!(q.parts, vec![7, 9]);
        assert!(s.end_query(0xa0, 4).is_none());
        let mut m = PartStore::default();
        m.chunks.push((0x111, 0x1000));
        for _ in 0..10 {
            m.take(&|_, _| true);
        }
        for s in [2, 7, 9] {
            m.hold(0x50, s);
        }
        m.complete(q);
        m.complete(ActiveQuery { heap: 0xb0, ty: 3, index: 1, parts: vec![2] });
        assert!(m.plan(0xa0, 0, 4, 1, 0).is_empty());
        assert_eq!(m.plan(0xa0, 1, 2, 4, 0x100), vec![(4, 0x110, 1, 1, vec![7, 9])]);
        assert_eq!(m.plan(0xb0, 3, 0, 2, 0), vec![(1, 88, 11, 0, vec![2])]);
        m.hold(0x60, 9);
        m.release(0x50);
        assert!(m.is_live(2) && m.is_live(7) && m.is_live(9));
        m.complete(ActiveQuery { heap: 0xb0, ty: 3, index: 1, parts: vec![] });
        assert!(m.plan(0xb0, 3, 0, 2, 0).is_empty());
        assert!(!m.is_live(2) && m.is_live(7));
        m.complete(ActiveQuery { heap: 0xa0, ty: 1, index: 4, parts: vec![] });
        assert!(!m.is_live(7) && m.is_live(9));
        m.release(0x60);
        assert!(!m.is_live(9));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            let (s, _) = m.take(&|_, _| true);
            assert!(seen.insert(s) && [2, 7, 9].contains(&s));
        }
        let (s, grow) = m.take(&|_, _| true);
        assert_eq!((s, grow), (10, false));
        for _ in 11..CHUNK_SLOTS {
            m.take(&|_, _| true);
        }
        assert_eq!(m.take(&|_, _| true), (CHUNK_SLOTS, true));
        let mut g = PartStore::default();
        g.chunks.push((1, 2));
        let (a, _) = g.take(&|_, _| true);
        let (b, _) = g.take(&|_, _| true);
        g.hold(0x10, a);
        g.hold(0x20, a);
        g.hold(0x20, b);
        assert!(g.holds_any(&[0x10]) && !g.holds_any(&[0x30]));
        g.note_exec(&[0x10], 0xf1, 5);
        g.note_exec(&[0x20], 0xf2, 7);
        g.note_exec(&[0x20], 0xf2, 6);
        g.release(0x10);
        g.release(0x20);
        assert!(g.is_pending(a) && g.is_pending(b));
        let gpu = |done1: u64, done2: u64| move |f: usize, v: u64| if f == 0xf1 { done1 >= v } else { done2 >= v };
        let (c, _) = g.take(&gpu(5, 6));
        assert!(c != a && c != b && g.is_pending(a) && g.is_pending(b));
        g.untake(c);
        let mut got = std::collections::HashSet::new();
        for _ in 0..3 {
            got.insert(g.take(&gpu(5, 7)).0);
        }
        assert!(got.contains(&a) && got.contains(&b) && !g.is_pending(a) && !g.is_pending(b));
        assert_eq!(m.loc(CHUNK_SLOTS + 1), None);
        assert_eq!(m.loc(3), Some((0x111, 0x1000, 3 * PART_BYTES)));
        assert_eq!(combine_query(0, &[5, 1], &[7, u64::MAX]), vec![12, 0]);
        assert_eq!(combine_query(1, &[0, 0, 1], &[0, 1, 0]), vec![0, 1, 1]);
        assert_eq!(&QUERY_DXIL[0..4], b"DXBC");
        assert!(QUERY_DXIL.windows(4).any(|w| w == b"RTS0") && QUERY_DXIL.windows(4).any(|w| w == b"DXIL"));
        assert!(14 * 8 <= PART_BYTES);
        assert_eq!(resolve_runs(2, 6, &[4, 7]), vec![(2, 2), (5, 2)]);
        assert_eq!(resolve_runs(0, 3, &[0, 1, 2]), vec![]);
        assert_eq!(resolve_runs(0, 2, &[]), vec![(0, 2)]);
    }

    #[test]
    fn xbox_command_list_has_no_render_pass_methods_to_span_a_split() {
        for x in 0..128 {
            let pc = crate::cmdlist_wrap::map_slot(x).or(crate::console_slots::pc_slot_for_list1(x));
            assert!(!matches!(pc, Some(68) | Some(69)), "Xbox slot {x} reaches PC BeginRenderPass/EndRenderPass");
        }
    }

    #[test]
    fn re_execution_after_close_replays_every_part_once_open_parts_are_cleared() {
        let mut r = crate::list_split::Registry::default();
        r.add(0x10, 0x20);
        r.add(0x20, 0x30);
        let chain: Vec<*mut c_void> = r.chain(0x30).into_iter().map(|x| x as *mut c_void).collect();
        let mut done = std::collections::HashSet::new();
        done.insert(0x10usize);
        done.insert(0x20usize);
        assert_eq!(filter_presubmitted(chain.clone(), &done), vec![0x30 as *mut c_void]);
        done.clear();
        assert_eq!(filter_presubmitted(chain.clone(), &done), chain);
        mark_presubmitted(0x7777_0010 as *mut c_void);
        mark_presubmitted(0x7777_0020 as *mut c_void);
        let c2 = vec![0x7777_0010 as *mut c_void, 0x7777_0020 as *mut c_void, 0x7777_0030 as *mut c_void];
        assert_eq!(skip_presubmitted(c2.clone()), vec![0x7777_0030 as *mut c_void]);
        clear_presubmitted(&c2);
        assert_eq!(skip_presubmitted(c2.clone()), c2);
    }

    #[test]
    fn execute_bundle_leaks_bundle_state_into_the_calling_list() {
        let mut caller = ListState { open: true, pso: Some(1), topology: Some(4), depth_bias_x: Some(DepthBiasX { bias: 1, clamp: 0.0, slope: 0.0, clip: true }), ..ListState::default() };
        caller.set_root(1, 0xa);
        caller.set_const(1, 0, 0, &[1, 2]);
        caller.args.insert((1, 1), RootArg::Table(0x50));
        caller.set_vbs(0, &[Some([1; 16])]);
        let mut b = ListState { pso: Some(2), variant_bound: true, ib: Some(None), ..ListState::default() };
        b.set_root(1, 0xa);
        b.set_const(1, 0, 1, &[9]);
        b.set_vbs(1, &[Some([2; 16])]);
        caller.merge_bundle(&b);
        assert_eq!((caller.pso, caller.variant_bound, caller.depth_bias_x, caller.topology), (Some(2), true, None, Some(4)));
        assert_eq!(caller.args.get(&(1, 0)), Some(&RootArg::Consts([(0, 1), (1, 9)].into_iter().collect())));
        assert_eq!(caller.args.get(&(1, 1)), Some(&RootArg::Table(0x50)));
        assert_eq!((caller.ib, caller.vbs.len()), (Some(None), 2));
        let mut b2 = ListState::default();
        b2.set_root(1, 0xc);
        b2.depth_bias_x = Some(DepthBiasX { bias: 3, clamp: 0.0, slope: 1.0, clip: false });
        caller.merge_bundle(&b2);
        assert_eq!((caller.root[1], caller.args.len(), caller.pso), (Some(0xc), 0, Some(2)));
        assert_eq!(caller.depth_bias_x.map(|x| x.bias), Some(3));
    }

    #[test]
    fn cl_a_xbox_state_is_recorded_for_reapply_after_a_split() {
        let mut s = ListState { sample_positions: Some((4, 1, vec![0; 8])), ..ListState::default() };
        s.set_msaa(None, None, Some([2, 0]));
        assert!(s.sample_positions.is_some());
        s.set_msaa(Some([1; 128]), Some([3; 16]), None);
        assert!(s.sample_positions.is_none());
        assert_eq!(s.xstate.msaa, Some(MsaaX { pos: Some([1; 128]), centroid: Some([3; 16]), config: Some([2, 0]) }));
        s.xstate.z_order = Some(1);
        s.xstate.hi_stencil = Some((0x40, Some([5, 6])));
        s.xstate.compute_limits = Some(None);
        let l = 0x7100_0040usize as *mut c_void;
        with(l, |x| *x = s.clone());
        let seg = 0x7100_0080usize as *mut c_void;
        clone_state(l, seg);
        assert_eq!(with(seg, |x| x.xstate.clone()), s.xstate);
        let mut b = ListState::default();
        b.xstate.z_order = Some(0);
        b.xstate.hi_stencil = Some((0x41, None));
        s.merge_bundle(&b);
        assert_eq!((s.xstate.z_order, s.xstate.hi_stencil), (Some(1), Some((0x41, None))));
    }

    #[test]
    fn depth_bias_x_maps_console_registers_to_pc_units() {
        let x = DepthBiasX { bias: -3, clamp: 0.0, slope: 1.5, clip: false };
        assert_eq!(x.pc_values(), (-3.0, 0.0, 1.5));
        assert_eq!(effective_clamp(1.0), 0.0);
        assert_eq!(effective_clamp(-1.0), 0.0);
        assert_eq!(effective_clamp(f32::NAN), 0.0);
        assert_eq!(effective_clamp(1_000_000_000.0), f32::from_bits(1_000_000_000));
        let mut d = vec![0u8; 0x290];
        d[DESC_DEPTH_BIAS..DESC_DEPTH_BIAS + 4].copy_from_slice(&7i32.to_le_bytes());
        d[DESC_DEPTH_BIAS + 8..DESC_DEPTH_BIAS + 12].copy_from_slice(&2.0f32.to_le_bytes());
        d[DESC_DEPTH_CLIP..DESC_DEPTH_CLIP + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(!pso_bias_from_desc(&d).unwrap().dynamic);
        assert!(!patch_desc_flags(&mut d, false));
        assert!(patch_desc_flags(&mut d, true));
        assert_eq!(pso_bias_from_desc(&d), Some(PsoBias { dynamic: true, bias: 7, clamp: 0.0, slope: 2.0, clip: true }));
        assert_eq!(u32::from_le_bytes(d[0x288..0x28c].try_into().unwrap()), PSO_FLAG_DYNAMIC_DEPTH_BIAS);
    }

    #[test]
    fn kickoff_splits_only_queue_attached_direct_lists_and_state_survives() {
        assert_eq!(kickoff_action(1, 0x10), KickoffAction::RejectBundle);
        assert_eq!(kickoff_action(0, 0), KickoffAction::Defer);
        assert_eq!(kickoff_action(0, 0x10), KickoffAction::Split);
        assert_eq!(kickoff_action(2, 0x10), KickoffAction::Split);
        let mut s = ListState { open: true, queue: 5, ..ListState::default() };
        s.set_root(1, 0xa);
        s.args.insert((1, 0), RootArg::Table(0x100));
        s.set_const(1, 2, 1, &[7, 8]);
        s.set_const(1, 2, 0, &[6]);
        s.set_root(0, 0xb);
        s.args.insert((0, 1), RootArg::View(41, 0x2000));
        assert_eq!(s.args.get(&(1, 2)), Some(&RootArg::Consts([(0, 6), (1, 7), (2, 8)].into_iter().collect())));
        s.set_root(1, 0xa);
        assert_eq!(s.args.len(), 3);
        s.set_root(1, 0xc);
        assert_eq!(s.args.len(), 1);
        assert!(s.args.contains_key(&(0, 1)));
        s.set_vbs(0, &[Some([1; 16]), Some([2; 16])]);
        s.set_vbs(1, &[None]);
        assert_eq!(s.vbs.get(&0), Some(&Some([1; 16])));
        assert_eq!(s.vbs.get(&1), Some(&None));
        s.pso = Some(9);
        s.clear_bindings();
        assert_eq!((s.open, s.queue, s.pso, s.args.len()), (true, 5, None, 0));
    }

    #[test]
    fn new_list_state_and_depth_bias_variants() {
        let l = 0x7000_1230usize as *mut c_void;
        with(l, |s| {
            s.pso = Some(4);
            s.queue = 9;
            s.depth_bias_x = Some(DepthBiasX { bias: 1, clamp: 0.0, slope: 0.0, clip: true });
        });
        on_new_list(l);
        assert_eq!(with(l, |s| s.clone()), ListState { open: true, ..ListState::default() });
        assert!(shard_of(0x1000) < SHARDS);
        let mut d = vec![0u8; 0x290];
        d[DESC_FLAGS..DESC_FLAGS + 4].copy_from_slice(&(PSO_FLAG_DYNAMIC_DEPTH_BIAS | 0x1).to_le_bytes());
        d[DESC_DEPTH_CLIP..DESC_DEPTH_CLIP + 4].copy_from_slice(&1u32.to_le_bytes());
        let x = DepthBiasX { bias: -5, clamp: 1.0, slope: 0.25, clip: false };
        let k = static_key(x);
        assert_eq!(k, VariantKey { clip: false, dynamic: false, bias: Some((-5, 0, 0.25f32.to_bits())), rect: false });
        variant_desc(&mut d, k);
        let pb = pso_bias_from_desc(&d).unwrap();
        assert_eq!(pb, PsoBias { dynamic: false, bias: -5, clamp: 0.0, slope: 0.25, clip: false });
        assert_eq!(u32::from_le_bytes(d[DESC_FLAGS..DESC_FLAGS + 4].try_into().unwrap()), 0x1);
        assert!(static_matches(&pb, x));
        assert!(!static_matches(&pb, DepthBiasX { clip: true, ..x }));
        variant_desc(&mut d, VariantKey { clip: true, dynamic: true, bias: None, rect: false });
        assert_eq!(pso_bias_from_desc(&d).unwrap(), PsoBias { dynamic: true, bias: -5, clamp: 0.0, slope: 0.25, clip: true });
        assert!(chunk_fits(120, 8, 128) && !chunk_fits(121, 8, 128) && !chunk_fits(u32::MAX, 8, 128));
    }

    use super::TEST_PREV_LOCK as PREV_LOCK;

    macro_rules! vc {
        ($obj:expr, $slot:expr, fn($($at:ty),*) -> $rt:ty $(, $arg:expr)*) => {{
            let f: unsafe extern "system" fn(*mut c_void $(, $at)*) -> $rt = core::mem::transmute(vslot($obj as *mut c_void, $slot));
            f($obj as *mut c_void $(, $arg)*)
        }};
    }

    const IID_QUEUE: Guid = Guid { d1: 0x0ec8_70a6, d2: 0x5d7e, d3: 0x4c22, d4: [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed] };
    const IID_LIST: Guid = Guid { d1: 0x5b16_0d0f, d2: 0xac1b, d3: 0x4185, d4: [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55] };
    const IID_QUERY_HEAP: Guid = Guid { d1: 0x0d96_58ae, d2: 0xed45, d3: 0x469e, d4: [0xa6, 0x1d, 0x97, 0x0e, 0xc5, 0x83, 0xca, 0xb4] };

    unsafe fn buffer(dev: *mut c_void, heap_type: u32, size: u64, flags: u32, state: u32) -> *mut c_void {
        let heap: [u32; 5] = [heap_type, 0, 0, 1, 1];
        let mut rd = [0u8; 56];
        rd[0..4].copy_from_slice(&1u32.to_le_bytes());
        rd[0x10..0x18].copy_from_slice(&size.to_le_bytes());
        rd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        rd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
        rd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
        rd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        rd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
        rd[0x30..0x34].copy_from_slice(&flags.to_le_bytes());
        let mut r: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32, heap.as_ptr(), 0, rd.as_ptr(), state, core::ptr::null(), &IID_RESOURCE, &mut r) >= 0);
        r
    }

    unsafe fn execute_and_wait(queue: *mut c_void, fence: *mut c_void, list: *mut c_void, value: u64) {
        let l = [list];
        vc!(queue, 10, fn(u32, *const *mut c_void) -> (), 1, l.as_ptr());
        assert!(vc!(queue, 14, fn(*mut c_void, u64) -> i32, fence, value) >= 0);
        let mut n = 0;
        while vc!(fence, 8, fn() -> u64) < value {
            std::thread::sleep(std::time::Duration::from_millis(1));
            n += 1;
            assert!(n < 10000, "GPU did not finish");
        }
    }

    unsafe fn work(list: *mut c_void, p: QueryPipe, uav: u64, groups: u32) {
        vc!(list, PC_SET_CROOT, fn(*mut c_void) -> (), p.rs as *mut c_void);
        vc!(list, PC_SET_PSO, fn(*mut c_void) -> (), p.pso as *mut c_void);
        let k = [0u32; 4];
        vc!(list, PC_SET_CCONSTS, fn(u32, u32, *const u32, u32) -> (), 0, 4, k.as_ptr(), 0);
        vc!(list, PC_SET_CUAV, fn(u32, u64) -> (), 1, uav);
        vc!(list, PC_SET_CUAV, fn(u32, u64) -> (), 2, uav);
        vc!(list, PC_DISPATCH, fn(u32, u32, u32) -> (), groups, 1, 1);
    }

    #[test]
    fn real_device_query_spanning_a_split_resolves_the_whole_total() {
        let _g = PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<usize> = PREV.iter().map(|p| p.swap(0, Ordering::Relaxed)).collect();
        unsafe {
            let create = crate::host_proc(b"D3D12CreateDevice\0");
            assert!(create != 0, "d3d12.dll missing");
            let f: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(create);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(f(core::ptr::null_mut(), 0xc000, &IID_DEVICE, &mut dev) >= 0 && !dev.is_null());
            let mut queue: *mut c_void = core::ptr::null_mut();
            let qd = [0u32; 4];
            assert!(vc!(dev, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &IID_QUEUE, &mut queue) >= 0);
            let mut alloc: *mut c_void = core::ptr::null_mut();
            assert!(vc!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc) >= 0);
            let mut list: *mut c_void = core::ptr::null_mut();
            assert!(vc!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &IID_LIST, &mut list) >= 0);
            let mut fence: *mut c_void = core::ptr::null_mut();
            assert!(vc!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut fence) >= 0);
            let qhd = [2u32, 1, 0];
            let mut qheap: *mut c_void = core::ptr::null_mut();
            assert!(vc!(dev, 39, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qhd.as_ptr(), &IID_QUERY_HEAP, &mut qheap) >= 0);
            let uav = buffer(dev, 1, 256, 4, ST_COMMON);
            let uav_va = vc!(uav, 11, fn() -> u64);
            let rb = buffer(dev, 3, 256, 0, ST_COPY_DEST);
            let p = query_pipe(list).expect("accumulation pipeline from the embedded DXIL");
            for (i, slot) in [(X_BEGIN_QUERY, 52usize), (X_END_QUERY, 53), (X_RESOLVE_QUERY, 54)] {
                PREV[i].store(vslot(list, slot), Ordering::Relaxed);
            }
            on_new_list(list);
            w_begin_query(list, qheap, 3, 0);
            work(list, p, uav_va, 3);
            assert!(close_part(list) >= 0);
            assert!(with(list, |s| s.suspended && s.queries.len() == 1 && s.queries[0].parts.len() == 1));
            execute_and_wait(queue, fence, list, 1);
            assert!(reopen(list));
            assert!(with(list, |s| s.open && !s.suspended));
            work(list, p, uav_va, 5);
            w_end_query(list, qheap, 3, 0);
            w_resolve_query(list, qheap, 3, 0, 1, rb, 0);
            assert!(vc!(list, PC_CLOSE, fn() -> i32) >= 0);
            execute_and_wait(queue, fence, list, 2);
            note_executed(queue, &[list]);
            let held: Vec<u32> = store(|st| st.held.get(&(list as usize)).cloned().unwrap_or_default());
            assert!(!held.is_empty());
            let mut m: *mut u64 = core::ptr::null_mut();
            assert!(vc!(rb, 8, fn(u32, *const c_void, *mut *mut u64) -> i32, 0, core::ptr::null(), &mut m) >= 0);
            let stats = core::slice::from_raw_parts(m, 11).to_vec();
            vc!(rb, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
            release_list_parts(list);
            let scratch: Vec<u32> = held.iter().copied().filter(|&s| store(|st| !st.is_live(s))).collect();
            assert!(!scratch.is_empty() && scratch.iter().all(|&s| store(|st| st.is_pending(s))));
            let mut n = 0;
            while scratch.iter().any(|&s| store(|st| st.is_pending(s))) {
                store(|st| {
                    let (s, _) = st.take(&|f, v| fence_done(f, v));
                    st.untake(s);
                });
                std::thread::sleep(std::time::Duration::from_millis(1));
                n += 1;
                assert!(n < 10000, "queue fence never passed");
            }
            for (i, v) in saved.into_iter().enumerate() {
                PREV[i].store(v, Ordering::Relaxed);
            }
            assert_eq!(stats[10], 8, "CSInvocations of 3 groups before and 5 after the split");
            for o in [rb, uav, qheap, fence, list, alloc] {
                release(o);
            }
        }
    }

    #[test]
    fn install_chains_tracked_slots_and_fills_kickoff_and_depth_bias() {
        let _g = PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let vt: Vec<AtomicUsize> = (0..128).map(|i| AtomicUsize::new(3000 + i)).collect();
        let defaults: Vec<usize> = vt.iter().map(|s| s.load(Ordering::Relaxed)).collect();
        vt[25].store(77, Ordering::Relaxed);
        let n = install(&vt, &defaults);
        assert_eq!(n, plan().len() + 2);
        assert_eq!(PREV[25].load(Ordering::Relaxed), 77);
        assert_eq!(vt[25].load(Ordering::Relaxed), w_pso as usize);
        assert_eq!(vt[93].load(Ordering::Relaxed), w_kickoff as usize);
        assert_eq!(vt[94].load(Ordering::Relaxed), w_depth_bias_x as usize);
        assert_eq!(vt[43].load(Ordering::Relaxed), w_v43 as usize);
        assert_eq!(vt[12].load(Ordering::Relaxed), 3012);
    }

    #[test]
    fn pc_list_slot_resolves_real_pc_methods_on_wrapped_xbox_command_list() {
        let dev = unsafe { real_device() };
        if dev.is_null() {
            return;
        }
        unsafe {
            let mut alloc = core::ptr::null_mut();
            if vc!(dev, DEV_CREATE_ALLOCATOR, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc) < 0 {
                return;
            }
            let mut list = core::ptr::null_mut();
            if vc!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &IID_LIST, &mut list) < 0 {
                release(alloc);
                return;
            }
            let expected_croot = vslot(list, PC_SET_CROOT);
            let expected_cconsts = vslot(list, PC_SET_CCONSTS);
            let expected_cuav = vslot(list, PC_SET_CUAV);
            let expected_resolve = vslot(list, PC_RESOLVE_QUERY);
            let expected_pred = vslot(list, PC_SET_PREDICATION);
            crate::cmdlist_wrap::apply(list);
            assert_eq!(pc_list_slot(list, PC_SET_CROOT), expected_croot);
            assert_eq!(pc_list_slot(list, PC_SET_CCONSTS), expected_cconsts);
            assert_eq!(pc_list_slot(list, PC_SET_CUAV), expected_cuav);
            assert_eq!(pc_list_slot(list, PC_RESOLVE_QUERY), expected_resolve);
            assert_eq!(pc_list_slot(list, PC_SET_PREDICATION), expected_pred);
            release(list);
            release(alloc);
        }
    }
}
