use compat_common::record;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use crate::device_wrap::Guid;

pub const GFX: u8 = 0;
pub const CS: u8 = 1;
const GFX_DESC: usize = 0x290;
const CS_DESC: usize = 0x38;
const MAGIC: &[u8; 8] = b"TBXREC01";
pub const MAX_RECORDS: usize = 8192;
pub const MAX_BYTES: usize = 256 << 20;
const GFX_SHADERS: [usize; 5] = [0x08, 0x18, 0x28, 0x38, 0x48];
const MAX_BLOB: usize = 64 << 20;
const REPLAY_PRIORITY: i32 = -1;
const SAVE_QUIET_MS: u64 = 1000;
const SAVE_MAX_DIRTY_MS: u64 = 3000;
const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a_6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };
const IID_PSO: Guid = Guid { d1: 0x765a_30f3, d2: 0xf624, d3: 0x4c6f, d4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45] };

static SKIP_EXT: AtomicU64 = AtomicU64::new(0);
static SKIP_CACHED: AtomicU64 = AtomicU64::new(0);
static SKIP_ROOT: AtomicU64 = AtomicU64::new(0);
static SKIP_CAP: AtomicU64 = AtomicU64::new(0);
static ADDED: AtomicU64 = AtomicU64::new(0);
static REPLAY_OK: AtomicU64 = AtomicU64::new(0);
static REPLAY_SLOW: AtomicU64 = AtomicU64::new(0);
static REPLAY_FAIL: AtomicU64 = AtomicU64::new(0);
static REPLAY_LEFT: AtomicUsize = AtomicUsize::new(0);
static LAST_ADD_MS: AtomicU64 = AtomicU64::new(0);
static FIRST_DIRTY_MS: AtomicU64 = AtomicU64::new(0);
static SAVING: AtomicBool = AtomicBool::new(false);

extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn SetThreadPriority(h: *mut c_void, p: i32) -> i32;
}

thread_local! {
    static REPLAYING: Cell<bool> = const { Cell::new(false) };
}

pub fn replaying() -> bool {
    REPLAYING.with(|r| r.get())
}

pub fn enabled() -> bool {
    if cfg!(test) {
        return false;
    }
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        let on = std::env::var("TOMBOX_PSO_XREC").map(|v| v.trim() != "0").unwrap_or(true);
        if !on {
            record("d3d12_xs", "pso_xrec", "TOMBOX_PSO_XREC=0: title PSO creates are not recorded or replayed at boot");
        }
        on && crate::pso_lib::enabled()
    })
}

pub fn workers_for(backlog: usize, cores: usize) -> usize {
    if backlog == 0 {
        return 0;
    }
    let cap = cores.saturating_sub(4).clamp(1, 16);
    backlog.div_ceil(16).clamp(1, cap).min(backlog)
}

fn cores() -> usize {
    std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4)
}

pub fn blob_id(b: &[u8]) -> u64 {
    crate::pso_lib::fnv64(b) ^ (b.len() as u64).rotate_left(40)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    pub kind: u8,
    pub desc: Vec<u8>,
    pub shaders: Vec<(u16, u64)>,
    pub root: Option<(u32, u64)>,
    pub il: Vec<(Vec<u8>, [u8; 32])>,
    pub so: Vec<(Vec<u8>, [u8; 24])>,
    pub strides: Vec<u32>,
}

impl Rec {
    pub fn key(&self) -> u128 {
        let mut a = crate::pso_lib::fnv64(&self.encode());
        let mut b = a.rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15;
        for (o, id) in &self.shaders {
            a = a.rotate_left(5) ^ id ^ *o as u64;
            b = b.wrapping_mul(0x100_0000_01b3) ^ id;
        }
        ((a as u128) << 64) | b as u128
    }

    fn blob_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.shaders.iter().map(|s| s.1).chain(self.root.iter().map(|r| r.1))
    }

    fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(self.desc.len() + 128);
        o.push(self.kind);
        put32(&mut o, self.desc.len() as u32);
        o.extend_from_slice(&self.desc);
        o.push(self.shaders.len() as u8);
        for (off, id) in &self.shaders {
            o.extend_from_slice(&off.to_le_bytes());
            o.extend_from_slice(&id.to_le_bytes());
        }
        match self.root {
            Some((mask, id)) => {
                o.push(1);
                put32(&mut o, mask);
                o.extend_from_slice(&id.to_le_bytes());
            }
            None => o.push(0),
        }
        o.extend_from_slice(&(self.il.len() as u16).to_le_bytes());
        for (n, e) in &self.il {
            o.extend_from_slice(&(n.len() as u16).to_le_bytes());
            o.extend_from_slice(n);
            o.extend_from_slice(e);
        }
        o.extend_from_slice(&(self.so.len() as u16).to_le_bytes());
        for (n, e) in &self.so {
            o.extend_from_slice(&(n.len() as u16).to_le_bytes());
            o.extend_from_slice(n);
            o.extend_from_slice(e);
        }
        o.push(self.strides.len() as u8);
        for s in &self.strides {
            put32(&mut o, *s);
        }
        o
    }

    fn decode(r: &mut Reader) -> Option<Rec> {
        let kind = r.u8()?;
        let dl = r.u32()? as usize;
        if dl != GFX_DESC && dl != CS_DESC {
            return None;
        }
        let desc = r.take(dl)?.to_vec();
        let ns = r.u8()? as usize;
        let mut shaders = Vec::with_capacity(ns);
        for _ in 0..ns {
            shaders.push((r.u16()?, r.u64()?));
        }
        let root = match r.u8()? {
            0 => None,
            1 => Some((r.u32()?, r.u64()?)),
            _ => return None,
        };
        let mut il = Vec::new();
        for _ in 0..r.u16()? {
            let n = r.u16()? as usize;
            let name = r.take(n)?.to_vec();
            il.push((name, r.take(32)?.try_into().ok()?));
        }
        let mut so = Vec::new();
        for _ in 0..r.u16()? {
            let n = r.u16()? as usize;
            let name = r.take(n)?.to_vec();
            so.push((name, r.take(24)?.try_into().ok()?));
        }
        let mut strides = Vec::new();
        for _ in 0..r.u8()? {
            strides.push(r.u32()?);
        }
        Some(Rec { kind, desc, shaders, root, il, so, strides })
    }
}

fn put32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn zero(d: &mut [u8], o: usize, n: usize) {
    d[o..o + n].fill(0);
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes(s.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
    }
}

unsafe fn cstr(p: *const u8) -> Option<Vec<u8>> {
    if p.is_null() {
        return Some(Vec::new());
    }
    let mut s = Vec::new();
    while s.len() < 256 {
        let b = unsafe { *p.add(s.len()) };
        if b == 0 {
            return Some(s);
        }
        s.push(b);
    }
    None
}

pub enum Capture {
    Rec(Rec, Vec<(u64, Arc<[u8]>)>),
    Skip(&'static AtomicU64),
}

pub unsafe fn capture(kind: u8, pdesc: *const u8, num_ext: u32, root: &dyn Fn(usize) -> Option<(u32, Arc<[u8]>)>) -> Capture {
    if num_ext != 0 {
        return Capture::Skip(&SKIP_EXT);
    }
    let size = if kind == GFX { GFX_DESC } else { CS_DESC };
    let mut desc = unsafe { core::slice::from_raw_parts(pdesc, size) }.to_vec();
    let (cached_ptr, cached_len) = if kind == GFX { (0x278, 0x280) } else { (0x20, 0x28) };
    if rd64(&desc, cached_ptr) != 0 && rd64(&desc, cached_len) != 0 {
        return Capture::Skip(&SKIP_CACHED);
    }
    zero(&mut desc, cached_ptr, 16);
    let mut blobs: Vec<(u64, Arc<[u8]>)> = Vec::new();
    let rs = rd64(&desc, 0) as usize;
    let root = if rs == 0 {
        None
    } else {
        match root(rs) {
            Some((mask, b)) => {
                let id = blob_id(&b);
                blobs.push((id, b));
                Some((mask, id))
            }
            None => return Capture::Skip(&SKIP_ROOT),
        }
    };
    zero(&mut desc, 0, 8);
    let offs: &[usize] = if kind == GFX { &GFX_SHADERS } else { &[0x08] };
    let mut shaders = Vec::new();
    for &o in offs {
        let (p, n) = (rd64(&desc, o) as *const u8, rd64(&desc, o + 8) as usize);
        zero(&mut desc, o, 8);
        if p.is_null() || n == 0 {
            zero(&mut desc, o + 8, 8);
            continue;
        }
        if n > MAX_BLOB {
            return Capture::Skip(&SKIP_CAP);
        }
        let b: Arc<[u8]> = Arc::from(unsafe { core::slice::from_raw_parts(p, n) });
        let id = blob_id(&b);
        shaders.push((o as u16, id));
        blobs.push((id, b));
    }
    let (mut il, mut so, mut strides) = (Vec::new(), Vec::new(), Vec::new());
    if kind == GFX {
        let (p, n) = (rd64(&desc, 0x228) as *const u8, rd32(&desc, 0x230) as usize);
        zero(&mut desc, 0x228, 8);
        if !p.is_null() && n > 0 {
            if n > 64 {
                return Capture::Skip(&SKIP_CAP);
            }
            for i in 0..n {
                let mut e: [u8; 32] = unsafe { core::ptr::read_unaligned(p.add(i * 32) as *const [u8; 32]) };
                let Some(name) = (unsafe { cstr(rd64(&e, 0) as *const u8) }) else { return Capture::Skip(&SKIP_CAP) };
                e[0..8].fill(0);
                il.push((name, e));
            }
        } else {
            zero(&mut desc, 0x230, 4);
        }
        let (p, n) = (rd64(&desc, 0x58) as *const u8, rd32(&desc, 0x60) as usize);
        zero(&mut desc, 0x58, 8);
        if !p.is_null() && n > 0 {
            if n > 64 {
                return Capture::Skip(&SKIP_CAP);
            }
            for i in 0..n {
                let mut e: [u8; 24] = unsafe { core::ptr::read_unaligned(p.add(i * 24) as *const [u8; 24]) };
                let Some(name) = (unsafe { cstr(rd64(&e, 8) as *const u8) }) else { return Capture::Skip(&SKIP_CAP) };
                e[8..16].fill(0);
                so.push((name, e));
            }
        } else {
            zero(&mut desc, 0x60, 4);
        }
        let (p, n) = (rd64(&desc, 0x68) as *const u32, rd32(&desc, 0x70) as usize);
        zero(&mut desc, 0x68, 8);
        if !p.is_null() && n > 0 {
            if n > 4 {
                return Capture::Skip(&SKIP_CAP);
            }
            for i in 0..n {
                strides.push(unsafe { core::ptr::read_unaligned(p.add(i)) });
            }
        } else {
            zero(&mut desc, 0x70, 4);
        }
    }
    Capture::Rec(Rec { kind, desc, shaders, root, il, so, strides }, blobs)
}

pub struct Built {
    pub desc: Vec<u8>,
    _names: Vec<Vec<u8>>,
    _il: Vec<[u8; 32]>,
    _so: Vec<[u8; 24]>,
    _strides: Vec<u32>,
    _blobs: Vec<Arc<[u8]>>,
}

pub fn build(r: &Rec, blobs: &HashMap<u64, Arc<[u8]>>, root_obj: usize) -> Option<Built> {
    let mut desc = r.desc.clone();
    let mut keep = Vec::new();
    desc[0..8].copy_from_slice(&(root_obj as u64).to_le_bytes());
    for (o, id) in &r.shaders {
        let b = blobs.get(id)?.clone();
        let o = *o as usize;
        desc[o..o + 8].copy_from_slice(&(b.as_ptr() as u64).to_le_bytes());
        desc[o + 8..o + 16].copy_from_slice(&(b.len() as u64).to_le_bytes());
        keep.push(b);
    }
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut il: Vec<[u8; 32]> = Vec::with_capacity(r.il.len());
    for (n, e) in &r.il {
        let mut s = n.clone();
        s.push(0);
        let mut e = *e;
        e[0..8].copy_from_slice(&(s.as_ptr() as u64).to_le_bytes());
        names.push(s);
        il.push(e);
    }
    let mut so: Vec<[u8; 24]> = Vec::with_capacity(r.so.len());
    for (n, e) in &r.so {
        let mut s = n.clone();
        s.push(0);
        let mut e = *e;
        e[8..16].copy_from_slice(&(s.as_ptr() as u64).to_le_bytes());
        names.push(s);
        so.push(e);
    }
    let strides = r.strides.clone();
    if r.kind == GFX {
        if !il.is_empty() {
            desc[0x228..0x230].copy_from_slice(&(il.as_ptr() as u64).to_le_bytes());
        }
        if !so.is_empty() {
            desc[0x58..0x60].copy_from_slice(&(so.as_ptr() as u64).to_le_bytes());
        }
        if !strides.is_empty() {
            desc[0x68..0x70].copy_from_slice(&(strides.as_ptr() as u64).to_le_bytes());
        }
    }
    Some(Built { desc, _names: names, _il: il, _so: so, _strides: strides, _blobs: keep })
}

#[derive(Default)]
pub struct Journal {
    pub recs: Vec<Rec>,
    pub keys: HashSet<u128>,
    pub blobs: HashMap<u64, Arc<[u8]>>,
    pub bytes: usize,
    pub dirty: bool,
}

impl Journal {
    pub fn add(&mut self, r: Rec, blobs: Vec<(u64, Arc<[u8]>)>) -> bool {
        let k = r.key();
        if self.keys.contains(&k) {
            return false;
        }
        let extra: usize = blobs.iter().filter(|(id, _)| !self.blobs.contains_key(id)).map(|(_, b)| b.len()).sum();
        if self.recs.len() >= MAX_RECORDS || self.bytes + extra > MAX_BYTES {
            return false;
        }
        for (id, b) in blobs {
            self.blobs.entry(id).or_insert(b);
        }
        self.bytes += extra;
        self.keys.insert(k);
        self.recs.push(r);
        self.dirty = true;
        true
    }

    pub fn merge(&mut self, other: Journal) {
        for r in other.recs {
            let ids: Vec<(u64, Arc<[u8]>)> = r.blob_ids().filter_map(|i| other.blobs.get(&i).map(|b| (i, b.clone()))).collect();
            let dirty = self.dirty;
            self.add(r, ids);
            self.dirty = dirty;
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(self.bytes + self.recs.len() * 800 + 64);
        o.extend_from_slice(MAGIC);
        put32(&mut o, self.recs.len() as u32);
        for r in &self.recs {
            let e = r.encode();
            put32(&mut o, e.len() as u32);
            o.extend_from_slice(&e);
        }
        let used: HashSet<u64> = self.recs.iter().flat_map(|r| r.blob_ids()).collect();
        let mut ids: Vec<&u64> = self.blobs.keys().filter(|i| used.contains(i)).collect();
        ids.sort();
        put32(&mut o, ids.len() as u32);
        for id in ids {
            let b = &self.blobs[id];
            o.extend_from_slice(&id.to_le_bytes());
            put32(&mut o, b.len() as u32);
            o.extend_from_slice(b);
        }
        o
    }

    pub fn decode(b: &[u8]) -> Option<Journal> {
        let mut r = Reader { b, at: 0 };
        if r.take(8)? != MAGIC {
            return None;
        }
        let n = r.u32()? as usize;
        let mut recs = Vec::with_capacity(n.min(MAX_RECORDS));
        for _ in 0..n {
            let len = r.u32()? as usize;
            let mut rr = Reader { b: r.take(len)?, at: 0 };
            recs.push(Rec::decode(&mut rr)?);
        }
        let nb = r.u32()? as usize;
        let mut blobs = HashMap::new();
        for _ in 0..nb {
            let id = r.u64()?;
            let len = r.u32()? as usize;
            let data = r.take(len)?;
            if blob_id(data) != id {
                return None;
            }
            blobs.insert(id, Arc::from(data));
        }
        let mut j = Journal::default();
        for rec in recs {
            let ids: Vec<(u64, Arc<[u8]>)> = rec.blob_ids().filter_map(|i| blobs.get(&i).map(|b: &Arc<[u8]>| (i, b.clone()))).collect();
            if ids.len() != rec.blob_ids().count() {
                continue;
            }
            j.add(rec, ids);
        }
        j.dirty = false;
        Some(j)
    }
}

fn journal() -> &'static Mutex<Journal> {
    static J: OnceLock<Mutex<Journal>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(Journal::default()))
}

static FILE: OnceLock<std::path::PathBuf> = OnceLock::new();

fn clock_ms() -> u64 {
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

pub unsafe fn note(kind: u8, pdesc: *const u8, num_ext: u32) {
    if pdesc.is_null() || replaying() || !enabled() || FILE.get().is_none() {
        return;
    }
    let root = |rs: usize| crate::pso_lib::root_info(rs).filter(|r| !r.2).map(|r| (r.0, r.1));
    match unsafe { capture(kind, pdesc, num_ext, &root) } {
        Capture::Skip(c) => {
            c.fetch_add(1, Relaxed);
        }
        Capture::Rec(r, blobs) => {
            let added = crate::frame_ev::lock(journal()).add(r, blobs);
            if added {
                ADDED.fetch_add(1, Relaxed);
                let now = clock_ms();
                LAST_ADD_MS.store(now, Relaxed);
                let _ = FIRST_DIRTY_MS.compare_exchange(0, now, Relaxed, Relaxed);
            }
        }
    }
}

pub fn save_due(now: u64, first_dirty: u64, last_add: u64) -> bool {
    first_dirty != 0 && (now.saturating_sub(last_add) >= SAVE_QUIET_MS || now.saturating_sub(first_dirty) >= SAVE_MAX_DIRTY_MS)
}

pub fn save() -> Option<usize> {
    let path = FILE.get()?;
    if SAVING.swap(true, Relaxed) {
        return None;
    }
    FIRST_DIRTY_MS.store(0, Relaxed);
    let bytes = {
        let mut j = crate::frame_ev::lock(journal());
        if let Some(disk) = std::fs::read(path).ok().and_then(|b| Journal::decode(&b)) {
            j.merge(disk);
        }
        j.dirty = false;
        (j.encode(), j.recs.len())
    };
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let ok = std::fs::write(&tmp, &bytes.0).is_ok() && std::fs::rename(&tmp, path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    SAVING.store(false, Relaxed);
    ok.then_some(bytes.1)
}

struct Replay {
    recs: Vec<Rec>,
    blobs: HashMap<u64, Arc<[u8]>>,
    next: AtomicUsize,
    wrapper: usize,
    roots: Mutex<HashMap<(u32, u64), usize>>,
    t0: std::time::Instant,
    workers_left: AtomicUsize,
    workers: usize,
}

static REPLAY: OnceLock<Replay> = OnceLock::new();

pub fn start(wrapper: *mut c_void, dir: &std::path::Path) {
    if !enabled() || wrapper.is_null() || FILE.get().is_some() {
        return;
    }
    let path = dir.join("pso_xrec.bin");
    let _ = FILE.set(path.clone());
    let t0 = std::time::Instant::now();
    let file = std::fs::read(&path).ok();
    let disk = file.as_deref().map(Journal::decode);
    let note = match &disk {
        None => "no file yet".to_string(),
        Some(None) => "unreadable, starting over".to_string(),
        Some(Some(j)) => format!("{} records, {} MB of Xbox shader and root-signature blobs", j.recs.len(), j.bytes >> 20),
    };
    let disk = disk.flatten().unwrap_or_default();
    let recs = disk.recs.clone();
    let blobs = disk.blobs.clone();
    crate::frame_ev::lock(journal()).merge(disk);
    let _ = std::thread::Builder::new().name("tombox-pso-xrec-save".into()).spawn(saver);
    let n = recs.len();
    let workers = workers_for(n, cores());
    record("d3d12_xs", "pso_xrec", &format!(
        "{} ({note}) read in {:.0} ms; records are the title's own Xbox PSO inputs, so translator and root-signature changes do not invalidate them; {workers} BELOW_NORMAL workers replay them through the title create path to fill the pipeline library (and, with cbv_raw=copy, the per-shader constant-buffer size table the title-thread create step needs); TOMBOX_PSO_XREC=0 disables",
        path.display(), t0.elapsed().as_secs_f64() * 1000.0));
    if n == 0 {
        return;
    }
    let rp = Replay { recs, blobs, next: AtomicUsize::new(0), wrapper: wrapper as usize, roots: Mutex::new(HashMap::new()), t0: std::time::Instant::now(), workers_left: AtomicUsize::new(workers), workers };
    if REPLAY.set(rp).is_err() {
        return;
    }
    REPLAY_LEFT.store(n, Relaxed);
    for _ in 0..workers {
        let _ = std::thread::Builder::new().name("tombox-pso-xrec".into()).stack_size(64 << 20).spawn(replay_worker);
    }
}

fn saver() {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if save_due(clock_ms(), FIRST_DIRTY_MS.load(Relaxed), LAST_ADD_MS.load(Relaxed)) {
            let _ = save();
        }
    }
}

pub fn save_at_exit() {
    if FIRST_DIRTY_MS.load(Relaxed) != 0 {
        let _ = save();
    }
}

unsafe fn vslot(obj: usize, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

unsafe fn release(obj: usize) {
    let f: unsafe extern "system" fn(usize) -> u32 = unsafe { core::mem::transmute(vslot(obj, 2)) };
    unsafe { f(obj) };
}

unsafe fn root_obj(rp: &Replay, mask: u32, id: u64) -> Option<usize> {
    if let Some(&o) = crate::frame_ev::lock(&rp.roots).get(&(mask, id)) {
        return Some(o);
    }
    let b = rp.blobs.get(&id)?;
    let f: unsafe extern "system" fn(usize, u32, *const u8, usize, *const Guid, *mut usize) -> i32 = unsafe { core::mem::transmute(vslot(rp.wrapper, 16)) };
    let mut o = 0usize;
    let hr = unsafe { f(rp.wrapper, mask, b.as_ptr(), b.len(), &IID_ROOT_SIGNATURE, &mut o) };
    if hr < 0 || o == 0 {
        return None;
    }
    let mut g = crate::frame_ev::lock(&rp.roots);
    match g.get(&(mask, id)) {
        Some(&prev) => {
            drop(g);
            unsafe { release(o) };
            Some(prev)
        }
        None => {
            g.insert((mask, id), o);
            Some(o)
        }
    }
}

fn replay_worker() {
    let Some(rp) = REPLAY.get() else { return };
    unsafe { SetThreadPriority(GetCurrentThread(), REPLAY_PRIORITY) };
    REPLAYING.with(|r| r.set(true));
    loop {
        let i = rp.next.fetch_add(1, Relaxed);
        let Some(r) = rp.recs.get(i) else { break };
        let t = std::time::Instant::now();
        let root = match r.root {
            Some((mask, id)) => unsafe { root_obj(rp, mask, id) },
            None => Some(0),
        };
        let hr = match root.and_then(|ro| build(r, &rp.blobs, ro)) {
            Some(b) => {
                let _slot = crate::shader_xlat::bg_slot(rp.workers);
                let mut out = 0usize;
                let hr = unsafe { crate::device_wrap::replay_create(rp.wrapper as *mut c_void, r.kind == GFX, b.desc.as_ptr(), &IID_PSO, &mut out) };
                if hr >= 0 && out != 0 {
                    crate::device_wrap::forget_pso(out);
                    unsafe { release(out) };
                }
                hr
            }
            None => -1,
        };
        if hr >= 0 {
            REPLAY_OK.fetch_add(1, Relaxed);
            if t.elapsed().as_millis() >= 20 {
                REPLAY_SLOW.fetch_add(1, Relaxed);
            }
        } else {
            REPLAY_FAIL.fetch_add(1, Relaxed);
        }
        REPLAY_LEFT.fetch_sub(1, Relaxed);
    }
    if rp.workers_left.fetch_sub(1, Relaxed) == 1 {
        let roots: Vec<usize> = crate::frame_ev::lock(&rp.roots).drain().map(|(_, o)| o).collect();
        for o in roots {
            unsafe { release(o) };
        }
        record("d3d12_xs", "pso_xrec", &format!("replay finished in {:.1} s: {}", rp.t0.elapsed().as_secs_f64(), summary()));
    }
}

pub fn summary() -> String {
    format!(
        "xbox records {} (+{} this run; not recorded: {} with Xbox ext entries, {} with a title cached blob, {} with an unknown root signature, {} over caps); replayed {} ok ({} took >= 20 ms, i.e. compiled by the driver), {} failed, {} left",
        crate::frame_ev::lock(journal()).recs.len(), ADDED.load(Relaxed), SKIP_EXT.load(Relaxed), SKIP_CACHED.load(Relaxed), SKIP_ROOT.load(Relaxed), SKIP_CAP.load(Relaxed),
        REPLAY_OK.load(Relaxed), REPLAY_SLOW.load(Relaxed), REPLAY_FAIL.load(Relaxed), REPLAY_LEFT.load(Relaxed))
}

pub struct Flights {
    set: Mutex<Vec<u128>>,
    cv: Condvar,
}

pub struct Flight<'a> {
    f: &'a Flights,
    k: u128,
}

impl Flights {
    pub const fn new() -> Self {
        Flights { set: Mutex::new(Vec::new()), cv: Condvar::new() }
    }

    pub fn enter(&self, k: u128) -> (Flight<'_>, bool) {
        let mut g = self.set.lock().unwrap_or_else(|e| e.into_inner());
        let mut waited = false;
        while g.contains(&k) {
            waited = true;
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.push(k);
        (Flight { f: self, k }, waited)
    }
}

impl Default for Flights {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.f.set.lock().unwrap_or_else(|e| e.into_inner()).retain(|&x| x != self.k);
        self.f.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gfx_desc(vs: &[u8], ps: &[u8], il: &[[u8; 32]], rs: usize) -> Vec<u8> {
        let mut d = vec![0u8; GFX_DESC];
        d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
        d[0x08..0x10].copy_from_slice(&(vs.as_ptr() as u64).to_le_bytes());
        d[0x10..0x18].copy_from_slice(&(vs.len() as u64).to_le_bytes());
        d[0x18..0x20].copy_from_slice(&(ps.as_ptr() as u64).to_le_bytes());
        d[0x20..0x28].copy_from_slice(&(ps.len() as u64).to_le_bytes());
        d[0x228..0x230].copy_from_slice(&(il.as_ptr() as u64).to_le_bytes());
        d[0x230..0x234].copy_from_slice(&(il.len() as u32).to_le_bytes());
        d[0x23c..0x240].copy_from_slice(&3u32.to_le_bytes());
        d[0x240..0x244].copy_from_slice(&1u32.to_le_bytes());
        d[0x244..0x248].copy_from_slice(&28u32.to_le_bytes());
        d
    }

    fn elem(name: &[u8], fmt: u32, off: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(&(name.as_ptr() as u64).to_le_bytes());
        e[12..16].copy_from_slice(&fmt.to_le_bytes());
        e[20..24].copy_from_slice(&off.to_le_bytes());
        e
    }

    fn root_blob() -> Arc<[u8]> {
        Arc::from(&b"xbox-root-signature-bytes"[..])
    }

    fn cap(d: &[u8]) -> (Rec, Vec<(u64, Arc<[u8]>)>) {
        let rb = root_blob();
        let root = move |rs: usize| (rs == 0x5000).then(|| (1u32, rb.clone()));
        match unsafe { capture(GFX, d.as_ptr(), 0, &root) } {
            Capture::Rec(r, b) => (r, b),
            Capture::Skip(_) => panic!("skipped"),
        }
    }

    #[test]
    fn records_hold_title_inputs_and_rebuild_an_equivalent_desc() {
        let (vs, ps) = (vec![1u8; 300], vec![2u8; 500]);
        let (n0, n1) = (b"POSITION\0".to_vec(), b"TEXCOORD\0".to_vec());
        let il = [elem(&n0, 6, 0), elem(&n1, 16, 12)];
        let d = gfx_desc(&vs, &ps, &il, 0x5000);
        let (r, blobs) = cap(&d);
        assert_eq!(r.root, Some((1, blob_id(&root_blob()))));
        assert_eq!(r.shaders.len(), 2);
        assert_eq!(r.il.iter().map(|(n, _)| n.as_slice()).collect::<Vec<_>>(), vec![&b"POSITION"[..], &b"TEXCOORD"[..]]);
        assert!([0usize, 0x08, 0x18, 0x228].iter().all(|&o| rd64(&r.desc, o) == 0));
        let map: HashMap<u64, Arc<[u8]>> = blobs.into_iter().collect();
        let b = build(&r, &map, 0x7777).unwrap();
        assert_eq!(rd64(&b.desc, 0), 0x7777);
        let vs2 = unsafe { core::slice::from_raw_parts(rd64(&b.desc, 0x08) as *const u8, rd64(&b.desc, 0x10) as usize) };
        assert_eq!(vs2, &vs[..]);
        let il2 = rd64(&b.desc, 0x228) as *const [u8; 32];
        let e1 = unsafe { *il2.add(1) };
        assert_eq!(unsafe { cstr(rd64(&e1, 0) as *const u8) }.unwrap(), b"TEXCOORD");
        assert_eq!(&e1[8..], &il[1][8..]);
        for o in (0..GFX_DESC).step_by(4).filter(|o| ![0usize, 4, 0x08, 0x0c, 0x18, 0x1c, 0x228, 0x22c].contains(o)) {
            assert_eq!(rd32(&b.desc, o), rd32(&d, o), "offset {o:#x}");
        }
    }

    #[test]
    fn the_key_ignores_title_pointers_but_not_contents() {
        let (vs, ps) = (vec![1u8; 64], vec![2u8; 64]);
        let (vs_b, ps_b) = (vs.clone(), ps.clone());
        let n = b"POSITION\0".to_vec();
        let nb = n.clone();
        let (r1, _) = cap(&gfx_desc(&vs, &ps, &[elem(&n, 6, 0)], 0x5000));
        let (r2, _) = cap(&gfx_desc(&vs_b, &ps_b, &[elem(&nb, 6, 0)], 0x5000));
        assert_eq!(r1.key(), r2.key());
        let mut ps_c = ps.clone();
        ps_c[10] ^= 1;
        let (r3, _) = cap(&gfx_desc(&vs, &ps_c, &[elem(&n, 6, 0)], 0x5000));
        assert_ne!(r1.key(), r3.key());
        let (r4, _) = cap(&gfx_desc(&vs, &ps, &[elem(&n, 7, 0)], 0x5000));
        assert_ne!(r1.key(), r4.key());
    }

    #[test]
    fn unknown_root_signatures_ext_entries_and_cached_blobs_are_not_recorded() {
        let (vs, ps) = (vec![1u8; 64], vec![2u8; 64]);
        let n = b"POSITION\0".to_vec();
        let il = [elem(&n, 6, 0)];
        let mut d = gfx_desc(&vs, &ps, &il, 0x6000);
        let none = |_: usize| None;
        assert!(matches!(unsafe { capture(GFX, d.as_ptr(), 0, &none) }, Capture::Skip(c) if core::ptr::eq(c, &SKIP_ROOT)));
        assert!(matches!(unsafe { capture(GFX, d.as_ptr(), 1, &none) }, Capture::Skip(c) if core::ptr::eq(c, &SKIP_EXT)));
        d[0..8].fill(0);
        d[0x278..0x280].copy_from_slice(&(vs.as_ptr() as u64).to_le_bytes());
        d[0x280..0x288].copy_from_slice(&8u64.to_le_bytes());
        assert!(matches!(unsafe { capture(GFX, d.as_ptr(), 0, &none) }, Capture::Skip(c) if core::ptr::eq(c, &SKIP_CACHED)));
        d[0x278..0x288].fill(0);
        assert!(matches!(unsafe { capture(GFX, d.as_ptr(), 0, &none) }, Capture::Rec(r, _) if r.root.is_none()));
    }

    #[test]
    fn compute_records_round_trip() {
        let cs = vec![9u8; 128];
        let mut d = vec![0u8; CS_DESC];
        d[0x08..0x10].copy_from_slice(&(cs.as_ptr() as u64).to_le_bytes());
        d[0x10..0x18].copy_from_slice(&(cs.len() as u64).to_le_bytes());
        let none = |_: usize| None;
        let Capture::Rec(r, blobs) = (unsafe { capture(CS, d.as_ptr(), 0, &none) }) else { panic!() };
        let mut j = Journal::default();
        assert!(j.add(r.clone(), blobs));
        let back = Journal::decode(&j.encode()).unwrap();
        assert_eq!(back.recs, vec![r.clone()]);
        let b = build(&back.recs[0], &back.blobs, 0).unwrap();
        assert_eq!(&b.desc[0x10..0x18], &d[0x10..0x18]);
        assert_ne!(rd64(&b.desc, 0x08), 0);
    }

    #[test]
    fn journal_dedups_survives_encode_and_merges_files_from_other_runs() {
        let (vs, ps1, ps2) = (vec![1u8; 64], vec![2u8; 64], vec![3u8; 64]);
        let n = b"POSITION\0".to_vec();
        let mut so = [0u8; 24];
        let sn = b"SV_Position\0".to_vec();
        so[8..16].copy_from_slice(&(sn.as_ptr() as u64).to_le_bytes());
        so[21] = 4;
        let strides = [16u32];
        let mut d1 = gfx_desc(&vs, &ps1, &[elem(&n, 6, 0)], 0x5000);
        d1[0x58..0x60].copy_from_slice(&(so.as_ptr() as u64).to_le_bytes());
        d1[0x60..0x64].copy_from_slice(&1u32.to_le_bytes());
        d1[0x68..0x70].copy_from_slice(&(strides.as_ptr() as u64).to_le_bytes());
        d1[0x70..0x74].copy_from_slice(&1u32.to_le_bytes());
        let (r1, b1) = cap(&d1);
        let (r2, b2) = cap(&gfx_desc(&vs, &ps2, &[elem(&n, 6, 0)], 0x5000));
        assert_eq!(r1.so[0].0, b"SV_Position");
        assert_eq!(r1.strides, vec![16]);
        let mut a = Journal::default();
        assert!(a.add(r1.clone(), b1.clone()));
        assert!(!a.add(r1.clone(), b1.clone()));
        let mut other = Journal::default();
        other.add(r2.clone(), b2);
        other.add(r1.clone(), b1);
        let disk = Journal::decode(&other.encode()).unwrap();
        a.dirty = false;
        a.merge(disk);
        assert_eq!(a.recs.len(), 2);
        assert!(!a.dirty);
        let back = Journal::decode(&a.encode()).unwrap();
        assert_eq!(back.recs, a.recs);
        assert_eq!(back.blobs.len(), 4);
        let mut bad = a.encode();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert!(Journal::decode(&bad).is_none());
        assert!(Journal::decode(b"TBXREC00").is_none());
    }

    #[test]
    fn worker_count_scales_with_the_backlog_and_leaves_cores_for_the_title() {
        assert_eq!(workers_for(0, 20), 0);
        assert_eq!(workers_for(1, 20), 1);
        assert_eq!(workers_for(16, 20), 1);
        assert_eq!(workers_for(17, 20), 2);
        assert_eq!(workers_for(259, 20), 16);
        assert_eq!(workers_for(5000, 20), 16);
        assert_eq!(workers_for(5000, 8), 4);
        assert_eq!(workers_for(100, 4), 1);
        assert_eq!(workers_for(3, 64), 1);
    }

    #[test]
    fn saves_wait_for_a_quiet_second_but_never_more_than_three() {
        assert!(!save_due(10_000, 0, 9_000));
        assert!(!save_due(10_000, 9_500, 9_800));
        assert!(save_due(10_000, 9_500, 9_000));
        assert!(save_due(10_000, 7_000, 9_999));
        assert!(!save_due(10_000, 7_500, 9_999));
    }

    #[test]
    fn a_second_creator_of_the_same_key_waits_for_the_first() {
        static F: Flights = Flights::new();
        let (g, waited) = F.enter(42);
        assert!(!waited);
        let order = Arc::new(Mutex::new(Vec::new()));
        let o2 = order.clone();
        let h = std::thread::spawn(move || {
            let (_g, waited) = F.enter(42);
            o2.lock().unwrap().push("second");
            waited
        });
        let (_other, w) = F.enter(43);
        assert!(!w);
        std::thread::sleep(std::time::Duration::from_millis(50));
        order.lock().unwrap().push("first");
        drop(g);
        assert!(h.join().unwrap());
        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }

    #[test]
    fn replay_flag_is_per_thread() {
        assert!(!replaying());
        REPLAYING.with(|r| r.set(true));
        assert!(replaying());
        assert!(!std::thread::spawn(replaying).join().unwrap());
        REPLAYING.with(|r| r.set(false));
    }
}
