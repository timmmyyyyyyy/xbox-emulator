use crate::abi::*;
use crate::request::{execute_with_intermediate, validate, Source, XRequest};
use crate::{log, log_fatal};
use core::ffi::c_void;
use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(sa: *mut c_void, manual: i32, initial: i32, name: *const u16) -> isize;
    fn SetEvent(h: isize) -> i32;
    fn CloseHandle(h: isize) -> i32;
    fn DuplicateHandle(sp: isize, sh: isize, tp: isize, th: *mut isize, access: u32, inherit: i32, options: u32) -> i32;
    fn GetCurrentProcess() -> isize;
    fn GetCurrentThread() -> isize;
    fn GetLastError() -> u32;
    fn SetThreadAffinityMask(h: isize, mask: usize) -> usize;
    fn GetProcessAffinityMask(h: isize, proc_mask: *mut usize, sys_mask: *mut usize) -> i32;
    fn VirtualQuery(addr: *const c_void, info: *mut MemInfo, len: usize) -> usize;
    fn DebugBreak();
}

const DUPLICATE_SAME_ACCESS: u32 = 2;

#[repr(C)]
#[derive(Default)]
struct MemInfo {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    partition: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

const MEM_COMMIT: u32 = 0x1000;
const PAGE_GUARD: u32 = 0x100;
const PAGE_NOACCESS: u32 = 0x01;
const WRITABLE: u32 = 0x04 | 0x08 | 0x40 | 0x80;

pub fn range_accessible(addr: u64, len: usize, write: bool) -> bool {
    if addr == 0 {
        return false;
    }
    if len == 0 {
        return true;
    }
    let end = match (addr as usize).checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    crate::access_cache::check(addr as usize, end, write, || range_accessible_uncached(addr as usize, end, write))
}

fn range_accessible_uncached(start: usize, end: usize, write: bool) -> Option<(usize, usize)> {
    let mut cur = start;
    let mut span_lo = start;
    while cur < end {
        let mut mi = MemInfo::default();
        let n = unsafe { VirtualQuery(cur as *const c_void, &mut mi, core::mem::size_of::<MemInfo>()) };
        if n == 0 || mi.state != MEM_COMMIT || mi.protect & PAGE_GUARD != 0 || mi.protect & PAGE_NOACCESS != 0 || mi.protect == 0 {
            return None;
        }
        if write && mi.protect & WRITABLE == 0 {
            return None;
        }
        if cur == start {
            span_lo = mi.base.min(start);
        }
        let next = mi.base.saturating_add(mi.region_size);
        if next <= cur {
            return None;
        }
        cur = next;
    }
    Some((span_lo, cur))
}

fn dup_handle(h: isize) -> Result<isize, i32> {
    let mut out: isize = 0;
    let ok = unsafe { DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &mut out, 0, 0, DUPLICATE_SAME_ACCESS) };
    if ok == 0 {
        Err(hresult_from_win32(unsafe { GetLastError() }))
    } else {
        Ok(out)
    }
}

pub static DEBUG_FLAGS: AtomicU32 = AtomicU32::new(0);
pub static CPU_AFFINITY: AtomicU64 = AtomicU64::new(0);
pub static STAGING_BUFFER_SIZE: AtomicU32 = AtomicU32::new(0);
pub static LIVE_QUEUES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_FILES: AtomicUsize = AtomicUsize::new(0);
static NEXT_FILE_ID: AtomicU32 = AtomicU32::new(1);

pub fn report(func: &str, hr: i32, detail: &str) {
    log(func, &format!("ERROR hr={hr:#010x} {detail}"));
    if DEBUG_FLAGS.load(Ordering::Relaxed) & 2 != 0 {
        unsafe { DebugBreak() };
    }
}

unsafe fn qi_out(ppv: *mut *mut c_void, obj: *mut c_void) {
    unsafe { *ppv = obj };
}

macro_rules! trap {
    ($name:ident, $iface:expr, $slot:expr) => {
        unsafe extern "system" fn $name(this: *mut c_void) -> i32 {
            log_fatal($iface, $slot, this as usize);
            E_NOTIMPL
        }
    };
}

#[repr(C)]
pub struct FactoryVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub create_queue: unsafe extern "system" fn(*mut c_void, *const u8, *const c_void, *mut *mut c_void) -> i32,
    pub open_file: unsafe extern "system" fn(*mut c_void, *const u16, *const c_void, *mut *mut c_void) -> i32,
    pub create_status_array: unsafe extern "system" fn(*mut c_void, u32, *const u8, *const c_void, *mut *mut c_void) -> i32,
    pub set_cpu_affinity: unsafe extern "system" fn(*mut c_void, u64) -> i32,
    pub set_debug_flags: unsafe extern "system" fn(*mut c_void, u32),
    pub set_staging_buffer_size: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    pub slot9: unsafe extern "system" fn(*mut c_void) -> i32,
    pub create_queue1: unsafe extern "system" fn(*mut c_void, *const u8, *const c_void, *mut *mut c_void) -> i32,
    pub slot11: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot12: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot13: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot14: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot15: unsafe extern "system" fn(*mut c_void) -> i32,
}

#[repr(C)]
pub struct FactoryObj {
    pub vtbl: &'static FactoryVtbl,
}

trap!(factory_slot9, "IDStorageFactoryX", 9);
trap!(factory_slot11, "IDStorageFactoryX", 11);
trap!(factory_slot12, "IDStorageFactoryX", 12);
trap!(factory_slot13, "IDStorageFactoryX", 13);
trap!(factory_slot14, "IDStorageFactoryX", 14);
trap!(factory_slot15, "IDStorageFactoryX", 15);

pub static FACTORY_VTBL: FactoryVtbl = FactoryVtbl {
    query_interface: factory_qi,
    add_ref: factory_add_ref,
    release: factory_add_ref,
    create_queue: factory_create_queue,
    open_file: factory_open_file,
    create_status_array: factory_create_status_array,
    set_cpu_affinity: factory_set_cpu_affinity,
    set_debug_flags: factory_set_debug_flags,
    set_staging_buffer_size: factory_set_staging_buffer_size,
    slot9: factory_slot9,
    create_queue1: factory_create_queue1,
    slot11: factory_slot11,
    slot12: factory_slot12,
    slot13: factory_slot13,
    slot14: factory_slot14,
    slot15: factory_slot15,
};

pub struct SyncFactory(pub FactoryObj);
unsafe impl Sync for SyncFactory {}
pub static FACTORY: SyncFactory = SyncFactory(FactoryObj { vtbl: &FACTORY_VTBL });

pub fn factory_ptr() -> *mut c_void {
    &FACTORY.0 as *const FactoryObj as *mut c_void
}

unsafe extern "system" fn factory_qi(this: *mut c_void, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    match unsafe { read_iid(riid) } {
        Some(i) if i == IID_FACTORY_X || i == IID_IUNKNOWN => {
            unsafe { qi_out(ppv, this) };
            S_OK
        }
        Some(i) => {
            report("IDStorageFactoryX::QueryInterface", E_NOINTERFACE, &format!("riid={}", iid_text(&i)));
            E_NOINTERFACE
        }
        None => E_POINTER,
    }
}

unsafe extern "system" fn factory_add_ref(_this: *mut c_void) -> u32 {
    1
}

unsafe fn create_queue_from(who: &str, desc: *const u8, extra: Option<u64>, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    if desc.is_null() {
        report(who, E_INVALIDARG, "null desc");
        return E_INVALIDARG;
    }
    let mut raw = [0u8; XQUEUE_DESC_SIZE];
    unsafe { core::ptr::copy_nonoverlapping(desc, raw.as_mut_ptr(), XQUEUE_DESC_SIZE) };
    let q = match QueueInner::create(raw) {
        Ok(q) => q,
        Err((hr, why)) => {
            report(who, hr, &why);
            return hr;
        }
    };
    let obj = Box::into_raw(Box::new(QueueObj { vtbl: &QUEUE_VTBL, refs: AtomicU32::new(1), inner: q.clone() }));
    let hr = unsafe { queue_qi(obj as *mut c_void, riid, ppv) };
    unsafe { queue_release(obj as *mut c_void) };
    let tail = extra.map(|v| format!(" desc+0x18={v:#x} (not modelled)")).unwrap_or_default();
    log(who, &format!("type={} capacity={} priority={} name='{}'{tail} -> hr={hr:#010x} queue={:p}", q.source_type, q.capacity, q.priority, q.name, unsafe { *ppv }));
    hr
}

unsafe extern "system" fn factory_create_queue(_this: *mut c_void, desc: *const u8, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    unsafe { create_queue_from("IDStorageFactoryX::CreateQueue", desc, None, riid, ppv) }
}

unsafe extern "system" fn factory_create_queue1(_this: *mut c_void, desc: *const u8, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    let extra = if desc.is_null() { None } else { Some(unsafe { core::ptr::read_unaligned(desc.add(XQUEUE_DESC_SIZE) as *const u64) }) };
    unsafe { create_queue_from("IDStorageFactoryX::CreateQueue (slot 10)", desc, extra, riid, ppv) }
}

unsafe extern "system" fn factory_open_file(_this: *mut c_void, path: *const u16, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    if path.is_null() {
        report("IDStorageFactoryX::OpenFile", E_INVALIDARG, "null path");
        return E_INVALIDARG;
    }
    let wide = unsafe { wide_string(path, 0x8000) };
    let os = <std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&wide);
    let file = match File::open(&os) {
        Ok(f) => f,
        Err(e) => {
            let hr = e.raw_os_error().map(|c| hresult_from_win32(c as u32)).unwrap_or(E_INVALIDARG);
            report("IDStorageFactoryX::OpenFile", hr, &format!("'{}': {e}", os.to_string_lossy()));
            return hr;
        }
    };
    let inner = Arc::new(FileInner {
        id: NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed),
        path: wide,
        file: RwLock::new(Some(Arc::new(file))),
    });
    LIVE_FILES.fetch_add(1, Ordering::Relaxed);
    let obj = Box::into_raw(Box::new(FileObj { vtbl: &FILE_VTBL, refs: AtomicU32::new(1), inner }));
    let hr = unsafe { file_qi(obj as *mut c_void, riid, ppv) };
    unsafe { file_release(obj as *mut c_void) };
    log("IDStorageFactoryX::OpenFile", &format!("'{}' -> hr={hr:#010x} file={:p}", os.to_string_lossy(), unsafe { *ppv }));
    hr
}

unsafe extern "system" fn factory_create_status_array(_this: *mut c_void, capacity: u32, name: *const u8, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let mut slots = Vec::new();
    if slots.try_reserve_exact(capacity as usize).is_err() {
        report("IDStorageFactoryX::CreateStatusArray", E_OUTOFMEMORY, &format!("capacity={capacity}"));
        return E_OUTOFMEMORY;
    }
    slots.extend((0..capacity).map(|_| AtomicI32::new(E_PENDING)));
    let inner = Arc::new(StatusInner { slots: slots.into_boxed_slice(), name: unsafe { c_string(name, STATUS_NAME_CHARS) } });
    let nm = inner.name.clone();
    let obj = Box::into_raw(Box::new(StatusObj { vtbl: &STATUS_VTBL, refs: AtomicU32::new(1), inner }));
    let hr = unsafe { status_qi(obj as *mut c_void, riid, ppv) };
    unsafe { status_release(obj as *mut c_void) };
    log("IDStorageFactoryX::CreateStatusArray", &format!("capacity={capacity} name='{nm}' -> hr={hr:#010x} array={:p}", unsafe { *ppv }));
    hr
}

pub fn process_affinity() -> u64 {
    let (mut p, mut s) = (0usize, 0usize);
    if unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut p, &mut s) } == 0 {
        return u64::MAX;
    }
    p as u64
}

unsafe extern "system" fn factory_set_cpu_affinity(_this: *mut c_void, mask: u64) -> i32 {
    let usable = mask & process_affinity();
    if usable == 0 {
        report("IDStorageFactoryX::SetCpuAffinity", E_INVALIDARG, &format!("mask {mask:#x} selects no CPU of this process"));
        return E_INVALIDARG;
    }
    CPU_AFFINITY.store(usable, Ordering::Relaxed);
    log("IDStorageFactoryX::SetCpuAffinity", &format!("mask={mask:#x} -> worker threads pinned to {usable:#x}"));
    S_OK
}

unsafe extern "system" fn factory_set_debug_flags(_this: *mut c_void, flags: u32) {
    DEBUG_FLAGS.store(flags, Ordering::Relaxed);
    if let Some(c) = crate::xcrd_driver::control_page() {
        unsafe { (c.flags as *mut u32).write_volatile(flags) };
    }
    log("IDStorageFactoryX::SetDebugFlags", &format!("flags={flags:#x}"));
}

unsafe extern "system" fn factory_set_staging_buffer_size(_this: *mut c_void, size: u32) -> i32 {
    let (q, f) = (LIVE_QUEUES.load(Ordering::Relaxed), LIVE_FILES.load(Ordering::Relaxed));
    if q != 0 || f != 0 {
        report("IDStorageFactoryX::SetStagingBufferSize", E_DSTORAGE_STAGING_BUFFER_LOCKED, &format!("size={size:#x} with {q} queues and {f} files open"));
        return E_DSTORAGE_STAGING_BUFFER_LOCKED;
    }
    STAGING_BUFFER_SIZE.store(size, Ordering::Relaxed);
    log("IDStorageFactoryX::SetStagingBufferSize", &format!("size={size:#x} (CPU decode path reads straight into the destination; no staging buffer is allocated)"));
    S_OK
}

pub struct FileInner {
    pub id: u32,
    pub path: Vec<u16>,
    pub file: RwLock<Option<Arc<File>>>,
}

impl FileInner {
    pub fn handle(&self) -> Option<Arc<File>> {
        self.file.read().ok()?.clone()
    }

    fn close(&self) -> bool {
        let was = self.file.write().map(|mut g| g.take().is_some()).unwrap_or(false);
        if was {
            LIVE_FILES.fetch_sub(1, Ordering::Relaxed);
        }
        was
    }
}

#[repr(C)]
pub struct FileVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub close: unsafe extern "system" fn(*mut c_void),
    pub get_handle: unsafe extern "system" fn(*mut c_void) -> isize,
    pub slot5: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot6: unsafe extern "system" fn(*mut c_void) -> i32,
    pub get_id: unsafe extern "system" fn(*mut c_void) -> u32,
    pub slot8: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot9: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot10: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot11: unsafe extern "system" fn(*mut c_void) -> i32,
}

#[repr(C)]
pub struct FileObj {
    pub vtbl: &'static FileVtbl,
    pub refs: AtomicU32,
    pub inner: Arc<FileInner>,
}

trap!(file_slot5, "IDStorageFileX", 5);
trap!(file_slot6, "IDStorageFileX", 6);
trap!(file_slot8, "IDStorageFileX", 8);
trap!(file_slot9, "IDStorageFileX", 9);
trap!(file_slot10, "IDStorageFileX", 10);
trap!(file_slot11, "IDStorageFileX", 11);

pub static FILE_VTBL: FileVtbl = FileVtbl {
    query_interface: file_qi,
    add_ref: file_add_ref,
    release: file_release,
    close: file_close,
    get_handle: file_get_handle,
    slot5: file_slot5,
    slot6: file_slot6,
    get_id: file_get_id,
    slot8: file_slot8,
    slot9: file_slot9,
    slot10: file_slot10,
    slot11: file_slot11,
};

pub unsafe fn as_file<'a>(p: *mut c_void) -> Option<&'a FileObj> {
    if !range_accessible(p as u64, 8, false) || unsafe { *(p as *const usize) } != &FILE_VTBL as *const FileVtbl as usize {
        return None;
    }
    Some(unsafe { &*(p as *const FileObj) })
}

unsafe extern "system" fn file_qi(this: *mut c_void, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    match unsafe { read_iid(riid) } {
        Some(i) if i == IID_FILE_X || i == IID_IUNKNOWN => {
            unsafe { file_add_ref(this) };
            unsafe { qi_out(ppv, this) };
            S_OK
        }
        Some(i) => {
            log("IDStorageFileX::QueryInterface", &format!("E_NOINTERFACE riid={}", iid_text(&i)));
            E_NOINTERFACE
        }
        None => E_POINTER,
    }
}

unsafe extern "system" fn file_add_ref(this: *mut c_void) -> u32 {
    let o = unsafe { &*(this as *const FileObj) };
    o.refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn file_release(this: *mut c_void) -> u32 {
    let o = unsafe { &*(this as *const FileObj) };
    let n = o.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        o.inner.close();
        drop(unsafe { Box::from_raw(this as *mut FileObj) });
    }
    n
}

unsafe extern "system" fn file_close(this: *mut c_void) {
    let o = unsafe { &*(this as *const FileObj) };
    let was = o.inner.close();
    log("IDStorageFileX::Close", &format!("file id={} closed={was}", o.inner.id));
}

unsafe extern "system" fn file_get_handle(this: *mut c_void) -> isize {
    let o = unsafe { &*(this as *const FileObj) };
    match o.inner.handle() {
        Some(f) => match dup_handle(f.as_raw_handle() as isize) {
            Ok(h) => h,
            Err(hr) => {
                report("IDStorageFileX::GetHandle", hr, "DuplicateHandle failed");
                0
            }
        },
        None => 0,
    }
}

unsafe extern "system" fn file_get_id(this: *mut c_void) -> u32 {
    let o = unsafe { &*(this as *const FileObj) };
    if o.inner.handle().is_some() {
        o.inner.id
    } else {
        u32::MAX
    }
}

pub struct StatusInner {
    pub slots: Box<[AtomicI32]>,
    pub name: String,
}

#[repr(C)]
pub struct StatusVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub is_complete: unsafe extern "system" fn(*mut c_void, u32) -> bool,
    pub get_hresult: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    pub slot5: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot6: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot7: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot8: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot9: unsafe extern "system" fn(*mut c_void) -> i32,
}

#[repr(C)]
pub struct StatusObj {
    pub vtbl: &'static StatusVtbl,
    pub refs: AtomicU32,
    pub inner: Arc<StatusInner>,
}

trap!(status_slot5, "IDStorageStatusArrayX", 5);
trap!(status_slot6, "IDStorageStatusArrayX", 6);
trap!(status_slot7, "IDStorageStatusArrayX", 7);
trap!(status_slot8, "IDStorageStatusArrayX", 8);
trap!(status_slot9, "IDStorageStatusArrayX", 9);

pub static STATUS_VTBL: StatusVtbl = StatusVtbl {
    query_interface: status_qi,
    add_ref: status_add_ref,
    release: status_release,
    is_complete: status_is_complete,
    get_hresult: status_get_hresult,
    slot5: status_slot5,
    slot6: status_slot6,
    slot7: status_slot7,
    slot8: status_slot8,
    slot9: status_slot9,
};

pub unsafe fn as_status<'a>(p: *mut c_void) -> Option<&'a StatusObj> {
    if !range_accessible(p as u64, 8, false) || unsafe { *(p as *const usize) } != &STATUS_VTBL as *const StatusVtbl as usize {
        return None;
    }
    Some(unsafe { &*(p as *const StatusObj) })
}

unsafe extern "system" fn status_qi(this: *mut c_void, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    match unsafe { read_iid(riid) } {
        Some(i) if i == IID_STATUS_ARRAY_X || i == IID_IUNKNOWN => {
            unsafe { status_add_ref(this) };
            unsafe { qi_out(ppv, this) };
            S_OK
        }
        Some(i) => {
            log("IDStorageStatusArrayX::QueryInterface", &format!("E_NOINTERFACE riid={}", iid_text(&i)));
            E_NOINTERFACE
        }
        None => E_POINTER,
    }
}

unsafe extern "system" fn status_add_ref(this: *mut c_void) -> u32 {
    let o = unsafe { &*(this as *const StatusObj) };
    o.refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn status_release(this: *mut c_void) -> u32 {
    let o = unsafe { &*(this as *const StatusObj) };
    let n = o.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        drop(unsafe { Box::from_raw(this as *mut StatusObj) });
    }
    n
}

unsafe extern "system" fn status_is_complete(this: *mut c_void, index: u32) -> bool {
    let o = unsafe { &*(this as *const StatusObj) };
    match o.inner.slots.get(index as usize) {
        Some(s) => s.load(Ordering::Acquire) != E_PENDING,
        None => false,
    }
}

unsafe extern "system" fn status_get_hresult(this: *mut c_void, index: u32) -> i32 {
    let o = unsafe { &*(this as *const StatusObj) };
    match o.inner.slots.get(index as usize) {
        Some(s) => s.load(Ordering::Acquire),
        None => {
            report("IDStorageStatusArrayX::GetHResult", E_DSTORAGE_INDEX_BOUND, &format!("index {index} >= capacity {}", o.inner.slots.len()));
            E_DSTORAGE_INDEX_BOUND
        }
    }
}

pub struct PcFence(pub usize);
unsafe impl Send for PcFence {}

impl PcFence {
    unsafe fn signal(&self, value: u64) -> i32 {
        let vt = unsafe { *(self.0 as *const *const usize) };
        let f: unsafe extern "system" fn(usize, u64) -> i32 = unsafe { core::mem::transmute(*vt.add(PC_FENCE_SLOT_SIGNAL)) };
        unsafe { f(self.0, value) }
    }
}

impl Drop for PcFence {
    fn drop(&mut self) {
        unsafe {
            let vt = *(self.0 as *const *const usize);
            let rel: unsafe extern "system" fn(usize) -> u32 = core::mem::transmute(*vt.add(2));
            rel(self.0);
        }
    }
}

pub enum Entry {
    Request { seq: u64, req: XRequest, file: Option<Arc<FileInner>>, pre_error: Option<i32> },
    Status { array: Arc<StatusInner>, index: u32 },
    Signal { fence: Option<PcFence>, raw: u64, value: u64 },
}

#[derive(Clone, Default)]
pub struct Failure {
    pub hr: i32,
    pub command: u32,
    pub req: XRequest,
    pub filename: Vec<u16>,
    pub name: String,
    pub object: u64,
    pub value: u64,
}

#[derive(Default)]
pub struct ErrorState {
    pub count: u32,
    pub first: Option<Failure>,
}

struct QState {
    pending: Vec<Entry>,
    tx: Option<Sender<Vec<Entry>>>,
    worker: Option<JoinHandle<()>>,
}

pub struct QueueInner {
    pub raw_desc: [u8; XQUEUE_DESC_SIZE],
    pub source_type: u32,
    pub capacity: u16,
    pub priority: i8,
    pub name: String,
    state: Mutex<QState>,
    pub errors: Mutex<ErrorState>,
    cancels: Mutex<Vec<(u64, u64, u64)>>,
    next_seq: AtomicU64,
    pub closed: AtomicBool,
    pub error_event: isize,
    pub completed: AtomicU64,
}

unsafe impl Send for QueueInner {}
unsafe impl Sync for QueueInner {}

impl QueueInner {
    pub fn create(raw: [u8; XQUEUE_DESC_SIZE]) -> Result<Arc<QueueInner>, (i32, String)> {
        let source_type = u32::from_le_bytes(raw[0..4].try_into().unwrap());
        let capacity = u16::from_le_bytes(raw[8..10].try_into().unwrap());
        let priority = raw[10] as i8;
        let name_ptr = u64::from_le_bytes(raw[0x10..0x18].try_into().unwrap());
        if !(PRIORITY_LOW..=PRIORITY_REALTIME).contains(&priority) {
            return Err((E_DSTORAGE_INVALID_QUEUE_PRIORITY, format!("priority {priority}")));
        }
        if source_type != QUEUE_SOURCE_FILE && source_type != QUEUE_SOURCE_MEMORY {
            return Err((E_DSTORAGE_INVALID_QUEUE_TYPE, format!("source type {source_type}")));
        }
        if source_type == QUEUE_SOURCE_MEMORY && priority != PRIORITY_REALTIME {
            return Err((E_DSTORAGE_INVALID_MEMORY_QUEUE_PRIORITY, format!("memory queue with priority {priority}")));
        }
        if capacity < 2 {
            return Err((E_DSTORAGE_INVALID_QUEUE_CAPACITY, format!("capacity {capacity}")));
        }
        let ev = unsafe { CreateEventW(core::ptr::null_mut(), 0, 0, core::ptr::null()) };
        if ev == 0 {
            return Err((hresult_from_win32(unsafe { GetLastError() }), "CreateEventW".into()));
        }
        let name = unsafe { c_string(name_ptr as *const u8, 0x40) };
        let q = Arc::new(QueueInner {
            raw_desc: raw,
            source_type,
            capacity,
            priority,
            name,
            state: Mutex::new(QState { pending: Vec::new(), tx: None, worker: None }),
            errors: Mutex::new(ErrorState::default()),
            cancels: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            error_event: ev,
            completed: AtomicU64::new(0),
        });
        let (tx, rx) = channel::<Vec<Entry>>();
        let wq = Arc::downgrade(&q);
        let worker = std::thread::Builder::new()
            .name("xcrdapi-queue".into())
            .spawn(move || {
                let mut applied = 0u64;
                let mut err_since_status: Option<i32> = None;
                while let Ok(batch) = rx.recv() {
                    let want = CPU_AFFINITY.load(Ordering::Relaxed);
                    if want != applied && want != 0 {
                        unsafe { SetThreadAffinityMask(GetCurrentThread(), want as usize) };
                        applied = want;
                    }
                    let q = match wq.upgrade() {
                        Some(q) => q,
                        None => return,
                    };
                    q.process_batch(batch, &mut err_since_status);
                }
            })
            .map_err(|e| (E_OUTOFMEMORY, format!("worker thread: {e}")))?;
        {
            let mut st = q.state.lock().unwrap();
            st.tx = Some(tx);
            st.worker = Some(worker);
        }
        LIVE_QUEUES.fetch_add(1, Ordering::Relaxed);
        Ok(q)
    }

    fn fail(&self, f: Failure) {
        let mut e = self.errors.lock().unwrap();
        e.count = e.count.saturating_add(1);
        if e.first.is_none() {
            e.first = Some(f);
        }
        drop(e);
        unsafe { SetEvent(self.error_event) };
    }

    fn cancelled(&self, seq: u64, tag: u64) -> bool {
        self.cancels.lock().unwrap().iter().any(|&(m, v, upto)| seq < upto && tag & m == v)
    }

    fn request_hr(&self, seq: u64, req: &XRequest, file: Option<&FileInner>, pre_error: Option<i32>) -> i32 {
        if let Some(hr) = pre_error {
            hr
        } else if self.cancelled(seq, req.cancellation_tag) {
            HR_OPERATION_ABORTED
        } else {
            self.run_request(req, file)
        }
    }

    fn process_batch(&self, batch: Vec<Entry>, err_since_status: &mut Option<i32>) {
        let mut it = batch.into_iter().peekable();
        while let Some(e) = it.next() {
            if !matches!(e, Entry::Request { .. }) {
                crate::stats::measure_entry(false, || self.process(e, None, err_since_status));
                self.completed.fetch_add(1, Ordering::Release);
                continue;
            }
            let mut group = vec![e];
            while matches!(it.peek(), Some(Entry::Request { .. })) {
                group.push(it.next().unwrap());
            }
            let hrs: Vec<core::sync::atomic::AtomicI32> = (0..group.len()).map(|_| core::sync::atomic::AtomicI32::new(0)).collect();
            crate::pool::for_each(group.len(), &|i| {
                if let Entry::Request { seq, req, file, pre_error } = &group[i] {
                    let hr = crate::stats::measure_entry(true, || self.request_hr(*seq, req, file.as_deref(), *pre_error));
                    hrs[i].store(hr, Ordering::Release);
                }
            });
            for (e, hr) in group.into_iter().zip(hrs) {
                self.process(e, Some(hr.into_inner()), err_since_status);
                self.completed.fetch_add(1, Ordering::Release);
            }
        }
    }

    fn process(&self, e: Entry, done: Option<i32>, err_since_status: &mut Option<i32>) {
        match e {
            Entry::Request { seq, req, file, pre_error } => {
                let hr = match done {
                    Some(hr) => hr,
                    None => self.request_hr(seq, &req, file.as_deref(), pre_error),
                };
                if hr < 0 {
                    if err_since_status.is_none() {
                        *err_since_status = Some(hr);
                    }
                    if hr != HR_OPERATION_ABORTED {
                        report("IDStorageQueueX::request", hr, &format!("queue='{}' options={:#x} off={:#x} size={:#x} dst={:#x} dsize={:#x}", self.name, req.options, req.source_offset, req.source_size, req.destination, req.destination_size));
                        self.fail(Failure {
                            hr,
                            command: COMMAND_TYPE_REQUEST,
                            req,
                            filename: file.map(|f| f.path.clone()).unwrap_or_default(),
                            name: unsafe { c_string(req.name as *const u8, XERR_NAME_CHARS) },
                            ..Default::default()
                        });
                    }
                }
            }
            Entry::Status { array, index } => {
                let hr = err_since_status.take().unwrap_or(S_OK);
                array.slots[index as usize].store(hr, Ordering::Release);
            }
            Entry::Signal { fence, raw, value } => match fence {
                Some(f) => {
                    let hr = unsafe { f.signal(value) };
                    if hr < 0 {
                        report("IDStorageQueueX::EnqueueSignal", hr, &format!("ID3D12Fence::Signal({value}) failed"));
                        self.fail(Failure { hr, command: COMMAND_TYPE_SIGNAL, object: raw, value, ..Default::default() });
                    }
                }
                None => self.fail(Failure { hr: E_DSTORAGE_INVALID_FENCE, command: COMMAND_TYPE_SIGNAL, object: raw, value, ..Default::default() }),
            },
        }
    }

    fn run_request(&self, req: &XRequest, file: Option<&FileInner>) -> i32 {
        let dsize = req.destination_size as usize;
        if !range_accessible(req.destination, dsize, true) {
            return E_DSTORAGE_ACCESS_VIOLATION;
        }
        let dst = unsafe { core::slice::from_raw_parts_mut(req.destination as *mut u8, dsize) };
        let result = if req.memory_source() {
            if !range_accessible(req.source, req.source_size as usize, false) {
                return E_DSTORAGE_ACCESS_VIOLATION;
            }
            let src = unsafe { core::slice::from_raw_parts(req.source as *const u8, req.source_size as usize) };
            execute_with_intermediate(req.compression(), req.field34, Source::Memory(src), req.source_size, dst)
        } else {
            let handle = match file.and_then(|f| f.handle()) {
                Some(h) => h,
                None => return E_DSTORAGE_INVALID_FILE_HANDLE,
            };
            execute_with_intermediate(req.compression(), req.field34, Source::File(&handle, req.source_offset), req.source_size, dst)
        };
        match result {
            Ok(o) => {
                crate::write_note::written(req.destination, o.written.min(dsize));
                if o.written != dsize {
                    log("IDStorageQueueX::request", &format!("zlib stream ended after {:#x} of {dsize:#x} destination bytes (off={:#x})", o.written, req.source_offset));
                }
                S_OK
            }
            Err(hr) => hr,
        }
    }

    pub fn enqueue(&self, e: Entry) -> Result<(), Entry> {
        if self.closed.load(Ordering::Acquire) {
            return Err(e);
        }
        let mut st = self.state.lock().unwrap();
        crate::stats::on_enqueue_q(self as *const _ as usize, st.pending.is_empty());
        st.pending.push(e);
        if st.pending.len() >= (self.capacity / 2).max(1) as usize {
            crate::stats::on_flush(self as *const _ as usize, st.pending.len(), false, self.priority);
            Self::flush(&mut st);
        }
        Ok(())
    }

    fn flush(st: &mut QState) {
        if st.pending.is_empty() {
            return;
        }
        let batch = core::mem::take(&mut st.pending);
        if let Some(tx) = &st.tx {
            let _ = tx.send(batch);
        }
    }

    pub fn submit(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let mut st = self.state.lock().unwrap();
        crate::stats::on_flush(self as *const _ as usize, st.pending.len(), true, self.priority);
        Self::flush(&mut st);
        true
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::AcqRel)
    }

    pub fn cancel(&self, mask: u64, value: u64) -> usize {
        let upto = self.next_seq.load(Ordering::Acquire);
        self.cancels.lock().unwrap().push((mask, value, upto));
        let st = self.state.lock().unwrap();
        st.pending.iter().filter(|e| matches!(e, Entry::Request { req, .. } if req.cancellation_tag & mask == value)).count()
    }

    pub fn query(&self, out: &mut [u8; XQUEUE_INFO_SIZE]) {
        out[..XQUEUE_DESC_SIZE].copy_from_slice(&self.raw_desc);
        let st = self.state.lock().unwrap();
        let pending = st.pending.len();
        let cap = self.capacity as usize;
        let empty = cap.saturating_sub(1).saturating_sub(pending) as u16;
        let half = (cap / 2).max(1);
        let until = half.saturating_sub(pending);
        let until = if until == 0 { half } else { until } as u16;
        out[0x18..0x1a].copy_from_slice(&empty.to_le_bytes());
        out[0x1a..0x1c].copy_from_slice(&until.to_le_bytes());
    }

    pub fn close(&self) -> bool {
        if self.closed.swap(true, Ordering::AcqRel) {
            return false;
        }
        let (left, tx, worker) = {
            let mut st = self.state.lock().unwrap();
            (core::mem::take(&mut st.pending), st.tx.take(), st.worker.take())
        };
        for e in left {
            if let Entry::Status { array, index } = e {
                array.slots[index as usize].store(E_DSTORAGE_QUEUE_CLOSED, Ordering::Release);
            }
        }
        drop(tx);
        if let Some(w) = worker {
            if w.thread().id() != std::thread::current().id() {
                let _ = w.join();
            }
        }
        LIVE_QUEUES.fetch_sub(1, Ordering::Relaxed);
        true
    }

    pub fn write_error_record(&self, rec: &mut [u8; XERROR_RECORD_SIZE]) {
        rec.fill(0);
        let e = self.errors.lock().unwrap();
        let f = match (&e.first, e.count) {
            (Some(f), n) if n > 0 => f.clone(),
            _ => {
                rec[0x0c..0x10].copy_from_slice(&COMMAND_TYPE_NONE.to_le_bytes());
                return;
            }
        };
        rec[0x00..0x04].copy_from_slice(&e.count.to_le_bytes());
        rec[0x08..0x0c].copy_from_slice(&f.hr.to_le_bytes());
        rec[0x0c..0x10].copy_from_slice(&f.command.to_le_bytes());
        match f.command {
            COMMAND_TYPE_REQUEST => {
                rec[0x10..0x18].copy_from_slice(&f.req.options.to_le_bytes());
                rec[0x18..0x20].copy_from_slice(&f.req.destination.to_le_bytes());
                rec[0x20..0x24].copy_from_slice(&f.req.destination_size.to_le_bytes());
                if f.req.memory_source() {
                    rec[0x238..0x240].copy_from_slice(&f.req.source.to_le_bytes());
                } else {
                    for (i, c) in f.filename.iter().take(XERR_FILENAME_CHARS - 1).enumerate() {
                        let o = XERR_FILENAME_OFF + i * 2;
                        rec[o..o + 2].copy_from_slice(&c.to_le_bytes());
                    }
                }
                rec[0x230..0x238].copy_from_slice(&f.req.source_offset.to_le_bytes());
                rec[0x240..0x244].copy_from_slice(&f.req.source_size.to_le_bytes());
                rec[0x248..0x250].copy_from_slice(&f.req.cancellation_tag.to_le_bytes());
                for (i, b) in f.name.bytes().take(XERR_NAME_CHARS - 1).enumerate() {
                    rec[XERR_NAME_OFF + i] = b;
                }
            }
            COMMAND_TYPE_STATUS => {
                rec[0x10..0x18].copy_from_slice(&f.object.to_le_bytes());
                rec[0x18..0x1c].copy_from_slice(&(f.value as u32).to_le_bytes());
            }
            _ => {
                rec[0x10..0x18].copy_from_slice(&f.object.to_le_bytes());
                rec[0x18..0x20].copy_from_slice(&f.value.to_le_bytes());
            }
        }
    }
}

impl Drop for QueueInner {
    fn drop(&mut self) {
        if self.error_event != 0 {
            unsafe { CloseHandle(self.error_event) };
        }
    }
}

#[repr(C)]
pub struct QueueVtbl {
    pub query_interface: unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    pub enqueue_request: unsafe extern "system" fn(*mut c_void, *const u8),
    pub enqueue_status: unsafe extern "system" fn(*mut c_void, *mut c_void, u32),
    pub enqueue_signal: unsafe extern "system" fn(*mut c_void, *mut c_void, u64),
    pub submit: unsafe extern "system" fn(*mut c_void),
    pub cancel_requests_with_tag: unsafe extern "system" fn(*mut c_void, u64, u64),
    pub close: unsafe extern "system" fn(*mut c_void),
    pub get_error_event: unsafe extern "system" fn(*mut c_void) -> isize,
    pub retrieve_error_record: unsafe extern "system" fn(*mut c_void, *mut u8),
    pub query: unsafe extern "system" fn(*mut c_void, *mut u8),
    pub slot12: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot13: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot14: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot15: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot16: unsafe extern "system" fn(*mut c_void) -> i32,
    pub slot17: unsafe extern "system" fn(*mut c_void) -> i32,
}

#[repr(C)]
pub struct QueueObj {
    pub vtbl: &'static QueueVtbl,
    pub refs: AtomicU32,
    pub inner: Arc<QueueInner>,
}

trap!(queue_slot12, "IDStorageQueueX", 12);
trap!(queue_slot13, "IDStorageQueueX", 13);
trap!(queue_slot14, "IDStorageQueueX", 14);
trap!(queue_slot15, "IDStorageQueueX", 15);
trap!(queue_slot16, "IDStorageQueueX", 16);
trap!(queue_slot17, "IDStorageQueueX", 17);

pub static QUEUE_VTBL: QueueVtbl = QueueVtbl {
    query_interface: queue_qi,
    add_ref: queue_add_ref,
    release: queue_release,
    enqueue_request: queue_enqueue_request,
    enqueue_status: queue_enqueue_status,
    enqueue_signal: queue_enqueue_signal,
    submit: queue_submit,
    cancel_requests_with_tag: queue_cancel,
    close: queue_close,
    get_error_event: queue_get_error_event,
    retrieve_error_record: queue_retrieve_error_record,
    query: queue_query,
    slot12: queue_slot12,
    slot13: queue_slot13,
    slot14: queue_slot14,
    slot15: queue_slot15,
    slot16: queue_slot16,
    slot17: queue_slot17,
};

unsafe fn queue<'a>(this: *mut c_void) -> &'a QueueObj {
    unsafe { &*(this as *const QueueObj) }
}

unsafe extern "system" fn queue_qi(this: *mut c_void, riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    match unsafe { read_iid(riid) } {
        Some(i) if i == IID_QUEUE_X || i == IID_QUEUE_X1 || i == IID_IUNKNOWN => {
            unsafe { queue_add_ref(this) };
            unsafe { qi_out(ppv, this) };
            S_OK
        }
        Some(i) => {
            report("IDStorageQueueX::QueryInterface", E_NOINTERFACE, &format!("riid={}", iid_text(&i)));
            E_NOINTERFACE
        }
        None => E_POINTER,
    }
}

unsafe extern "system" fn queue_add_ref(this: *mut c_void) -> u32 {
    unsafe { queue(this) }.refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn queue_release(this: *mut c_void) -> u32 {
    let o = unsafe { queue(this) };
    let n = o.refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        o.inner.close();
        drop(unsafe { Box::from_raw(this as *mut QueueObj) });
    }
    n
}

unsafe extern "system" fn queue_enqueue_request(this: *mut c_void, preq: *const u8) {
    let q = &unsafe { queue(this) }.inner;
    if preq.is_null() {
        report("IDStorageQueueX::EnqueueRequest", E_INVALIDARG, "null request");
        return;
    }
    let req = unsafe { XRequest::read(preq) };
    let mut pre_error = validate(&req, q.source_type).err().map(|r| {
        report("IDStorageQueueX::EnqueueRequest", r.hr, &format!("pre-validation failed, the request will fail: {}", r.why));
        r.hr
    });
    let mut file = None;
    if !req.memory_source() && pre_error.is_none() {
        if req.source == 0 {
            report("IDStorageQueueX::EnqueueRequest", HR_NOACCESS, "DSTORAGE_REQUEST::File is null");
            pre_error.get_or_insert(HR_NOACCESS);
        } else {
            match unsafe { as_file(req.source as *mut c_void) } {
                Some(f) => {
                    if f.inner.handle().is_none() {
                        pre_error.get_or_insert(E_DSTORAGE_INVALID_FILE_HANDLE);
                    }
                    file = Some(f.inner.clone());
                }
                None => {
                    report("IDStorageQueueX::EnqueueRequest", E_DSTORAGE_INVALID_FILE_HANDLE, &format!("source {:#x} is not an IDStorageFileX from this runtime", req.source));
                    pre_error.get_or_insert(E_DSTORAGE_INVALID_FILE_HANDLE);
                }
            }
        }
    }
    let entry = Entry::Request { seq: q.next_seq(), req, file, pre_error };
    if q.enqueue(entry).is_err() {
        report("IDStorageQueueX::EnqueueRequest", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
    }
}

unsafe extern "system" fn queue_enqueue_status(this: *mut c_void, array: *mut c_void, index: u32) {
    let q = &unsafe { queue(this) }.inner;
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::EnqueueStatus", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return;
    }
    let arr = match unsafe { as_status(array) } {
        Some(a) => a.inner.clone(),
        None => {
            report("IDStorageQueueX::EnqueueStatus", E_DSTORAGE_INVALID_STATUS_ARRAY, &format!("{array:p} is not an IDStorageStatusArrayX from this runtime"));
            return;
        }
    };
    if index as usize >= arr.slots.len() {
        report("IDStorageQueueX::EnqueueStatus", E_DSTORAGE_INDEX_BOUND, &format!("index {index} >= capacity {}", arr.slots.len()));
        return;
    }
    arr.slots[index as usize].store(E_PENDING, Ordering::Release);
    if let Err(Entry::Status { array, index }) = q.enqueue(Entry::Status { array: arr, index }) {
        array.slots[index as usize].store(E_DSTORAGE_QUEUE_CLOSED, Ordering::Release);
        report("IDStorageQueueX::EnqueueStatus", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
    }
}

unsafe extern "system" fn queue_enqueue_signal(this: *mut c_void, fence: *mut c_void, value: u64) {
    let q = &unsafe { queue(this) }.inner;
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::EnqueueSignal", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return;
    }
    let pc = if fence.is_null() {
        None
    } else {
        let mut out: *mut c_void = core::ptr::null_mut();
        let hr = unsafe {
            let vt = *(fence as *const *const usize);
            let qi: unsafe extern "system" fn(*mut c_void, *const Iid, *mut *mut c_void) -> i32 = core::mem::transmute(*vt);
            qi(fence, &IID_D3D12_FENCE_PC, &mut out)
        };
        if hr >= 0 && !out.is_null() {
            Some(PcFence(out as usize))
        } else {
            report(
                "IDStorageQueueX::EnqueueSignal",
                E_DSTORAGE_INVALID_FENCE,
                &format!("fence {fence:p} does not expose PC ID3D12Fence (QI hr={hr:#010x}); console fence IID {} has no host implementation", iid_text(&IID_D3D12_FENCE_X)),
            );
            None
        }
    };
    if pc.is_none() && fence.is_null() {
        report("IDStorageQueueX::EnqueueSignal", E_DSTORAGE_INVALID_FENCE, "null fence");
    }
    if q.enqueue(Entry::Signal { fence: pc, raw: fence as u64, value }).is_err() {
        report("IDStorageQueueX::EnqueueSignal", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
    }
}

unsafe extern "system" fn queue_submit(this: *mut c_void) {
    let q = &unsafe { queue(this) }.inner;
    if !q.submit() {
        report("IDStorageQueueX::Submit", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
    }
}

unsafe extern "system" fn queue_cancel(this: *mut c_void, mask: u64, value: u64) {
    let q = &unsafe { queue(this) }.inner;
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::CancelRequestsWithTag", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return;
    }
    if mask & value != value {
        log("IDStorageQueueX::CancelRequestsWithTag", &format!("mask({mask:#x}) & value({value:#x}) != value, cancellation matches nothing"));
    }
    let n = q.cancel(mask, value);
    log("IDStorageQueueX::CancelRequestsWithTag", &format!("mask={mask:#x} value={value:#x} unsubmitted matches={n}"));
}

unsafe extern "system" fn queue_close(this: *mut c_void) {
    let q = &unsafe { queue(this) }.inner;
    if !q.close() {
        report("IDStorageQueueX::Close", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}' already closed", q.name));
    }
}

unsafe extern "system" fn queue_get_error_event(this: *mut c_void) -> isize {
    let q = &unsafe { queue(this) }.inner;
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::GetErrorEvent", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return 0;
    }
    match dup_handle(q.error_event) {
        Ok(h) => h,
        Err(hr) => {
            report("IDStorageQueueX::GetErrorEvent", hr, "DuplicateHandle failed");
            0
        }
    }
}

unsafe extern "system" fn queue_retrieve_error_record(this: *mut c_void, out: *mut u8) {
    let q = &unsafe { queue(this) }.inner;
    if out.is_null() {
        report("IDStorageQueueX::RetrieveErrorRecord", E_POINTER, "null record");
        return;
    }
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::RetrieveErrorRecord", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return;
    }
    let mut rec = [0u8; XERROR_RECORD_SIZE];
    q.write_error_record(&mut rec);
    unsafe { core::ptr::copy_nonoverlapping(rec.as_ptr(), out, XERROR_RECORD_SIZE) };
}

unsafe extern "system" fn queue_query(this: *mut c_void, out: *mut u8) {
    let q = &unsafe { queue(this) }.inner;
    if out.is_null() {
        report("IDStorageQueueX::Query", E_POINTER, "null info");
        return;
    }
    if q.closed.load(Ordering::Acquire) {
        report("IDStorageQueueX::Query", E_DSTORAGE_QUEUE_CLOSED, &format!("queue '{}'", q.name));
        return;
    }
    let mut info = [0u8; XQUEUE_INFO_SIZE];
    q.query(&mut info);
    unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), out, XQUEUE_INFO_SIZE) };
}
