use crate::link::{DeviceOps, WSABUF, IOCTL_RECV, IOCTL_SEND, MSG_SIZE, REAL_DEVICE};
use crate::sys::*;
use core::ffi::c_void;
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

pub const TAG: usize = 0x4c44_0000_0000;
pub const TAG_MASK: usize = 0xffff_0000_0000;
pub const REFUSED: u32 = 0xfd00_0102;
pub const STATUS_CANCELLED: usize = 0xc000_0120;
pub const STATUS_PENDING: usize = 0x103;
pub const ERROR_IO_PENDING: u32 = 0x3e5;
pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_INVALID_FUNCTION: u32 = 1;
pub const ERROR_INVALID_PARAMETER: u32 = 0x57;
pub const MSG_PEEK: u32 = 2;
pub const AUTO_PORT_BASE: u32 = 0x9000;

struct Pending {
    buf: usize,
    len: u32,
    peek: bool,
    ov: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryObject(handle: HANDLE, class: u32, info: *mut c_void, size: u32, returned: *mut u32) -> i32;
    fn NtSetIoCompletion(port: HANDLE, key: usize, overlapped: usize, status: i32, bytes: usize) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn DuplicateHandle(from: HANDLE, handle: HANDLE, to: HANDLE, duplicate: *mut HANDLE, access: u32, inherit: i32, options: u32) -> i32;
}

struct CompletionPort {
    handle: usize,
    key: usize,
}

impl Drop for CompletionPort {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle as HANDLE) };
    }
}

pub(crate) unsafe fn set_completion_port(device: HANDLE, port: HANDLE, key: usize) -> HRESULT {
    let mut r = router();
    let Some(ep) = r.eps.iter_mut().find(|ep| ep.id == device as usize) else {
        return hr_from_win32(ERROR_INVALID_HANDLE);
    };
    if ep.completion.is_some() || port.is_null() {
        return E_INVALIDARG;
    }
    let process = GetCurrentProcess();
    let mut duplicate = core::ptr::null_mut();
    if DuplicateHandle(process, port, process, &mut duplicate, 0, 0, 2) == 0 {
        return hr_last_error();
    }
    let owned = CompletionPort { handle: duplicate as usize, key };
    let mut basic = [0u32; 14];
    let status = NtQueryObject(duplicate, 0, basic.as_mut_ptr().cast(), core::mem::size_of_val(&basic) as u32, core::ptr::null_mut());
    if status < 0 {
        return hr_from_win32(RtlNtStatusToDosError(status));
    }
    let mut object_type = [0usize; 64];
    let status = NtQueryObject(duplicate, 2, object_type.as_mut_ptr().cast(), core::mem::size_of_val(&object_type) as u32, core::ptr::null_mut());
    if status < 0 {
        return hr_from_win32(RtlNtStatusToDosError(status));
    }
    let name = core::slice::from_raw_parts(object_type[1] as *const u16, (object_type[0] as u16 / 2) as usize);
    if !name.iter().copied().eq("IoCompletion".encode_utf16()) {
        return hr_from_win32(ERROR_INVALID_HANDLE);
    }
    if basic[1] & 2 == 0 {
        return hr_from_win32(5);
    }
    ep.completion = Some(owned);
    S_OK
}

struct Endpoint {
    completion: Option<CompletionPort>,
    id: usize,
    port: Option<u32>,
    inbox: VecDeque<u8>,
    eof: bool,
    pending: VecDeque<Pending>,
}

struct Router {
    eps: Vec<Endpoint>,
    next_id: usize,
    next_port: u32,
}

static ROUTER: Mutex<Router> = Mutex::new(Router { eps: Vec::new(), next_id: 1, next_port: AUTO_PORT_BASE });

fn router() -> MutexGuard<'static, Router> {
    ROUTER.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn is_in_process(h: HANDLE) -> bool {
    h as usize & TAG_MASK == TAG
}

pub fn endpoint_count() -> usize {
    router().eps.len()
}

pub fn listening(port: u32) -> bool {
    router().eps.iter().any(|e| e.port == Some(port))
}

pub fn pending_recvs(port: u32) -> usize {
    router().eps.iter().filter(|e| e.port == Some(port)).map(|e| e.pending.len()).sum()
}

unsafe fn complete(port: Option<&CompletionPort>, ov: *mut OVERLAPPED, n: u32, status: usize) {
    let event = (*ov).event as usize;
    (*ov).internal_high = n as usize;
    (*ov).internal = status;
    let ev = (event & !1) as HANDLE;
    if !ev.is_null() {
        SetEvent(ev);
    }
    if event & 1 == 0 {
        if let Some(port) = port {
            NtSetIoCompletion(port.handle as HANDLE, port.key, ov as usize, status as i32, n as usize);
        }
    }
}

unsafe fn take(ep: &mut Endpoint, buf: *mut u8, len: u32, peek: bool) -> u32 {
    let n = (len as usize).min(ep.inbox.len());
    for (k, b) in ep.inbox.iter().take(n).enumerate() {
        *buf.add(k) = *b;
    }
    if !peek {
        ep.inbox.drain(..n);
    }
    n as u32
}

unsafe fn pump(ep: &mut Endpoint) {
    while !ep.inbox.is_empty() || ep.eof {
        let Some(p) = ep.pending.pop_front() else { break };
        let n = take(ep, p.buf as *mut u8, p.len, p.peek);
        complete(ep.completion.as_ref(), p.ov as *mut OVERLAPPED, n, 0);
        if n == 0 {
            continue;
        }
    }
}

unsafe fn deliver(ep: &mut Endpoint, data: &[u8]) {
    if ep.eof {
        return;
    }
    ep.inbox.extend(data.iter().copied());
    pump(ep);
}

unsafe fn finish(ep: &mut Endpoint) {
    ep.eof = true;
    pump(ep);
}

fn port_at(h: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([h[off], h[off + 1], h[off + 2], h[off + 3]])
}

unsafe fn dev_open() -> HANDLE {
    let real = (REAL_DEVICE.open)();
    if real != INVALID_HANDLE_VALUE {
        return real;
    }
    let mut r = router();
    let id = TAG | r.next_id;
    r.next_id += 1;
    r.eps.push(Endpoint { id, completion: None, port: None, inbox: VecDeque::new(), eof: false, pending: VecDeque::new() });
    id as HANDLE
}

unsafe fn sync_done(ep: &Endpoint, r: *mut u32, ov: *mut OVERLAPPED, n: u32) -> i32 {
    if !r.is_null() {
        *r = n;
    }
    if !ov.is_null() {
        complete(ep.completion.as_ref(), ov, n, 0);
    }
    1
}

unsafe fn recv(r: &mut Router, me: usize, i: *const c_void, il: u32, o: *mut c_void, ol: u32, ret: *mut u32, ov: *mut OVERLAPPED) -> i32 {
    if il < 4 || ol < 0x10 || o.is_null() || i.is_null() {
        SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let peek = *(i as *const u32) & MSG_PEEK != 0;
    let wb = *(o as *const WSABUF);
    let ep = &mut r.eps[me];
    if !ep.inbox.is_empty() || ep.eof {
        let n = take(ep, wb.buf, wb.len, peek);
        return sync_done(ep, ret, ov, n);
    }
    if ov.is_null() {
        SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    (*ov).internal = STATUS_PENDING;
    (*ov).internal_high = 0;
    ep.pending.push_back(Pending { buf: wb.buf as usize, len: wb.len, peek, ov: ov as usize });
    SetLastError(ERROR_IO_PENDING);
    0
}

unsafe fn send(r: &mut Router, me: usize, i: *const c_void, il: u32, o: *mut c_void, ol: u32, ret: *mut u32, ov: *mut OVERLAPPED) -> i32 {
    let n = (ol >> 4) as usize;
    if il < 4 || i.is_null() || o.is_null() || n == 0 {
        SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let arr = core::slice::from_raw_parts(o as *const WSABUF, n);
    if (arr[0].len as usize) < MSG_SIZE || arr[0].buf.is_null() {
        SetLastError(ERROR_INVALID_PARAMETER);
        return 0;
    }
    let hdr = core::slice::from_raw_parts(arr[0].buf, MSG_SIZE).to_vec();
    let mut payload = Vec::new();
    for b in &arr[1..] {
        if b.len != 0 {
            payload.extend_from_slice(core::slice::from_raw_parts(b.buf, b.len as usize));
        }
    }
    let dst = port_at(&hdr, 0x24);
    let peer = r.eps.iter().position(|e| e.port == Some(dst));
    match hdr[0] {
        0x80 => {
            let svc = port_at(&hdr, 0x10);
            let port = if svc != 0 {
                svc
            } else {
                loop {
                    r.next_port = r.next_port.wrapping_add(1).max(AUTO_PORT_BASE);
                    let p = r.next_port;
                    if !r.eps.iter().any(|e| e.port == Some(p)) {
                        break p;
                    }
                }
            };
            r.eps[me].port = Some(port);
            let mut rep = hdr.clone();
            rep[0x10..0x14].copy_from_slice(&port.to_le_bytes());
            rep[0x14..0x24].fill(0);
            rep[0x23] = 1;
            deliver(&mut r.eps[me], &rep);
        }
        2 => match peer {
            Some(p) if p != me => deliver(&mut r.eps[p], &hdr),
            _ => {
                let mut rep = hdr.clone();
                rep[0..4].copy_from_slice(&REFUSED.to_le_bytes());
                deliver(&mut r.eps[me], &rep);
            }
        },
        3 => {
            if let Some(p) = peer {
                deliver(&mut r.eps[p], &hdr);
            }
        }
        1 => {
            if let Some(p) = peer {
                deliver(&mut r.eps[p], &payload);
            }
        }
        5 => {
            finish(&mut r.eps[me]);
            if let Some(p) = peer {
                if p != me {
                    finish(&mut r.eps[p]);
                }
            }
        }
        _ => {
            SetLastError(ERROR_INVALID_PARAMETER);
            return 0;
        }
    }
    sync_done(&r.eps[me], ret, ov, (MSG_SIZE + payload.len()) as u32)
}

unsafe fn dev_ioctl(h: HANDLE, code: u32, i: *const c_void, il: u32, o: *mut c_void, ol: u32, ret: *mut u32, ov: *mut OVERLAPPED) -> i32 {
    if !is_in_process(h) {
        return (REAL_DEVICE.ioctl)(h, code, i, il, o, ol, ret, ov);
    }
    let mut r = router();
    let Some(me) = r.eps.iter().position(|e| e.id == h as usize) else {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    };
    match code {
        IOCTL_RECV => recv(&mut r, me, i, il, o, ol, ret, ov),
        IOCTL_SEND => send(&mut r, me, i, il, o, ol, ret, ov),
        _ => {
            SetLastError(ERROR_INVALID_FUNCTION);
            0
        }
    }
}

unsafe fn cancel_where(h: HANDLE, pick: impl Fn(&Pending) -> bool) -> i32 {
    let mut r = router();
    let Some(ep) = r.eps.iter_mut().find(|e| e.id == h as usize) else {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    };
    let all: Vec<Pending> = ep.pending.drain(..).collect();
    for p in all {
        if pick(&p) {
            complete(ep.completion.as_ref(), p.ov as *mut OVERLAPPED, 0, STATUS_CANCELLED);
        } else {
            ep.pending.push_back(p);
        }
    }
    1
}

unsafe fn dev_cancel_io(h: HANDLE) -> i32 {
    if !is_in_process(h) {
        return (REAL_DEVICE.cancel_io)(h);
    }
    cancel_where(h, |_| true)
}

unsafe fn dev_cancel_io_ex(h: HANDLE, ov: *mut OVERLAPPED) -> i32 {
    if !is_in_process(h) {
        return (REAL_DEVICE.cancel_io_ex)(h, ov);
    }
    cancel_where(h, |p| ov.is_null() || p.ov == ov as usize)
}

unsafe fn dev_close(h: HANDLE) -> i32 {
    if !is_in_process(h) {
        return (REAL_DEVICE.close)(h);
    }
    let mut r = router();
    let Some(k) = r.eps.iter().position(|e| e.id == h as usize) else {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    };
    let mut ep = r.eps.remove(k);
    for p in ep.pending.drain(..) {
        complete(ep.completion.as_ref(), p.ov as *mut OVERLAPPED, 0, STATUS_CANCELLED);
    }
    1
}

pub static DEVICE: DeviceOps = DeviceOps {
    open: dev_open,
    ioctl: dev_ioctl,
    cancel_io: dev_cancel_io,
    cancel_io_ex: dev_cancel_io_ex,
    close: dev_close,
};
