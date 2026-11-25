use console_sandbox::{claims, xvcmgmt};

extern "system" {
    fn CreateEventW(sa: *const core::ffi::c_void, manual: i32, initial: i32, name: *const u16) -> usize;
    fn WaitForSingleObject(h: usize, ms: u32) -> u32;
    fn CloseHandle(h: usize) -> i32;
}

const STATUS_NOT_IMPLEMENTED: i32 = 0xC000_0002u32 as i32;
const STATUS_INVALID_CONNECTION: i32 = 0xC000_0140u32 as i32;
const STATUS_INVALID_CONNECTION_STATE: i32 = 0xC000_02CCu32 as i32;

fn setup() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("verify_xvcmgmt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("TOMBOX_LOG", dir.join("tombox_compat.log"));
    std::env::set_var("TOMBOX_GDK_ROOT", dir.join("data"));
}

struct Call {
    io: Box<[usize; 2]>,
    ev: usize,
}

impl Call {
    fn new() -> Call {
        Call { io: Box::new([0x55; 2]), ev: unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) } }
    }
    fn run(&mut self, code: u32, inb: &[u8], out: &mut [u8]) -> i32 {
        let o = if out.is_empty() { core::ptr::null_mut() } else { out.as_mut_ptr() };
        let i = if inb.is_empty() { core::ptr::null() } else { inb.as_ptr() };
        unsafe { xvcmgmt::ioctl(self.io.as_mut_ptr(), self.ev, 0, 0, code, i, inb.len() as u32, o, out.len() as u32) }
    }
    fn wait(&self, ms: u32) -> Option<(i32, usize)> {
        (unsafe { WaitForSingleObject(self.ev, ms) } == 0).then(|| (self.io[0] as u32 as i32, self.io[1]))
    }
    fn untouched(&self) -> bool {
        self.wait(0).is_none() && self.io[0] == 0x55 && self.io[1] == 0x55
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.ev) };
    }
}

fn sync_fail(code: u32, inb: &[u8], out: &mut [u8]) -> i32 {
    let mut c = Call::new();
    let st = c.run(code, inb, out);
    assert!(st < 0, "{code:#x} expected a synchronous failure, got {st:#x}");
    assert!(c.untouched(), "{code:#x}: a synchronous NT_ERROR must not signal the event or write the IOSB");
    st
}

fn create(vm: [u8; 16], svc: [u8; 16]) -> u64 {
    let mut inb = vm.to_vec();
    inb.extend_from_slice(&svc);
    let mut out = [0u8; 8];
    let mut c = Call::new();
    assert_eq!(c.run(xvcmgmt::IOCTL_CREATE, &inb, &mut out), 0);
    assert_eq!(c.wait(0), Some((0, 8)), "CREATE completes synchronously with Information = 8");
    u64::from_le_bytes(out)
}

fn pend(code: u32, id: u64, out: &mut [u8]) -> Call {
    let mut c = Call::new();
    assert_eq!(c.run(code, &id.to_le_bytes(), out), xvcmgmt::STATUS_PENDING, "{code:#x}");
    c
}

fn send_entries(id: u64, entries: &[(&[u8], u32)]) -> (i32, Call) {
    let mut inb = id.to_le_bytes().to_vec();
    for (p, l) in entries {
        inb.extend_from_slice(&(p.as_ptr() as u64).to_le_bytes());
        inb.extend_from_slice(&l.to_le_bytes());
    }
    let mut c = Call::new();
    let st = c.run(xvcmgmt::IOCTL_SEND, &inb, &mut []);
    (st, c)
}

#[test]
fn counter_group_reads_the_four_19041_sfr_values() {
    use console_sandbox::xvcsys::{IOCTL_SFR_COUNTER_GROUP, IOCTL_SFR_UPDATE};
    if std::env::var_os("XVC_COUNTER_GROUP_TEST_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "counter_group_reads_the_four_19041_sfr_values"])
            .env("XVC_COUNTER_GROUP_TEST_CHILD", "1")
            .env("TOMBOX_CONSOLE_OSBUILD", "19041")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let mut out = [0xa5; 35];
    assert_eq!(sync_fail(IOCTL_SFR_COUNTER_GROUP, &[], &mut out[1..33]), xvcmgmt::STATUS_INVALID_PARAMETER);
    assert_eq!(out, [0xa5; 35]);
    let mut updates = Vec::new();
    for i in 0..4u32 {
        updates.extend((0x28 + i).to_le_bytes());
        updates.extend(0u32.to_le_bytes());
        updates.extend((0x1234_5678_0000_0000u64 + u64::from(i)).to_le_bytes());
    }
    let mut call = Call::new();
    assert_eq!(call.run(IOCTL_SFR_UPDATE, &updates, &mut []), 0);
    assert_eq!(call.run(IOCTL_SFR_COUNTER_GROUP, &[], &mut out[1..]), 0);
    assert_eq!(call.wait(0), Some((0, 33)));
    for (i, bytes) in out[1..33].chunks_exact(8).enumerate() {
        assert_eq!(u64::from_le_bytes(bytes.try_into().unwrap()), 0x1234_5678_0000_0000 + i as u64);
    }
    assert_eq!((out[0], out[33], out[34]), (0xa5, 0, 0xa5));
}

#[test]
fn driver_size_checks_and_status_codes() {
    setup();
    assert_eq!(sync_fail(xvcmgmt::IOCTL_CREATE, &[0; 0x20], &mut [0; 7]), xvcmgmt::STATUS_INVALID_PARAMETER, "CREATE out < 8");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_LISTEN, &[0; 7], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER, "LISTEN in < 8");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_CONNECT, &[0; 7], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER, "CONNECT in < 8");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_LISTEN, &0xbeefu64.to_le_bytes(), &mut []), xvcmgmt::STATUS_INVALID_HANDLE, "LISTEN unknown id is synchronous");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_ACCEPT, &0xbeefu64.to_le_bytes(), &mut [0; 8]), xvcmgmt::STATUS_INVALID_HANDLE);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_ACCEPT, &[0; 7], &mut [0; 8]), xvcmgmt::STATUS_BUFFER_TOO_SMALL);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_RECEIVE, &[0; 7], &mut [0; 8]), xvcmgmt::STATUS_BUFFER_TOO_SMALL);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_RECEIVE, &0xbeefu64.to_le_bytes(), &mut [0; 8]), xvcmgmt::STATUS_INVALID_HANDLE);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SEND, &[0; 0x13], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER, "SEND in < 0x14");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SEND, &[0; 0x20], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER, "SEND (in-8) % 12 != 0");
    let b = [1u8; 4];
    assert_eq!(send_entries(0xbeef, &[(&b, 4)]).0, xvcmgmt::STATUS_INVALID_HANDLE);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_CLOSE, &[0; 7], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SHUTDOWN, &[0; 7], &mut []), xvcmgmt::STATUS_INVALID_PARAMETER);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SHUTDOWN, &0xbeefu64.to_le_bytes(), &mut []), xvcmgmt::STATUS_INVALID_HANDLE);
    assert_eq!(sync_fail(0x0015_0099, &[0; 8], &mut []), xvcmgmt::STATUS_INVALID_DEVICE_REQUEST);

    let lone = create(xvcmgmt::LOOPBACK_VM, [0x31; 16]);
    assert_eq!(send_entries(lone, &[(&b, 4)]).0, STATUS_INVALID_CONNECTION, "SEND on an unconnected queue");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SHUTDOWN, &lone.to_le_bytes(), &mut []), STATUS_INVALID_CONNECTION_STATE, "loopback, not connected");
    let host_unconnected = create(xvcmgmt::HOST_VM, [0x32; 16]);
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SHUTDOWN, &host_unconnected.to_le_bytes(), &mut []), STATUS_NOT_IMPLEMENTED, "non-loopback checked before connected state");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_RECEIVE, &lone.to_le_bytes(), &mut []), xvcmgmt::STATUS_BUFFER_TOO_SMALL, "RECEIVE out == 0");
}

#[test]
fn send_scatter_and_zero_length_entries() {
    setup();
    let svc = [0x41u8; 16];
    let server = create(xvcmgmt::LOOPBACK_VM, svc);
    let a = create(xvcmgmt::LOOPBACK_VM, svc);
    assert_eq!(pend(xvcmgmt::IOCTL_LISTEN, server, &mut []).wait(5000), Some((0, 0)));
    let mut accepted = [0u8; 8];
    let mut acc = Call::new();
    assert_eq!(acc.run(xvcmgmt::IOCTL_ACCEPT, &server.to_le_bytes(), &mut accepted), xvcmgmt::STATUS_PENDING);
    assert_eq!(pend(xvcmgmt::IOCTL_CONNECT, a, &mut []).wait(5000), Some((0, 0)));
    assert_eq!(acc.wait(5000), Some((0, 8)));
    let b = u64::from_le_bytes(accepted);

    let (p1, p2) = (b"head".to_vec(), b"tail!".to_vec());
    let (st, c) = send_entries(a, &[(&p1, 4), (&p2, 5)]);
    assert_eq!(st, 0);
    assert_eq!(c.wait(0), Some((0, 0)), "SEND completes synchronously, Information 0");
    let mut buf = [0u8; 32];
    assert_eq!(pend(xvcmgmt::IOCTL_RECEIVE, b, &mut buf).wait(5000), Some((0, 9)));
    assert_eq!(&buf[..9], b"headtail!");

    let mut c = Call::new();
    let mut inb = a.to_le_bytes().to_vec();
    inb.extend_from_slice(&(p1.as_ptr() as u64).to_le_bytes());
    inb.extend_from_slice(&4u32.to_le_bytes());
    inb.extend_from_slice(&(p2.as_ptr() as u64).to_le_bytes());
    inb.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(c.run(xvcmgmt::IOCTL_SEND, &inb, &mut []), xvcmgmt::STATUS_INVALID_PARAMETER, "a zero-length scatter entry fails the whole send");
    assert!(c.untouched());
    let r = pend(xvcmgmt::IOCTL_RECEIVE, b, &mut buf);
    assert_eq!(r.wait(150), None, "nothing from the rejected send was delivered");
    assert_eq!(Call::new().run(xvcmgmt::IOCTL_CLOSE, &b.to_le_bytes(), &mut []), 0);
    assert_eq!(r.wait(5000).map(|x| x.0), Some(xvcmgmt::STATUS_CANCELLED));
    for id in [a, server] {
        Call::new().run(xvcmgmt::IOCTL_CLOSE, &id.to_le_bytes(), &mut []);
    }
}

#[test]
fn client_flow_against_the_emulated_host() {
    setup();
    let other = create(xvcmgmt::HOST_VM, [0x77; 16]);
    assert_eq!(pend(xvcmgmt::IOCTL_CONNECT, other, &mut []).wait(5000), Some((xvcmgmt::STATUS_CONNECTION_REFUSED, 0)), "an unknown host service completes refused, it does not hang");
    let q = create(xvcmgmt::HOST_VM, claims::launch_instance());
    assert_eq!(pend(xvcmgmt::IOCTL_CONNECT, q, &mut []).wait(5000), Some((0, 0)));
    let before = xvcmgmt::host_connections(claims::launch_instance());
    let mut frame = vec![0u8; 0x10];
    frame[8..12].copy_from_slice(&1u32.to_le_bytes());
    frame[12..16].copy_from_slice(&4u32.to_le_bytes());
    frame.extend_from_slice(b"ping");
    let (st, c) = send_entries(q, &[(&frame, frame.len() as u32)]);
    assert_eq!(st, 0);
    assert_eq!(c.wait(0), Some((0, 0)));
    let mut rx = [0u8; 0x4000];
    let r = pend(xvcmgmt::IOCTL_RECEIVE, q, &mut rx);
    assert_eq!(r.wait(300), None, "the emulated host never answers on its own");
    let id = *before.last().unwrap();
    assert_eq!(xvcmgmt::host_take_received(id), frame);
    let log = [std::path::PathBuf::from(std::env::var("TOMBOX_LOG").unwrap()), std::env::temp_dir().join("tombox_compat.log")].iter().map(|p| std::fs::read_to_string(p).unwrap_or_default()).collect::<String>();
    assert!(log.contains(&format!("queue {q} sent {} bytes to emulated host endpoint", frame.len())), "the swallowed send is logged");
    assert_eq!(sync_fail(xvcmgmt::IOCTL_SHUTDOWN, &q.to_le_bytes(), &mut []), STATUS_NOT_IMPLEMENTED);
    assert_eq!(Call::new().run(xvcmgmt::IOCTL_CLOSE, &q.to_le_bytes(), &mut []), 0);
    assert_eq!(r.wait(5000).map(|x| x.0), Some(xvcmgmt::STATUS_CANCELLED));
}

#[test]
fn layout_ioctl_returns_two_frames_in_19041() {
    if std::env::var_os("XVC_LAYOUT_TEST_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "layout_ioctl_returns_two_frames_in_19041"])
            .env("XVC_LAYOUT_TEST_CHILD", "1")
            .env("TOMBOX_CONSOLE_OSBUILD", "19041")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let mut bytes = [0xaa; 18];
    let code = xvcmgmt::IOCTL_PHYSICAL_MEMORY_LAYOUT;
    assert_eq!(sync_fail(code, &[], &mut bytes[1..16]), xvcmgmt::STATUS_BUFFER_TOO_SMALL);
    assert_eq!(bytes, [0xaa; 18]);
    let mut call = Call::new();
    assert_eq!(call.run(code, &[], &mut bytes[1..]), 0);
    assert_eq!(call.wait(0), Some((0, 16)));
    assert_eq!(u64::from_le_bytes(bytes[1..9].try_into().unwrap()), 0x10000);
    assert_eq!(bytes[1..9], bytes[9..17]);
    assert_eq!((bytes[0], bytes[17]), (0xaa, 0xaa));
}
