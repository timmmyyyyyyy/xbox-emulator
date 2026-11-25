use crate::encoding::XBTP_MESSAGE_BODY;
use crate::msvc::SharedPtr;
use crate::session::*;
use crate::sys::*;
use crate::xbtp2_ext::{com_add_ref, com_qi, com_release, message_set_received, new_message, IID_XBTP_MESSAGE_IMPL};
use crate::xbtp2_json::IID_IUNKNOWN;
use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

pub const IID_XBTP_SESSION: GUID = GUID::new(0x38d55a2c, 0x22cc, 0x4c2a, [0x8e, 0x7e, 0xab, 0xf8, 0x4e, 0x06, 0x25, 0xe7]);
pub const IID_XBTP_CLIENT_SESSION: GUID = GUID::new(0xcf84f553, 0xcd9b, 0x4608, [0x96, 0x40, 0x28, 0x71, 0x4e, 0x5e, 0xd4, 0x40]);
pub const IID_IXBTP_MESSAGE: GUID = IID_XBTP_MESSAGE_IMPL;
pub const E_NOT_SET: HRESULT = 0x8007_0490u32 as i32;

pub type SendCompletion = crate::conn::SendCallback;

#[repr(C)]
pub struct SessionImplVtbl {
    pub query_interface: unsafe extern "system" fn(*mut SessionImpl, *const GUID, *mut *mut c_void) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(*mut SessionImpl) -> u32,
    pub release: unsafe extern "system" fn(*mut SessionImpl) -> u32,
    pub send_begin_message: unsafe extern "system" fn(*mut SessionImpl, u32, *mut c_void, *mut i32) -> HRESULT,
    pub send_more_message: unsafe extern "system" fn(*mut SessionImpl, i32, *mut c_void) -> HRESULT,
    pub send_more_message_async: unsafe extern "system" fn(*mut SessionImpl, i32, *mut c_void, Option<SendCompletion>, *mut c_void) -> HRESULT,
    pub receive_more_message: unsafe extern "system" fn(*mut SessionImpl, u32, i32, *mut *mut c_void) -> HRESULT,
    pub receive_end_message: unsafe extern "system" fn(*mut SessionImpl, u32, i32, *mut i32, *mut *mut c_void) -> HRESULT,
    pub receive_message: unsafe extern "system" fn(*mut SessionImpl, u32, i32, *mut i16, *mut i32, *mut *mut c_void) -> HRESULT,
    pub get_message_received_event: unsafe extern "system" fn(*mut SessionImpl, *mut HANDLE) -> HRESULT,
    pub get_property: unsafe extern "system" fn(*mut SessionImpl, *const u16, *mut BSTR) -> HRESULT,
}

#[repr(C)]
pub struct SessionImpl {
    pub vtbl: *const SessionImplVtbl,
    pub refs: AtomicU32,
    pub lock: Mutex<()>,
    pub session: SharedPtr<XbtpSession>,
}

pub static SESSION_IMPL_VTBL: SessionImplVtbl = SessionImplVtbl {
    query_interface: si_qi,
    add_ref: si_add_ref,
    release: si_release,
    send_begin_message: si_send_begin_message,
    send_more_message: si_send_more_message,
    send_more_message_async: si_send_more_message_async,
    receive_more_message: si_receive_more_message,
    receive_end_message: si_receive_end_message,
    receive_message: si_receive_message,
    get_message_received_event: si_get_message_received_event,
    get_property: si_get_property,
};

pub unsafe fn new_session_impl(session: SharedPtr<XbtpSession>) -> *mut SessionImpl {
    Box::into_raw(Box::new(SessionImpl { vtbl: &SESSION_IMPL_VTBL, refs: AtomicU32::new(1), lock: Mutex::new(()), session }))
}

pub unsafe fn wrap_session(session: SharedPtr<XbtpSession>, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    let s = new_session_impl(session);
    let hr = si_qi(s, iid, out);
    si_release(s);
    hr
}

unsafe extern "system" fn si_qi(this: *mut SessionImpl, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = null_mut();
    if *iid == IID_XBTP_SESSION || *iid == IID_IUNKNOWN || *iid == IID_XBTP_CLIENT_SESSION {
        *out = this as *mut c_void;
        si_add_ref(this);
        S_OK
    } else {
        E_NOINTERFACE
    }
}

unsafe extern "system" fn si_add_ref(this: *mut SessionImpl) -> u32 {
    (*this).refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn si_release(this: *mut SessionImpl) -> u32 {
    let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        drop(Box::from_raw(this));
    }
    n
}

pub unsafe fn format_message_body(msg: *mut c_void, body: &mut XBTP_MESSAGE_BODY) -> HRESULT {
    if msg.is_null() {
        return E_INVALIDARG;
    }
    let v = *(msg as *const *const usize);
    let get_binary: unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut u8) -> HRESULT = core::mem::transmute(*v.add(4));
    let hr = get_binary(msg, &mut body.binary_body_length, &mut body.binary_body);
    if hr < 0 {
        return hr;
    }
    let get_json: unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut u8) -> HRESULT = core::mem::transmute(*v.add(3));
    get_json(msg, &mut body.json_body_length, &mut body.json_body)
}

pub unsafe fn convert_message(body: SharedPtr<XBTP_MESSAGE_BODY>, out: *mut *mut c_void) -> HRESULT {
    let m = new_message() as *mut c_void;
    let hr = message_set_received(m as *mut _, body);
    if hr < 0 {
        com_release(m);
        return hr;
    }
    let mut p: *mut c_void = null_mut();
    let hr = com_qi(m, &IID_IXBTP_MESSAGE, &mut p);
    if hr >= 0 {
        *out = p;
    }
    com_release(m);
    hr
}

unsafe fn locked<R>(this: *mut SessionImpl, f: impl FnOnce(*mut XbtpSession) -> R) -> R {
    let _g = (*this).lock.lock().unwrap_or_else(|e| e.into_inner());
    f((*this).session.ptr)
}

unsafe extern "system" fn si_send_begin_message(this: *mut SessionImpl, op: u32, msg: *mut c_void, exchange: *mut i32) -> HRESULT {
    locked(this, |s| {
        if exchange.is_null() {
            return E_INVALIDARG;
        }
        let mut body = XBTP_MESSAGE_BODY::default();
        let hr = format_message_body(msg, &mut body);
        if hr < 0 {
            return hr;
        }
        XbtpSession_SendBeginMessage(s, op, &body, exchange)
    })
}

unsafe extern "system" fn si_send_more_message(this: *mut SessionImpl, exchange: i32, msg: *mut c_void) -> HRESULT {
    locked(this, |s| {
        let mut body = XBTP_MESSAGE_BODY::default();
        let hr = format_message_body(msg, &mut body);
        if hr < 0 {
            return hr;
        }
        XbtpSession_SendMoreMessageCallback(s, exchange, &body, None, null_mut())
    })
}

unsafe extern "system" fn si_send_more_message_async(this: *mut SessionImpl, exchange: i32, msg: *mut c_void, cb: Option<SendCompletion>, ctx: *mut c_void) -> HRESULT {
    locked(this, |s| {
        if cb.is_none() {
            return E_INVALIDARG;
        }
        let mut body = XBTP_MESSAGE_BODY::default();
        let hr = format_message_body(msg, &mut body);
        if hr < 0 {
            return hr;
        }
        XbtpSession_SendMoreMessageCallback(s, exchange, &body, cb, ctx)
    })
}

unsafe extern "system" fn si_receive_more_message(this: *mut SessionImpl, timeout: u32, exchange: i32, out: *mut *mut c_void) -> HRESULT {
    locked(this, |s| {
        if out.is_null() {
            return E_INVALIDARG;
        }
        let mut body = SharedPtr::<XBTP_MESSAGE_BODY>::default();
        let hr = XbtpSession_ReceiveMoreMessageTimeout(s, timeout, exchange, &mut body);
        if hr < 0 {
            return hr;
        }
        convert_message(body, out)
    })
}

unsafe extern "system" fn si_receive_end_message(this: *mut SessionImpl, timeout: u32, exchange: i32, status: *mut i32, out: *mut *mut c_void) -> HRESULT {
    locked(this, |s| {
        if out.is_null() || status.is_null() {
            return E_INVALIDARG;
        }
        *status = 0;
        let mut body = SharedPtr::<XBTP_MESSAGE_BODY>::default();
        let hr = XbtpSession_ReceiveEndMessageTimeout(s, timeout, exchange, status, &mut body);
        if hr < 0 {
            return hr;
        }
        convert_message(body, out)
    })
}

unsafe extern "system" fn si_receive_message(this: *mut SessionImpl, timeout: u32, exchange: i32, message_type: *mut i16, status: *mut i32, out: *mut *mut c_void) -> HRESULT {
    locked(this, |s| {
        if message_type.is_null() || out.is_null() || status.is_null() {
            return E_INVALIDARG;
        }
        let mut body = SharedPtr::<XBTP_MESSAGE_BODY>::default();
        let hr = XbtpSession_ReceiveMessage(s, timeout, exchange, message_type, status, &mut body);
        if hr < 0 {
            return hr;
        }
        convert_message(body, out)
    })
}

unsafe extern "system" fn si_get_message_received_event(this: *mut SessionImpl, out: *mut HANDLE) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    session_get_event((*this).session.ptr, out)
}

unsafe extern "system" fn si_get_property(this: *mut SessionImpl, name: *const u16, value: *mut BSTR) -> HRESULT {
    if name.is_null() || value.is_null() {
        return E_INVALIDARG;
    }
    *value = null_mut();
    if from_wide(name).eq_ignore_ascii_case("address") {
        session_get_address((*this).session.ptr, value)
    } else {
        E_NOT_SET
    }
}

pub unsafe fn add_ref_raw(p: *mut c_void) -> u32 {
    com_add_ref(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_interface_clears_output_for_an_unsupported_iid() {
        unsafe {
            let session = new_session_impl(SharedPtr::default());
            let v = &*(*session).vtbl;
            let mut out = session as *mut c_void;
            let hr = (v.query_interface)(session, &GUID::default(), &mut out);
            let refs = (*session).refs.load(Ordering::Acquire);
            assert_eq!((v.release)(session), 0);
            assert_eq!(hr, E_NOINTERFACE);
            assert!(out.is_null(), "unsupported QueryInterface retained the caller's pointer");
            assert_eq!(refs, 1);
        }
    }

    #[test]
    fn query_interface_rejects_a_null_output_without_adding_a_reference() {
        unsafe {
            let session = new_session_impl(SharedPtr::default());
            let v = &*(*session).vtbl;
            for iid in [IID_IUNKNOWN, IID_XBTP_SESSION, IID_XBTP_CLIENT_SESSION, GUID::default()] {
                assert_eq!((v.query_interface)(session, &iid, null_mut()), E_POINTER);
                assert_eq!((*session).refs.load(Ordering::Acquire), 1);
            }
            assert_eq!((v.release)(session), 0);
        }
    }

    #[test]
    fn query_interface_preserves_identity_and_adds_one_reference_on_success() {
        unsafe {
            let session = new_session_impl(SharedPtr::default());
            let v = &*(*session).vtbl;
            for iid in [IID_IUNKNOWN, IID_XBTP_SESSION, IID_XBTP_CLIENT_SESSION] {
                let mut out = null_mut();
                assert_eq!((v.query_interface)(session, &iid, &mut out), S_OK);
                assert_eq!(out, session as *mut c_void);
                assert_eq!((*session).refs.load(Ordering::Acquire), 2);
                assert_eq!((v.release)(out as *mut SessionImpl), 1);
            }
            assert_eq!((v.release)(session), 0);
        }
    }
}
