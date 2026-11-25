use core::ffi::c_void;

pub type Iid = [u8; 16];

pub const fn iid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Iid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

pub const IID_IUNKNOWN: Iid = iid(0x0000_0000, 0x0000, 0x0000, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
pub const IID_FACTORY_X: Iid = iid(0xfb3b_54ac, 0xbf82, 0x463a, [0xb7, 0x77, 0x73, 0x19, 0x58, 0x37, 0x86, 0x4d]);
pub const IID_QUEUE_X1: Iid = iid(0x3c02_cb0c, 0xc050, 0x4195, [0x99, 0xa1, 0xa1, 0xce, 0x94, 0xba, 0x33, 0xc5]);
pub const IID_FILE_X: Iid = iid(0x4370_4f9f, 0x27c6, 0x4a9f, [0x88, 0x15, 0xd1, 0x65, 0x27, 0x89, 0x4d, 0xf4]);
pub const IID_QUEUE_X: Iid = iid(0xa3f6_649a, 0x8c2c, 0x4b3a, [0x8b, 0x6e, 0x10, 0x18, 0x6a, 0x04, 0xb9, 0xbc]);
pub const IID_STATUS_ARRAY_X: Iid = iid(0xcd66_d752, 0x2786, 0x44f7, [0x90, 0x70, 0xe2, 0xa6, 0xfd, 0xad, 0x3f, 0x1b]);
pub const IID_D3D12_FENCE_X: Iid = iid(0x4336_85fe, 0xe22b, 0x4ca0, [0xa8, 0xdb, 0xb5, 0xb4, 0xf4, 0xdd, 0x0e, 0x4a]);
pub const IID_D3D12_FENCE_PC: Iid = iid(0x0a75_3dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);

pub const S_OK: i32 = 0;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
pub const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
pub const E_POINTER: i32 = 0x8000_4003u32 as i32;
pub const E_PENDING: i32 = 0x8000_000au32 as i32;
pub const E_OUTOFMEMORY: i32 = 0x8007_000eu32 as i32;
pub const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
pub const HR_NOACCESS: i32 = 0x8007_03e6u32 as i32;
pub const HR_OPERATION_ABORTED: i32 = 0x8007_03e3u32 as i32;

pub const E_DSTORAGE_INVALID_QUEUE_CAPACITY: i32 = 0x8924_0003u32 as i32;
pub const E_DSTORAGE_END_OF_FILE: i32 = 0x8924_0007u32 as i32;
pub const E_DSTORAGE_ACCESS_VIOLATION: i32 = 0x8924_0009u32 as i32;
pub const E_DSTORAGE_RESERVED_FIELDS: i32 = 0x8924_000cu32 as i32;
pub const E_DSTORAGE_INVALID_DESTINATION_SIZE: i32 = 0x8924_000fu32 as i32;
pub const E_DSTORAGE_QUEUE_CLOSED: i32 = 0x8924_0010u32 as i32;
pub const E_DSTORAGE_INVALID_QUEUE_PRIORITY: i32 = 0x8924_0013u32 as i32;
pub const E_DSTORAGE_INDEX_BOUND: i32 = 0x8924_0015u32 as i32;
pub const E_DSTORAGE_INVALID_FILE_HANDLE: i32 = 0x8924_0017u32 as i32;
pub const E_DSTORAGE_INVALID_SOURCE_TYPE: i32 = 0x8924_001bu32 as i32;
pub const E_DSTORAGE_INVALID_INTERMEDIATE_SIZE: i32 = 0x8924_001cu32 as i32;
pub const E_DSTORAGE_STAGING_BUFFER_LOCKED: i32 = 0x8924_001fu32 as i32;
pub const E_DSTORAGE_INVALID_FENCE: i32 = 0x8924_0022u32 as i32;
pub const E_DSTORAGE_INVALID_STATUS_ARRAY: i32 = 0x8924_0023u32 as i32;
pub const E_DSTORAGE_INVALID_MEMORY_QUEUE_PRIORITY: i32 = 0x8924_0024u32 as i32;
pub const E_DSTORAGE_DECOMPRESSION_ERROR: i32 = 0x8924_0030u32 as i32;
pub const E_DSTORAGE_ZLIB_BAD_HEADER: i32 = 0x8924_0031u32 as i32;
pub const E_DSTORAGE_ZLIB_BAD_DATA: i32 = 0x8924_0032u32 as i32;
pub const E_DSTORAGE_ZLIB_PARITY_FAIL: i32 = 0x8924_0033u32 as i32;
pub const E_DSTORAGE_PASSTHROUGH_ERROR: i32 = 0x8924_0037u32 as i32;
pub const E_DSTORAGE_INVALID_QUEUE_TYPE: i32 = 0x8924_00f5u32 as i32;

pub const COMPRESSION_NONE: u8 = 0;
pub const COMPRESSION_ZLIB: u8 = 1;

pub const OPT_EXT10_VALID: u64 = 1 << 21;
pub const OPT_SOURCE_MEMORY: u64 = 1 << 22;
pub const OPT_EXT28_VALID: u64 = 1 << 23;
pub const OPT_ZLIB: u64 = 1;
pub const OPT_BCPACK_SHIFT: u32 = 1;
pub const OPT_BCPACK_MASK: u64 = 0xf;
pub const OPT_SWIZZLE_SHIFT: u32 = 5;
pub const OPT_SWIZZLE_MASK: u64 = 0xffff;
pub const OPT_KNOWN_MASK: u64 = OPT_ZLIB | (OPT_BCPACK_MASK << OPT_BCPACK_SHIFT) | OPT_SOURCE_MEMORY;

pub const QUEUE_SOURCE_FILE: u32 = 0;
pub const QUEUE_SOURCE_MEMORY: u32 = 1;
pub const PRIORITY_LOW: i8 = -1;
pub const PRIORITY_REALTIME: i8 = 2;

pub const XREQUEST_SIZE: usize = 0x48;
pub const XQUEUE_DESC_SIZE: usize = 0x18;
pub const XQUEUE_INFO_SIZE: usize = 0x1c;
pub const XERROR_RECORD_SIZE: usize = 0x290;
pub const XERR_FILENAME_OFF: usize = 0x24;
pub const XERR_FILENAME_CHARS: usize = 0x104;
pub const XERR_NAME_OFF: usize = 0x250;
pub const XERR_NAME_CHARS: usize = 0x40;
pub const STATUS_NAME_CHARS: usize = 0x20;

pub const COMMAND_TYPE_NONE: u32 = 0xffff_ffff;
pub const COMMAND_TYPE_REQUEST: u32 = 0;
pub const COMMAND_TYPE_STATUS: u32 = 1;
pub const COMMAND_TYPE_SIGNAL: u32 = 2;

pub const PC_FENCE_SLOT_SIGNAL: usize = 10;

pub fn hresult_from_win32(code: u32) -> i32 {
    if code == 0 {
        S_OK
    } else if (code as i32) < 0 {
        code as i32
    } else {
        (0x8007_0000u32 | (code & 0xffff)) as i32
    }
}

pub unsafe fn read_iid(p: *const c_void) -> Option<Iid> {
    if p.is_null() {
        return None;
    }
    let mut out = [0u8; 16];
    unsafe { core::ptr::copy_nonoverlapping(p as *const u8, out.as_mut_ptr(), 16) };
    Some(out)
}

pub fn iid_text(i: &Iid) -> String {
    let d1 = u32::from_le_bytes([i[0], i[1], i[2], i[3]]);
    let d2 = u16::from_le_bytes([i[4], i[5]]);
    let d3 = u16::from_le_bytes([i[6], i[7]]);
    format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        i[8], i[9], i[10], i[11], i[12], i[13], i[14], i[15]
    )
}

pub unsafe fn c_string(p: *const u8, max: usize) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut v = Vec::new();
    for i in 0..max {
        let b = unsafe { *p.add(i) };
        if b == 0 {
            break;
        }
        v.push(b);
    }
    String::from_utf8_lossy(&v).into_owned()
}

pub unsafe fn wide_string(p: *const u16, max: usize) -> Vec<u16> {
    let mut v = Vec::new();
    if p.is_null() {
        return v;
    }
    for i in 0..max {
        let c = unsafe { *p.add(i) };
        if c == 0 {
            break;
        }
        v.push(c);
    }
    v
}
