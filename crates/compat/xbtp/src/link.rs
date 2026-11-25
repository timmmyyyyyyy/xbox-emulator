use crate::sys::*;
use core::ffi::c_void;
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicUsize, Ordering};

#[path = "link_fmt.rs"]
mod decode;
#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;

pub use decode::{format_link_address, LineCallback, XbtpLinkDecodePacketRecord, XbtpLinkDecodeStatisticRecord};

pub type SOCKET = usize;
pub const INVALID_SOCKET: SOCKET = usize::MAX;

pub const HANDLE_SIZE: usize = 0x540;
pub const MSG_SIZE: usize = 0x80;

pub const LINK_E_CONNECT_ADDRESS: HRESULT = 0x8c11_0500u32 as i32;
pub const LINK_E_CONNECT_REFUSED: HRESULT = 0x8c11_0501u32 as i32;
pub const LINK_E_CONNECT_RESPONSE: HRESULT = 0x8c11_0502u32 as i32;
pub const LINK_E_ACCEPT_WAIT: HRESULT = 0x8c11_0504u32 as i32;
pub const LINK_E_ACCEPT_REQUEST: HRESULT = 0x8c11_0505u32 as i32;
pub const LINK_E_ACCEPT_RESPONSE: HRESULT = 0x8c11_0506u32 as i32;
pub const LINK_E_BAD_MODE: HRESULT = 0x8c11_0507u32 as i32;
pub const LINK_E_ALREADY_ACTIVE: HRESULT = 0x8c11_050bu32 as i32;
pub const LINK_E_NO_DEVICE: HRESULT = 0x8c11_050cu32 as i32;
pub const LINK_E_BIND_SEND: HRESULT = 0x8c11_050fu32 as i32;
pub const LINK_E_BIND_RECV: HRESULT = 0x8c11_0510u32 as i32;
pub const LINK_E_TOO_LARGE: HRESULT = 0x8c11_0514u32 as i32;
pub const LINK_E_NULL_HANDLE: HRESULT = 0x8c11_051du32 as i32;

pub const MSG_DATA: u32 = 0xfe00_0001;
pub const MSG_CONNECT: u32 = 0xfd00_0002;
pub const MSG_ACCEPT: u32 = 0xfc00_0003;
pub const MSG_END: u32 = 0xfa00_0005;
pub const MSG_BIND: u32 = 0x7f00_0080;
pub const MSG_TCP_CONNECT: u32 = 0xf700_0008;
pub const MSG_GATEWAY: u32 = 0xbf00_0040;

pub const IOCTL_SEND: u32 = 0x0022_0007;
pub const IOCTL_RECV: u32 = 0x0022_000b;
pub const IOCTL_RESERVE: u32 = 0x0022_0012;

pub const MODE_MASK: u32 = 0xf000_0000;
pub const MODE_DEVICE: u32 = 0x3000_0000;
pub const MODE_TCP: u32 = 0x4000_0000;
pub const FLAG_BOUND: u32 = 1;
pub const FLAG_LISTENING: u32 = 2;
pub const FLAG_CONNECTED: u32 = 4;
pub const FLAG_STOP: u32 = 8;
pub const FLAG_SHUTDOWN: u32 = 0x80;
pub const FLAG_TCP: u32 = 0x100;

pub const ROUTER_PORT: u16 = 0x1069;

const OFF_DEVICE: usize = 0x0;
const OFF_SOCKET: usize = 0x10;
const OFF_IO_CS: usize = 0x20;
const OFF_LIST_CS: usize = 0x48;
const OFF_TARGET: usize = 0x70;
const OFF_REMOTE: usize = 0x84;
const OFF_LOCAL: usize = 0x98;
const OFF_BIND_MSG: usize = 0xac;
const OFF_REFS: usize = 0x12c;
const OFF_IDLE_EVENT: usize = 0x130;
const OFF_ACCEPT_THREAD: usize = 0x138;
const OFF_WAKE_EVENT: usize = 0x140;
const OFF_READY_EVENT: usize = 0x148;
const OFF_ACCEPT_STATE: usize = 0x150;
const OFF_ACCEPT_SEQ: usize = 0x154;
const OFF_PENDING_REQ: usize = 0x158;
const OFF_PATH_TCP: usize = 0x534;
const OFF_PATH_XTD: usize = 0x538;
const OFF_FLAGS: usize = 0x53c;

const ADDR_SIZE: usize = 0x14;
const OPTS_OFF: usize = 0x38;
const OPTS_SIZE: usize = 0x48;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WSABUF {
    pub len: u32,
    pub buf: *mut u8,
}

#[repr(C)]
pub struct ADDRINFOW {
    pub ai_flags: i32,
    pub ai_family: i32,
    pub ai_socktype: i32,
    pub ai_protocol: i32,
    pub ai_addrlen: usize,
    pub ai_canonname: *mut u16,
    pub ai_addr: *mut u8,
    pub ai_next: *mut ADDRINFOW,
}

#[repr(C)]
struct CriticalSection([u8; 0x28]);

#[repr(C)]
struct SystemTime([u16; 8]);

#[link(name = "kernel32")]
extern "system" {
    fn InitializeCriticalSection(cs: *mut CriticalSection);
    fn DeleteCriticalSection(cs: *mut CriticalSection);
    fn EnterCriticalSection(cs: *mut CriticalSection);
    fn LeaveCriticalSection(cs: *mut CriticalSection);
    fn CancelIo(h: HANDLE) -> i32;
    fn SetThreadDescription(h: HANDLE, d: *const u16) -> HRESULT;
    fn GetSystemTimePreciseAsFileTime(ft: *mut u64);
    fn SystemTimeToFileTime(st: *const SystemTime, ft: *mut u64) -> i32;
}

#[allow(clashing_extern_declarations)]
#[link(name = "ws2_32")]
extern "system" {
    pub fn WSAStartup(ver: u16, data: *mut u8) -> i32;
    pub fn WSAGetLastError() -> i32;
    pub fn socket(af: i32, ty: i32, proto: i32) -> SOCKET;
    pub fn setsockopt(s: SOCKET, level: i32, name: i32, val: *const u8, len: i32) -> i32;
    pub fn connect(s: SOCKET, addr: *const u8, len: i32) -> i32;
    pub fn send(s: SOCKET, buf: *const u8, len: i32, flags: i32) -> i32;
    pub fn recv(s: SOCKET, buf: *mut u8, len: i32, flags: i32) -> i32;
    pub fn closesocket(s: SOCKET) -> i32;
    pub fn shutdown(s: SOCKET, how: i32) -> i32;
    pub fn WSASend(s: SOCKET, bufs: *const WSABUF, n: u32, sent: *mut u32, flags: u32, ov: *mut OVERLAPPED, cr: *const c_void) -> i32;
    pub fn WSARecv(s: SOCKET, bufs: *const WSABUF, n: u32, got: *mut u32, flags: *mut u32, ov: *mut OVERLAPPED, cr: *const c_void) -> i32;
    pub fn GetAddrInfoW(node: *const u16, service: *const u16, hints: *const ADDRINFOW, res: *mut *mut ADDRINFOW) -> i32;
    pub fn FreeAddrInfoW(ai: *mut ADDRINFOW);
}

pub struct DeviceOps {
    pub open: unsafe fn() -> HANDLE,
    pub ioctl: unsafe fn(HANDLE, u32, *const c_void, u32, *mut c_void, u32, *mut u32, *mut OVERLAPPED) -> i32,
    pub cancel_io: unsafe fn(HANDLE) -> i32,
    pub cancel_io_ex: unsafe fn(HANDLE, *mut OVERLAPPED) -> i32,
    pub close: unsafe fn(HANDLE) -> i32,
}

unsafe fn real_open() -> HANDLE {
    let name = wide("\\??\\xbtplink");
    CreateFileW(name.as_ptr(), 0xc000_0000, 0, null_mut(), 3, 0x4000_0000, null_mut())
}

unsafe fn real_ioctl(h: HANDLE, code: u32, i: *const c_void, il: u32, o: *mut c_void, ol: u32, r: *mut u32, ov: *mut OVERLAPPED) -> i32 {
    DeviceIoControl(h, code, i, il, o, ol, r, ov)
}

unsafe fn real_cancel_io(h: HANDLE) -> i32 {
    CancelIo(h)
}

unsafe fn real_cancel_io_ex(h: HANDLE, ov: *mut OVERLAPPED) -> i32 {
    CancelIoEx(h, ov)
}

unsafe fn real_close(h: HANDLE) -> i32 {
    CloseHandle(h)
}

pub static REAL_DEVICE: DeviceOps = DeviceOps {
    open: real_open,
    ioctl: real_ioctl,
    cancel_io: real_cancel_io,
    cancel_io_ex: real_cancel_io_ex,
    close: real_close,
};

static DEVICE_OPS: AtomicPtr<DeviceOps> = AtomicPtr::new(&crate::link_dev::DEVICE as *const DeviceOps as *mut DeviceOps);

pub fn set_device_ops(ops: &'static DeviceOps) {
    DEVICE_OPS.store(ops as *const DeviceOps as *mut DeviceOps, Ordering::SeqCst);
}

fn dev() -> &'static DeviceOps {
    unsafe { &*DEVICE_OPS.load(Ordering::SeqCst) }
}

static INIT_STATE: AtomicU32 = AtomicU32::new(0);
static INIT_ERROR: AtomicI32 = AtomicI32::new(0);
static WSA_STARTED: AtomicU32 = AtomicU32::new(0);
static EPOCH_PENDING: AtomicU32 = AtomicU32::new(1);
static EPOCH_ERROR: AtomicI32 = AtomicI32::new(0);
static RESERVE_STATE: AtomicU32 = AtomicU32::new(0);
static RESERVE_DEVICE: AtomicUsize = AtomicUsize::new(usize::MAX);
static mut SYSTEM_CS: CriticalSection = CriticalSection([0; 0x28]);
static mut RESERVE_OV: OVERLAPPED = OVERLAPPED { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: null_mut() };
static mut RESERVE_OUT: [u64; 3] = [0; 3];
static mut REDIRECT: [u8; 0x18] = [0; 0x18];

#[cfg(test)]
fn reset_reserved_device() {
    RESERVE_DEVICE.store(usize::MAX, Ordering::SeqCst);
    RESERVE_STATE.store(0, Ordering::SeqCst);
}

#[inline]
unsafe fn at<T>(h: *mut u8, off: usize) -> *mut T {
    h.add(off) as *mut T
}

#[inline]
unsafe fn flags<'a>(h: *mut u8) -> &'a AtomicU32 {
    &*(h.add(OFF_FLAGS) as *const AtomicU32)
}

#[inline]
unsafe fn atom<'a>(h: *mut u8, off: usize) -> &'a AtomicU32 {
    &*(h.add(off) as *const AtomicU32)
}

fn wsa_hr(e: i32) -> HRESULT {
    hr_from_win32(e as u32)
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkInitializeSystem() -> HRESULT {
    let st = loop {
        match INIT_STATE.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst) {
            Err(1) => Sleep(10),
            Ok(v) | Err(v) => break v,
        }
    };
    if st == 2 {
        return S_OK;
    }
    if st == 3 {
        return INIT_ERROR.load(Ordering::SeqCst);
    }
    InitializeCriticalSection(core::ptr::addr_of_mut!(SYSTEM_CS));
    let mut data = [0u8; 0x198];
    let mut hr: HRESULT;
    if WSAStartup(0x202, data.as_mut_ptr()) != 0 {
        hr = wsa_hr(WSAGetLastError());
        if hr < 0 {
            INIT_ERROR.store(hr, Ordering::SeqCst);
            INIT_STATE.swap(3, Ordering::SeqCst);
            return hr;
        }
    } else {
        WSA_STARTED.store(1, Ordering::SeqCst);
    }
    if EPOCH_PENDING.swap(0, Ordering::SeqCst) != 0 {
        GetTickCount64();
        let mut now = 0u64;
        GetSystemTimePreciseAsFileTime(&mut now);
        let st = SystemTime([1970, 1, 0, 1, 0, 0, 0, 0]);
        let mut ft = 0u64;
        if SystemTimeToFileTime(&st, &mut ft) == 0 {
            EPOCH_ERROR.store(hr_last_error(), Ordering::SeqCst);
        }
    }
    hr = EPOCH_ERROR.load(Ordering::SeqCst);
    if hr < 0 {
        INIT_ERROR.store(hr, Ordering::SeqCst);
        INIT_STATE.swap(3, Ordering::SeqCst);
    } else {
        INIT_STATE.swap(2, Ordering::SeqCst);
    }
    hr
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkCreateHandle() -> *mut u8 {
    let mut fl = 0u32;
    if XbtpLinkInitializeSystem() != 0 {
        if WSA_STARTED.load(Ordering::SeqCst) == 0 {
            return null_mut();
        }
        fl = FLAG_TCP;
    }
    let h = HeapAlloc(GetProcessHeap(), 8, HANDLE_SIZE) as *mut u8;
    if h.is_null() {
        return null_mut();
    }
    InitializeCriticalSection(at(h, OFF_IO_CS));
    InitializeCriticalSection(at(h, OFF_LIST_CS));
    *at::<u32>(h, OFF_REFS) = 1;
    let ev = CreateEventW(null_mut(), 1, 0, null());
    *at::<u64>(h, 0) = 0;
    *at::<u64>(h, 8) = 0;
    *at::<usize>(h, OFF_SOCKET) = INVALID_SOCKET;
    *at::<*mut u8>(h, 0x3a0) = h.add(0x2c8);
    *at::<*mut u8>(h, 0x490) = h.add(0x3b8);
    *at::<HANDLE>(h, OFF_IDLE_EVENT) = ev;
    *at::<u32>(h, OFF_FLAGS) = fl;
    *at::<*mut u8>(h, 0x2d8) = h;
    *at::<u32>(h, 0x2e8) = 1;
    *at::<u32>(h, 0x2f4) = 1;
    *at::<*mut u8>(h, 0x3c8) = h;
    *at::<u32>(h, 0x3d8) = 1;
    *at::<u32>(h, 0x3e4) = 1;
    if !ev.is_null() {
        return h;
    }
    DeleteCriticalSection(at(h, OFF_IO_CS));
    DeleteCriticalSection(at(h, OFF_LIST_CS));
    HeapFree(GetProcessHeap(), 0, h as *mut c_void);
    null_mut()
}

unsafe fn reset_handle(h: *mut u8) {
    if h.is_null() {
        return;
    }
    XbtpLinkShutdownHandle(h);
    if flags(h).load(Ordering::SeqCst) & FLAG_LISTENING != 0 && !(*at::<HANDLE>(h, OFF_ACCEPT_THREAD)).is_null() {
        let th = *at::<HANDLE>(h, OFF_ACCEPT_THREAD);
        while WaitForSingleObject(th, 100) == 0x102 {
            flags(h).fetch_or(FLAG_STOP, Ordering::SeqCst);
            SetEvent(*at::<HANDLE>(h, OFF_WAKE_EVENT));
        }
        CloseHandle(th);
        *at::<HANDLE>(h, OFF_ACCEPT_THREAD) = null_mut();
    }
    let d = *at::<HANDLE>(h, OFF_DEVICE);
    if !d.is_null() {
        (dev().close)(d);
        *at::<HANDLE>(h, OFF_DEVICE) = null_mut();
    }
    for off in [OFF_WAKE_EVENT, OFF_READY_EVENT] {
        let e = *at::<HANDLE>(h, off);
        if !e.is_null() {
            CloseHandle(e);
            *at::<HANDLE>(h, off) = null_mut();
        }
    }
    if atom(h, OFF_REFS).fetch_sub(1, Ordering::SeqCst) == 1 {
        SetEvent(*at::<HANDLE>(h, OFF_IDLE_EVENT));
    }
    WaitForSingleObject(*at::<HANDLE>(h, OFF_IDLE_EVENT), u32::MAX);
    *at::<u64>(h, 0) = 0;
    *at::<u64>(h, 8) = 0;
    *at::<u64>(h, 0x18) = 0;
    core::ptr::write_bytes(h.add(0x70), 0, 0xbc);
    *at::<u32>(h, OFF_REFS) = 1;
    ResetEvent(*at::<HANDLE>(h, OFF_IDLE_EVENT));
    *at::<u64>(h, OFF_ACCEPT_STATE) = 0;
    core::ptr::write_bytes(h.add(OFF_PENDING_REQ), 0, 0x170);
    core::ptr::write_bytes(h.add(0x4a8), 0, 0x80);
    *at::<u64>(h, 0x528) = 0;
    *at::<u32>(h, 0x530) = 0;
    *at::<u32>(h, OFF_FLAGS) = 0;
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkCloseHandle(h: *mut u8) {
    if h.is_null() {
        return;
    }
    reset_handle(h);
    let e = *at::<HANDLE>(h, OFF_IDLE_EVENT);
    if !e.is_null() {
        CloseHandle(e);
    }
    DeleteCriticalSection(at(h, OFF_IO_CS));
    DeleteCriticalSection(at(h, OFF_LIST_CS));
    HeapFree(GetProcessHeap(), 0, h as *mut c_void);
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkHandleControl(h: *mut u8, code: u32, in_len: u32, input: *const u32, out_len: u32, returned: *mut u32, out: *mut u32) -> HRESULT {
    match code {
        1 => {
            if in_len < 4 || input.is_null() {
                return E_INVALIDARG;
            }
            if h.is_null() {
                return E_FAIL;
            }
            let mut fl = *at::<u32>(h, OFF_FLAGS);
            if fl & FLAG_CONNECTED != 0 {
                return E_INVALIDARG;
            }
            match *input {
                0 => fl &= !FLAG_TCP,
                1 => fl |= FLAG_TCP,
                _ => return E_INVALIDARG,
            }
            *at::<u32>(h, OFF_FLAGS) = fl;
            S_OK
        }
        2 => {
            if out_len < 4 || out.is_null() {
                return E_INVALIDARG;
            }
            *out = 0;
            if !h.is_null() && *at::<u32>(h, OFF_FLAGS) & FLAG_TCP != 0 {
                *out = 1;
            }
            *returned = 4;
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

unsafe fn put_addr(msg: *mut u8, off: usize, h: *mut u8, src: usize) {
    core::ptr::copy_nonoverlapping(h.add(src), msg.add(off), ADDR_SIZE);
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkShutdownHandle(h: *mut u8) -> HRESULT {
    let mut msg = [0u8; MSG_SIZE];
    let mut sent = 0u32;
    if h.is_null() || *at::<u32>(h, OFF_FLAGS) & FLAG_SHUTDOWN != 0 {
        return S_OK;
    }
    core::ptr::write_unaligned(msg.as_mut_ptr() as *mut u32, MSG_END);
    put_addr(msg.as_mut_ptr(), 0x10, h, OFF_LOCAL);
    put_addr(msg.as_mut_ptr(), 0x24, h, OFF_REMOTE);
    flags(h).fetch_or(FLAG_SHUTDOWN, Ordering::SeqCst);
    send_msg(h, null(), 0, &mut sent, msg.as_ptr(), null_mut(), 0xfa0)
}

unsafe fn abort_io(h: *mut u8) {
    let mode = *at::<u32>(h, OFF_FLAGS) & MODE_MASK;
    if mode == 0x2000_0000 || mode == MODE_DEVICE {
        let d = *at::<HANDLE>(h, OFF_DEVICE);
        if !d.is_null() {
            (dev().cancel_io)(d);
            (dev().close)(*at::<HANDLE>(h, OFF_DEVICE));
            *at::<HANDLE>(h, OFF_DEVICE) = null_mut();
        }
    } else if mode == MODE_TCP {
        let s = *at::<usize>(h, OFF_SOCKET);
        if s != INVALID_SOCKET {
            closesocket(s);
            *at::<usize>(h, OFF_SOCKET) = INVALID_SOCKET;
        }
    }
}

unsafe fn local_overlapped(ov: &mut OVERLAPPED) -> Result<HANDLE, HRESULT> {
    let ev = CreateEventW(null_mut(), 1, 0, null());
    if ev.is_null() {
        return Err(E_OUTOFMEMORY);
    }
    *ov = OVERLAPPED::default();
    ov.event = (ev as usize | 1) as HANDLE;
    Ok(ev)
}

unsafe fn device_send(h: *mut u8, bufs: *const WSABUF, n: u32, sent: *mut u32, header: *const u8, ov: *mut OVERLAPPED) -> HRESULT {
    if ov.is_null() {
        return E_INVALIDARG;
    }
    let mut arr: Vec<WSABUF> = Vec::with_capacity(n as usize + 1);
    arr.push(WSABUF { len: MSG_SIZE as u32, buf: header as *mut u8 });
    for i in 0..n as usize {
        arr.push(*bufs.add(i));
    }
    let zero = 0u32;
    let ok = (dev().ioctl)(*at::<HANDLE>(h, OFF_DEVICE), IOCTL_SEND, &zero as *const u32 as *const c_void, 4, arr.as_mut_ptr() as *mut c_void, (n + 1) << 4, sent, ov);
    if ok != 0 {
        return S_OK;
    }
    if GetLastError() == 0x3e5 {
        return E_PENDING;
    }
    hr_last_error()
}

pub unsafe fn send_msg(h: *mut u8, bufs: *const WSABUF, n: u32, sent: *mut u32, header: *const u8, ov: *mut OVERLAPPED, timeout: u32) -> HRESULT {
    let mut local = OVERLAPPED::default();
    let mut ev: HANDLE = null_mut();
    let use_ov = if ov.is_null() {
        match local_overlapped(&mut local) {
            Ok(e) => ev = e,
            Err(e) => return e,
        }
        &mut local as *mut OVERLAPPED
    } else {
        ov
    };
    let mode = *at::<u32>(h, OFF_FLAGS) & MODE_MASK;
    EnterCriticalSection(at(h, OFF_IO_CS));
    let mut hr = if mode == MODE_DEVICE {
        device_send(h, bufs, n, sent, header, use_ov)
    } else if mode == MODE_TCP {
        if WSASend(*at::<usize>(h, OFF_SOCKET), bufs, n, sent, 0, use_ov, null()) == -1 {
            if WSAGetLastError() == 0x3e5 {
                E_PENDING
            } else {
                wsa_hr(WSAGetLastError())
            }
        } else {
            S_OK
        }
    } else {
        LINK_E_BAD_MODE
    };
    LeaveCriticalSection(at(h, OFF_IO_CS));
    if hr == E_PENDING && ov.is_null() {
        let w = WaitForSingleObject(ev, timeout);
        if w == 0 {
            hr = if GetOverlappedResult(*at::<HANDLE>(h, OFF_DEVICE), use_ov, sent, 0) != 0 { S_OK } else { E_FAIL };
        } else if w == 0x102 {
            abort_io(h);
            WaitForSingleObject(ev, u32::MAX);
            hr = E_FAIL;
        }
    }
    if !ev.is_null() {
        CloseHandle(ev);
    }
    hr
}

pub unsafe fn recv_bytes(h: *mut u8, buf: *mut u8, len: u32, got: *mut u32, rflags: u32, ov: *mut OVERLAPPED, timeout: u32) -> HRESULT {
    let mut local = OVERLAPPED::default();
    let mut ev: HANDLE = null_mut();
    let use_ov = if ov.is_null() {
        match local_overlapped(&mut local) {
            Ok(e) => ev = e,
            Err(e) => return e,
        }
        &mut local as *mut OVERLAPPED
    } else {
        ov
    };
    let mode = *at::<u32>(h, OFF_FLAGS) & MODE_MASK;
    EnterCriticalSection(at(h, OFF_IO_CS));
    let mut wb = WSABUF { len, buf };
    let mut hr = if mode == MODE_DEVICE {
        let mut fw: u32 = if rflags & 1 != 0 { 2 } else { 0 };
        let ok = (dev().ioctl)(*at::<HANDLE>(h, OFF_DEVICE), IOCTL_RECV, &mut fw as *mut u32 as *const c_void, 4, &mut wb as *mut WSABUF as *mut c_void, 0x10, got, use_ov);
        if ok != 0 {
            S_OK
        } else if GetLastError() == 0x3e5 {
            E_PENDING
        } else {
            hr_last_error()
        }
    } else if mode == MODE_TCP {
        if rflags & 1 != 0 {
            E_INVALIDARG
        } else {
            let mut wf = 0u32;
            if WSARecv(*at::<usize>(h, OFF_SOCKET), &wb, 1, got, &mut wf, use_ov, null()) == -1 {
                if WSAGetLastError() == 0x3e5 {
                    E_PENDING
                } else {
                    wsa_hr(WSAGetLastError())
                }
            } else {
                S_OK
            }
        }
    } else {
        LINK_E_BAD_MODE
    };
    LeaveCriticalSection(at(h, OFF_IO_CS));
    if hr == E_PENDING && ov.is_null() {
        let w = WaitForSingleObject(ev, timeout);
        if w == 0 {
            hr = if GetOverlappedResult(*at::<HANDLE>(h, OFF_DEVICE), use_ov, got, 0) != 0 { S_OK } else { E_FAIL };
        } else if w == 0x102 {
            abort_io(h);
            WaitForSingleObject(ev, u32::MAX);
            hr = E_FAIL;
        }
    }
    if !ev.is_null() {
        CloseHandle(ev);
    }
    hr
}

unsafe fn bind(h: *mut u8, service: u32) -> HRESULT {
    let fl = *at::<u32>(h, OFF_FLAGS);
    if fl & (FLAG_LISTENING | FLAG_CONNECTED) != 0 {
        return LINK_E_ALREADY_ACTIVE;
    }
    if fl & FLAG_BOUND != 0 {
        return S_OK;
    }
    *at::<u32>(h, OFF_FLAGS) = fl | MODE_DEVICE;
    let d = (dev().open)();
    *at::<HANDLE>(h, OFF_DEVICE) = d;
    let hr;
    if d == INVALID_HANDLE_VALUE {
        *at::<HANDLE>(h, OFF_DEVICE) = null_mut();
        hr = LINK_E_NO_DEVICE;
    } else {
        let m = h.add(OFF_BIND_MSG);
        core::ptr::write_bytes(m, 0, MSG_SIZE);
        core::ptr::write_unaligned(m as *mut u32, MSG_BIND);
        *at::<u32>(h, 0xe4) = 1;
        *at::<u32>(h, 0xbc) = service;
        let mut sent = 0u32;
        if send_msg(h, null(), 0, &mut sent, m, null_mut(), 0xfa0) < 0 {
            hr = LINK_E_BIND_SEND;
        } else {
            let mut total = 0u32;
            loop {
                let mut n = 0u32;
                let r = recv_bytes(h, m.add(total as usize), MSG_SIZE as u32 - total, &mut n, 0, null_mut(), u32::MAX);
                if r < 0 {
                    break;
                }
                total += n;
                if total == MSG_SIZE as u32 {
                    core::ptr::copy_nonoverlapping(h.add(0xbc), h.add(OFF_LOCAL), ADDR_SIZE);
                    flags(h).fetch_or(FLAG_BOUND, Ordering::SeqCst);
                    return r;
                }
            }
            hr = LINK_E_BIND_RECV;
        }
    }
    let d = *at::<HANDLE>(h, OFF_DEVICE);
    if !d.is_null() {
        (dev().close)(d);
        *at::<HANDLE>(h, OFF_DEVICE) = null_mut();
    }
    hr
}

unsafe extern "system" fn accept_thread(p: *mut c_void) -> u32 {
    let h = p as *mut u8;
    let mut spins = 0u32;
    while flags(h).load(Ordering::SeqCst) & FLAG_STOP == 0 {
        let mut wait = u32::MAX;
        if atom(h, OFF_ACCEPT_STATE).load(Ordering::SeqCst) == 1 {
            spins += 1;
            if spins > 100 {
                atom(h, OFF_ACCEPT_STATE).swap(0, Ordering::SeqCst);
                spins = 0;
            } else {
                wait = 10;
            }
        }
        if atom(h, OFF_ACCEPT_STATE).load(Ordering::SeqCst) == 0 {
            ResetEvent(*at::<HANDLE>(h, OFF_READY_EVENT));
            atom(h, OFF_ACCEPT_SEQ).fetch_add(1, Ordering::SeqCst);
            let mut left = MSG_SIZE as u32;
            loop {
                let mut n = 0u32;
                let r = recv_bytes(h, h.add(OFF_PENDING_REQ), left, &mut n, 0, null_mut(), u32::MAX);
                if r < 0 || n == 0 {
                    break;
                }
                left -= n.min(left);
                if left == 0 {
                    break;
                }
            }
            atom(h, OFF_ACCEPT_STATE).swap(if left != 0 { 2 } else { 1 }, Ordering::SeqCst);
            SetEvent(*at::<HANDLE>(h, OFF_READY_EVENT));
        }
        if atom(h, OFF_ACCEPT_STATE).load(Ordering::SeqCst) == 2 {
            break;
        }
        if WaitForSingleObject(*at::<HANDLE>(h, OFF_WAKE_EVENT), wait) == 0 {
            spins = 0;
        }
        ResetEvent(*at::<HANDLE>(h, OFF_WAKE_EVENT));
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkListen(h: *mut u8, _reserved: *mut c_void, service: u16, ready: *mut HANDLE) -> HRESULT {
    if h.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    if !ready.is_null() {
        *ready = null_mut();
    }
    let hr = bind(h, service as u32);
    if hr < 0 {
        return hr;
    }
    let e1 = CreateEventW(null_mut(), 1, 0, null());
    *at::<HANDLE>(h, OFF_WAKE_EVENT) = e1;
    if e1.is_null() {
        return E_OUTOFMEMORY;
    }
    let e2 = CreateEventW(null_mut(), 1, 0, null());
    *at::<HANDLE>(h, OFF_READY_EVENT) = e2;
    if e2.is_null() {
        return E_OUTOFMEMORY;
    }
    flags(h).fetch_or(FLAG_LISTENING, Ordering::SeqCst);
    let th = CreateThread(null_mut(), 0, accept_thread, h as *mut c_void, 0, null_mut());
    *at::<HANDLE>(h, OFF_ACCEPT_THREAD) = th;
    if th.is_null() {
        flags(h).fetch_and(!FLAG_LISTENING, Ordering::SeqCst);
        return E_OUTOFMEMORY;
    }
    let name = wide("XBTP::Accept");
    SetThreadDescription(th, name.as_ptr());
    if !ready.is_null() {
        *ready = e2;
    }
    S_OK
}

pub unsafe fn parse_options(msg: *const u8, out: &mut [(*const u8, u16, u16)]) -> u32 {
    for e in out.iter_mut() {
        *e = (null(), 0, 0);
    }
    let mut count = 0u32;
    let mut off = 0u32;
    loop {
        let w = core::ptr::read_unaligned(msg.add(OPTS_OFF + off as usize) as *const u16);
        let len = (w & 0x1f) * 2;
        let ty = w >> 5;
        if ty == 0 {
            break;
        }
        off += 2;
        if (count as usize) < out.len() {
            let e = &mut out[count as usize];
            e.1 = ty;
            e.2 = len;
            if off + (len as u32) < OPTS_SIZE as u32 {
                e.0 = msg.add(OPTS_OFF + off as usize);
                count += 1;
            }
        }
        off += len as u32;
        if off >= 0x46 {
            break;
        }
    }
    count
}

pub unsafe fn sockaddr_to_link(addr: *const u8, len: u32, out: *mut u8) -> i32 {
    let fam = core::ptr::read_unaligned(addr as *const u16);
    if fam == 2 {
        if len < 0x10 {
            return 0xc000_000du32 as i32;
        }
        let port = u16::from_be(core::ptr::read_unaligned(addr.add(2) as *const u16));
        core::ptr::write_unaligned(out.add(0xc) as *mut u32, 0xffff_0000);
        core::ptr::write_unaligned(out.add(4) as *mut u64, 0);
        core::ptr::write_unaligned(out as *mut u32, port as u32);
        let b = [*addr.add(4), *addr.add(5), *addr.add(6), *addr.add(7)];
        core::ptr::copy_nonoverlapping(b.as_ptr(), out.add(0x10), 4);
        if b == [0x7f, 0, 0, 1] {
            let mut lo = [0u8; 20];
            lo[15] = 1;
            core::ptr::copy_nonoverlapping(lo.as_ptr(), out.add(4), 20);
        }
        0
    } else if fam == 0x17 && len >= 0x1c {
        let port = u16::from_be(core::ptr::read_unaligned(addr.add(2) as *const u16));
        core::ptr::write_unaligned(out as *mut u32, port as u32);
        core::ptr::copy_nonoverlapping(addr.add(8), out.add(4), 16);
        0
    } else {
        0xc000_000du32 as i32
    }
}

unsafe fn tcp_connect(h: *mut u8, addr: *const u8, len: u32, timeout: u32) -> HRESULT {
    let mut sa = [0u8; 0x80];
    if h.is_null() || *at::<usize>(h, OFF_SOCKET) != INVALID_SOCKET || len < 0x10 {
        return E_INVALIDARG;
    }
    let fam = core::ptr::read_unaligned(addr as *const u16);
    if fam == 2 {
        sa[0x12] = 0xff;
        sa[0x13] = 0xff;
        core::ptr::copy_nonoverlapping(addr.add(4), sa.as_mut_ptr().add(0x14), 4);
    } else if len >= 0x1c && fam == 0x17 {
        core::ptr::copy_nonoverlapping(addr, sa.as_mut_ptr(), 0x1c);
    } else {
        return E_INVALIDARG;
    }
    let port = u16::from_be(core::ptr::read_unaligned(addr.add(2) as *const u16));
    let mut tries = 0u32;
    let start = GetTickCount64();
    let mut hr: HRESULT = 0;
    loop {
        if tries >= 2 && GetTickCount64() - start >= timeout as u64 {
            break;
        }
        tries += 1;
        let s = *at::<usize>(h, OFF_SOCKET);
        if s != INVALID_SOCKET {
            closesocket(s);
            *at::<usize>(h, OFF_SOCKET) = INVALID_SOCKET;
        }
        let s = socket(0x17, 1, 6);
        *at::<usize>(h, OFF_SOCKET) = s;
        if s == INVALID_SOCKET {
            hr = hr_last_error();
            break;
        }
        let zero = 0u32;
        if setsockopt(s, 0x29, 0x1b, &zero as *const u32 as *const u8, 4) == -1 {
            hr = hr_last_error();
            break;
        }
        let one = 1u32;
        if setsockopt(s, 6, 1, &one as *const u32 as *const u8, 4) == -1 {
            hr = hr_last_error();
            break;
        }
        core::ptr::write_unaligned(sa.as_mut_ptr() as *mut u16, 0x17);
        core::ptr::write_unaligned(sa.as_mut_ptr().add(2) as *mut u16, ROUTER_PORT.to_be());
        crate::offline::localize_sockaddr(sa.as_mut_ptr(), 0x1c);
        if !crate::offline::sockaddr_is_loopback(sa.as_ptr(), 0x1c) {
            hr = crate::offline::WSAEHOSTUNREACH_HR;
            continue;
        }
        if connect(s, sa.as_ptr(), 0x1c) == -1 {
            hr = hr_last_error();
            continue;
        }
        let mut pre = [0u8; 0x80];
        pre[0] = 0xa8;
        pre[1] = 0xaf;
        if send(s, pre.as_ptr(), 8, 0) != 8 {
            hr = LINK_E_BIND_SEND;
            continue;
        }
        let mut msg = [0u8; 0x80];
        core::ptr::write_unaligned(msg.as_mut_ptr() as *mut u32, MSG_TCP_CONNECT);
        core::ptr::write_unaligned(msg.as_mut_ptr().add(0x24) as *mut u32, port as u32);
        if send(s, msg.as_ptr(), 0x80, 0) == -1 {
            hr = LINK_E_BIND_SEND;
            continue;
        }
        let mut got = 0i32;
        let mut bad = false;
        while got < 0x80 {
            let r = recv(s, msg.as_mut_ptr().add(got as usize), 0x80 - got, 0);
            if r == 0 || r == -1 {
                bad = true;
                break;
            }
            got += r;
        }
        if bad || got != 0x80 {
            hr = LINK_E_BIND_RECV;
            continue;
        }
        let mut opts = [(null::<u8>(), 0u16, 0u16); 0x20];
        let n = parse_options(msg.as_ptr(), &mut opts);
        match opts[..n as usize].iter().find(|o| o.1 == 3 && o.2 == 0x10) {
            None => return S_OK,
            Some(o) => {
                core::ptr::copy_nonoverlapping(o.0, sa.as_mut_ptr().add(8), 16);
                hr = 0;
                continue;
            }
        }
    }
    if hr < 0 {
        let s = *at::<usize>(h, OFF_SOCKET);
        if s != INVALID_SOCKET {
            closesocket(s);
            *at::<usize>(h, OFF_SOCKET) = INVALID_SOCKET;
        }
    }
    hr
}

unsafe fn connect_fail(h: *mut u8, hr: HRESULT) -> HRESULT {
    core::ptr::write_bytes(h.add(OFF_TARGET), 0, ADDR_SIZE);
    core::ptr::write_bytes(h.add(OFF_REMOTE), 0, ADDR_SIZE);
    hr
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkConnect(h: *mut u8, addr: *const u8, len: u32, timeout: u32) -> HRESULT {
    if h.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    let fl = *at::<u32>(h, OFF_FLAGS);
    core::ptr::write_unaligned(at::<u64>(h, OFF_PATH_TCP), 0);
    if fl & FLAG_TCP != 0 {
        *at::<u32>(h, OFF_FLAGS) = (fl & 0x0fff_ffff) | MODE_TCP;
        return tcp_connect(h, addr, len, timeout);
    }
    let start = GetTickCount64();
    let mut first = true;
    let mut hr: HRESULT = 0;
    loop {
        if !first && GetTickCount64() - start >= timeout as u64 {
            if hr < 0 {
                return if timeout != 0 { E_PENDING } else { hr };
            }
            flags(h).fetch_or(FLAG_CONNECTED, Ordering::SeqCst);
            return hr;
        }
        if sockaddr_to_link(addr, len, h.add(OFF_TARGET)) < 0 {
            return LINK_E_CONNECT_ADDRESS;
        }
        hr = bind(h, 0);
        if hr < 0 {
            return connect_fail(h, hr);
        }
        let mut msg = [0u8; MSG_SIZE];
        let m = msg.as_mut_ptr();
        core::ptr::write_unaligned(m as *mut u32, MSG_CONNECT);
        put_addr(m, 0x10, h, OFF_LOCAL);
        put_addr(m, 0x24, h, OFF_TARGET);
        if timeout != u32::MAX {
            first = false;
        }
        let mut sent = 0u32;
        hr = send_msg(h, null(), 0, &mut sent, m, null_mut(), 0xfa0);
        if hr < 0 {
            return connect_fail(h, hr);
        }
        let mut total = 0u32;
        let code: u32;
        loop {
            let mut n = 0u32;
            hr = recv_bytes(h, m.add(total as usize), MSG_SIZE as u32 - total, &mut n, 0, null_mut(), 1000);
            if hr == E_OUTOFMEMORY {
                return connect_fail(h, hr);
            }
            if hr < 0 || n == 0 {
                core::ptr::write_unaligned(m as *mut u32, 0x100);
                code = 0x100;
                break;
            }
            total += n;
            if total == MSG_SIZE as u32 {
                code = core::ptr::read_unaligned(m as *const u32);
                break;
            }
        }
        if code & 0xff00 == 0x100 {
            reset_handle(h);
            let el = GetTickCount64() - start;
            if timeout == 0 {
                return connect_fail(h, LINK_E_CONNECT_REFUSED);
            }
            if el > timeout as u64 {
                return connect_fail(h, E_PENDING);
            }
            Sleep((timeout as u64 - el).min(1000) as u32);
            continue;
        }
        let mut opts = [(null::<u8>(), 0u16, 0u16); 0x10];
        let n = parse_options(m, &mut opts);
        if let Some(o) = opts[..n as usize].iter().find(|o| o.1 == 0xc) {
            if o.2 != 0 {
                *at::<u32>(h, OFF_PATH_TCP) = *o.0 as u32;
                if o.2 > 1 {
                    *at::<u32>(h, OFF_PATH_XTD) = *o.0.add(1) as u32;
                }
            }
        }
        let c = msg[0];
        if c != 3 && c != 6 {
            return connect_fail(h, LINK_E_CONNECT_RESPONSE);
        }
        core::ptr::copy_nonoverlapping(m.add(0x10), h.add(OFF_REMOTE), ADDR_SIZE);
        if c != 6 {
            flags(h).fetch_or(FLAG_CONNECTED, Ordering::SeqCst);
        }
        return hr;
    }
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkConnectByName(h: *mut u8, name: *const u16, port: u16, timeout: u32) -> HRESULT {
    let mut hints: ADDRINFOW = core::mem::zeroed();
    hints.ai_socktype = 1;
    hints.ai_protocol = 6;
    let mut res: *mut ADDRINFOW = null_mut();
    let mut sa = [0u8; 0x80];
    let hr;
    if matches!(crate::offline::classify_wide(name), crate::offline::HostClass::RemoteName) {
        hr = crate::offline::WSAHOST_NOT_FOUND_HR;
    } else if GetAddrInfoW(name, null(), &hints, &mut res) != 0 {
        hr = wsa_hr(WSAGetLastError());
    } else {
        let mut pick: *mut ADDRINFOW = null_mut();
        let mut a = res;
        while !a.is_null() {
            if (*a).ai_family == 2 {
                pick = a;
            } else if pick.is_null() && (*a).ai_family == 0x17 {
                pick = a;
            }
            a = (*a).ai_next;
        }
        if pick.is_null() || (*pick).ai_addrlen > 0x80 {
            hr = 0x8007_2af9u32 as i32;
        } else {
            let l = (*pick).ai_addrlen;
            if l != 0 && !(*pick).ai_addr.is_null() {
                core::ptr::copy_nonoverlapping((*pick).ai_addr, sa.as_mut_ptr(), l);
            }
            if (*pick).ai_family == 2 || (*pick).ai_family == 0x17 {
                core::ptr::write_unaligned(sa.as_mut_ptr().add(2) as *mut u16, port.to_be());
            }
            hr = XbtpLinkConnect(h, sa.as_ptr(), l as u32, timeout);
        }
    }
    if !res.is_null() {
        FreeAddrInfoW(res);
    }
    hr
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkSetCompletionPort(h: *mut u8, port: HANDLE, key: usize, threads: u32) -> HRESULT {
    if h.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    let device = *at::<HANDLE>(h, OFF_DEVICE);
    if crate::link_dev::is_in_process(device) {
        return crate::link_dev::set_completion_port(device, port, key);
    }
    if CreateIoCompletionPort(device, port, key, threads) == port {
        return S_OK;
    }
    hr_last_error()
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkAccept(listener: *mut u8, out: *mut *mut u8, timeout: u32) -> HRESULT {
    let mut req = [0u8; MSG_SIZE];
    if listener.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    *out = null_mut();
    let nh = XbtpLinkCreateHandle();
    if nh.is_null() {
        return E_OUTOFMEMORY;
    }
    let mut hr = bind(nh, 0);
    if hr < 0 {
        XbtpLinkCloseHandle(nh);
        return hr;
    }
    let start = GetTickCount64();
    let mut first = true;
    let claimed = loop {
        let el = GetTickCount64() - start;
        let mut wait = timeout;
        if first {
            first = false;
        } else if timeout != u32::MAX {
            if el > timeout as u64 {
                XbtpLinkCloseHandle(nh);
                return E_PENDING;
            }
            wait = timeout - el as u32;
        }
        let w = WaitForSingleObject(*at::<HANDLE>(listener, OFF_READY_EVENT), wait);
        if w == 0x102 {
            XbtpLinkCloseHandle(nh);
            return E_PENDING;
        }
        if w != 0 {
            XbtpLinkCloseHandle(nh);
            return LINK_E_ACCEPT_WAIT;
        }
        core::ptr::copy_nonoverlapping(listener.add(OFF_PENDING_REQ), req.as_mut_ptr(), MSG_SIZE);
        let seq = atom(listener, OFF_ACCEPT_SEQ).load(Ordering::SeqCst);
        SetEvent(*at::<HANDLE>(listener, OFF_WAKE_EVENT));
        if seq == atom(listener, OFF_ACCEPT_SEQ).load(Ordering::SeqCst) {
            let mut got = false;
            match atom(listener, OFF_ACCEPT_STATE).compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => got = true,
                Err(2) => hr = LINK_E_ACCEPT_WAIT,
                Err(_) => {}
            }
            SetEvent(*at::<HANDLE>(listener, OFF_WAKE_EVENT));
            if got {
                break hr;
            }
        }
        if hr < 0 {
            XbtpLinkCloseHandle(nh);
            return hr;
        }
    };
    hr = claimed;
    if hr < 0 {
        XbtpLinkCloseHandle(nh);
        return hr;
    }
    if req[0] != 2 {
        XbtpLinkCloseHandle(nh);
        return LINK_E_ACCEPT_REQUEST;
    }
    core::ptr::copy_nonoverlapping(req.as_ptr().add(0x10), nh.add(OFF_REMOTE), ADDR_SIZE);
    let mut msg = [0u8; MSG_SIZE];
    let m = msg.as_mut_ptr();
    core::ptr::write_unaligned(m as *mut u32, MSG_ACCEPT);
    put_addr(m, 0x10, nh, OFF_LOCAL);
    put_addr(m, 0x24, nh, OFF_REMOTE);
    let mut sent = 0u32;
    hr = send_msg(nh, null(), 0, &mut sent, m, null_mut(), 0xfa0);
    if hr < 0 {
        XbtpLinkCloseHandle(nh);
        return LINK_E_ACCEPT_RESPONSE;
    }
    *out = nh;
    hr
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkRecv(h: *mut u8, buf: *mut u8, len: u32, got: *mut u32, rflags: u32, ov: *mut OVERLAPPED) -> HRESULT {
    if h.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    recv_bytes(h, buf, len, got, rflags, ov, u32::MAX)
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkSend(h: *mut u8, bufs: *const WSABUF, n: u32, sent: *mut u32, _flags: u32, ov: *mut OVERLAPPED) -> HRESULT {
    let mut msg = [0u8; MSG_SIZE];
    if h.is_null() {
        return LINK_E_NULL_HANDLE;
    }
    let m = msg.as_mut_ptr();
    core::ptr::write_unaligned(m as *mut u32, MSG_DATA);
    put_addr(m, 0x10, h, OFF_LOCAL);
    put_addr(m, 0x24, h, OFF_REMOTE);
    let mut total = 0u32;
    for i in 0..n as usize {
        if total >= 0x0100_0000 || (*bufs.add(i)).len > 0x1000_0000 {
            return LINK_E_TOO_LARGE;
        }
        total += (*bufs.add(i)).len;
    }
    core::ptr::write_unaligned(m.add(4) as *mut u32, total);
    send_msg(h, bufs, n, sent, m, ov, u32::MAX)
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkGetOverlappedResult(_h: *mut u8, ov: *const OVERLAPPED, n: *mut u32) -> HRESULT {
    let st = (*ov).internal as u32;
    *n = 0;
    if st == 0x103 {
        return E_PENDING;
    }
    if st == 0 {
        *n = (*ov).internal_high as u32;
        return S_OK;
    }
    if st & 0xffff_0000 != 0xe078_0000 {
        return 0x8c11_051cu32 as i32;
    }
    (match st & 0xff {
        1 => 0x8c11_051au32,
        2 => 0x8c11_0514,
        3 => 0x8007_000e,
        4 => 0x8c11_0516,
        5 => 0x8c11_0517,
        6 => 0x8c11_0518,
        7 => 0x8c11_0519,
        8 => 0x8c11_051b,
        _ => 0x8c11_051c,
    }) as i32
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkGetServiceLocation(buf: *mut u16, len: *mut u32) -> HRESULT {
    if *len > 0 {
        *buf = 0;
        *len = 0;
    }
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkGetServiceVersion() -> HRESULT {
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkGetServiceOptions(out: *mut u32) -> HRESULT {
    *out = 0;
    S_OK
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkSetServiceOptions() -> HRESULT {
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkQueryStatistics() -> HRESULT {
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkQueryPacketLog() -> HRESULT {
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkQueryHandleInformation(h: *mut u8, class: u32, size: u32, out: *mut u8) -> HRESULT {
    match class {
        1 => {
            if size != 0x10 {
                return E_INVALIDARG;
            }
            core::ptr::copy_nonoverlapping(h.add(0x88), out, 0x10);
            S_OK
        }
        2 => {
            if size != 8 {
                return E_INVALIDARG;
            }
            core::ptr::write_unaligned(out as *mut u32, *at::<u32>(h, OFF_PATH_TCP));
            core::ptr::write_unaligned(out.add(4) as *mut u32, *at::<u32>(h, OFF_PATH_XTD));
            S_OK
        }
        _ => S_OK,
    }
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkAllocReservedMemory(addr: *mut *mut c_void, size: *mut u64) -> HRESULT {
    *addr = null_mut();
    *size = 0;
    if RESERVE_STATE.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return HR_SHARING_VIOLATION;
    }
    let mut d = RESERVE_DEVICE.load(Ordering::SeqCst) as HANDLE;
    let hr;
    if d == INVALID_HANDLE_VALUE {
        d = (dev().open)();
        RESERVE_DEVICE.store(d as usize, Ordering::SeqCst);
        if d == INVALID_HANDLE_VALUE {
            RESERVE_STATE.swap(0, Ordering::SeqCst);
            return HR_NOT_SUPPORTED;
        }
    }
    let out = core::ptr::addr_of_mut!(RESERVE_OUT);
    let ov = core::ptr::addr_of_mut!(RESERVE_OV);
    (*out)[2] = 0;
    *ov = OVERLAPPED::default();
    (*out)[0] = 0;
    (*out)[1] = 0;
    let mut ret = 0u32;
    let ok = (dev().ioctl)(d, IOCTL_RESERVE, null(), 0, out as *mut c_void, 0x18, &mut ret, ov);
    if ok == 0 && GetLastError() == 0x3e5 {
        if (*out)[1] != 0 && (*out)[2] != 0 {
            RESERVE_STATE.swap(2, Ordering::SeqCst);
            *addr = (*out)[1] as *mut c_void;
            *size = (*out)[2];
            return S_OK;
        }
        hr = E_FAIL;
    } else {
        hr = hr_last_error();
        if hr == 0 {
            return 0;
        }
    }
    RESERVE_STATE.swap(0, Ordering::SeqCst);
    hr
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkFreeReservedMemory() -> u32 {
    if let Err(v) = RESERVE_STATE.compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst) {
        return v;
    }
    let ov = core::ptr::addr_of_mut!(RESERVE_OV);
    (dev().cancel_io_ex)(RESERVE_DEVICE.load(Ordering::SeqCst) as HANDLE, ov);
    while core::ptr::read_volatile(core::ptr::addr_of!((*ov).internal)) as u32 == 0x103 {
        Sleep(10);
    }
    RESERVE_STATE.swap(0, Ordering::SeqCst)
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkStartRouting() -> HRESULT {
    S_OK
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkStopRouting() {}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkSetupRoutingLog() -> HRESULT {
    E_NOTIMPL
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkSetRoutingRedirect(addr: *const u8, len: u32) -> HRESULT {
    sockaddr_to_link(addr, len, core::ptr::addr_of_mut!(REDIRECT) as *mut u8);
    S_OK
}

pub fn routing_redirect() -> [u8; 0x18] {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(REDIRECT)) }
}

unsafe fn wait_ov(s: SOCKET, ov: *mut OVERLAPPED, n: *mut u32) -> bool {
    GetOverlappedResult(s as HANDLE, ov, n, 1) != 0
}

unsafe fn routing_session(s: SOCKET, ev: HANDLE) {
    let mut buf = [0u8; 0x80];
    let mut got = 0u32;
    let mut ov: OVERLAPPED;
    let mut n = 0u32;
    while got < 0x80 {
        ResetEvent(ev);
        let wb = WSABUF { len: 0x80 - got, buf: buf.as_mut_ptr().add(got as usize) };
        ov = OVERLAPPED { event: ev, ..OVERLAPPED::default() };
        let mut fl = 0u32;
        if WSARecv(s, &wb, 1, null_mut(), &mut fl, &mut ov, null()) == -1 && WSAGetLastError() != 0x3e5 {
            return;
        }
        if !wait_ov(s, &mut ov, &mut n) {
            return;
        }
        if n.wrapping_sub(1) > 0x7f {
            return;
        }
        got += n;
    }
    core::ptr::write_bytes(buf.as_mut_ptr().add(0x10), 0, 0x70);
    core::ptr::write_unaligned(buf.as_mut_ptr() as *mut u32, MSG_GATEWAY);
    core::ptr::write_unaligned(buf.as_mut_ptr().add(4) as *mut u32, 0);
    core::ptr::write_unaligned(buf.as_mut_ptr().add(8) as *mut u64, 0);
    core::ptr::write_unaligned(buf.as_mut_ptr().add(OPTS_OFF) as *mut u16, 0x68);
    let red = routing_redirect();
    core::ptr::copy_nonoverlapping(red.as_ptr().add(4), buf.as_mut_ptr().add(OPTS_OFF + 2), 16);
    ResetEvent(ev);
    let wb = WSABUF { len: 0x80, buf: buf.as_mut_ptr() };
    ov = OVERLAPPED { event: ev, ..OVERLAPPED::default() };
    WSASend(s, &wb, 1, &mut n, 0, &mut ov, null());
    if !wait_ov(s, &mut ov, &mut n) {
        return;
    }
    ResetEvent(ev);
    ov = OVERLAPPED { event: ev, ..OVERLAPPED::default() };
    let mut fl = 0u32;
    if WSARecv(s, &wb, 1, null_mut(), &mut fl, &mut ov, null()) == -1 && WSAGetLastError() != 0x3e5 {
        return;
    }
    wait_ov(s, &mut ov, &mut n);
}

unsafe extern "system" fn routing_worker(p: *mut c_void) -> u32 {
    let s = p as SOCKET;
    let ev = CreateEventW(null_mut(), 1, 0, null());
    if !ev.is_null() {
        routing_session(s, ev);
    }
    shutdown(s, 2);
    closesocket(s);
    if !ev.is_null() {
        CloseHandle(ev);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn XbtpLinkProcessRoutingSocket(s: SOCKET) -> HRESULT {
    QueueUserWorkItem(routing_worker, s as *mut c_void, 0);
    S_OK
}
