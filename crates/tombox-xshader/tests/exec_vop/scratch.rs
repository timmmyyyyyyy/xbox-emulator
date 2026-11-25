use super::*;

const NO_SADDR: u32 = 0x7d;
const LOAD_UBYTE: u32 = 0x08;
const LOAD_SBYTE: u32 = 0x09;
const LOAD_USHORT: u32 = 0x0a;
const LOAD_SSHORT: u32 = 0x0b;
const LOAD_DWORD: u32 = 0x0c;
const LOAD_DWORDX2: u32 = 0x0d;
const LOAD_DWORDX4: u32 = 0x0e;
const STORE_BYTE: u32 = 0x18;
const STORE_SHORT: u32 = 0x1a;
const STORE_DWORD: u32 = 0x1c;
const STORE_DWORDX2: u32 = 0x1d;
const STORE_DWORDX4: u32 = 0x1e;

fn scratch(op: u32, offset: i32, addr: u32, data: u32, saddr: u32, vdst: u32) -> [u32; 2] {
    [0xdc00_0000 | (op << 18) | (1 << 14) | (offset as u32 & 0xfff), addr | (data << 8) | (saddr << 16) | (vdst << 24)]
}

fn store(op: u32, offset: i32, addr: u32, data: u32) -> [u32; 2] {
    scratch(op, offset, addr, data, NO_SADDR, 0)
}

fn load(op: u32, offset: i32, addr: u32, vdst: u32) -> [u32; 2] {
    scratch(op, offset, addr, 0, NO_SADDR, vdst)
}

fn round_trip(c: &mut Cases, m: &'static str, init: &[(u32, u32)], code: Vec<u32>, outs: &[u32], want: &[u32]) {
    vc(c, m, init, &code, outs, want);
}

#[test]
fn scratch_memory_stores_and_loads_per_thread() {
    let mut c = Cases::new();
    let word = 0xaabb_ccddu32;
    let join = |a: [u32; 2], b: [u32; 2]| vec![a[0], a[1], b[0], b[1]];
    round_trip(&mut c, "flat_store_dword", &[(1, 16), (2, word)], join(store(STORE_DWORD, 0, 1, 2), load(LOAD_DWORD, 0, 1, 10)), &[10], &[word]);
    round_trip(&mut c, "flat_store_dword", &[(1, 8), (2, word), (3, 12)], join(store(STORE_DWORD, 4, 1, 2), load(LOAD_DWORD, 0, 3, 10)), &[10], &[word]);
    round_trip(&mut c, "flat_store_dword", &[(1, 20), (2, word), (3, 12)], join(store(STORE_DWORD, -8, 1, 2), load(LOAD_DWORD, 0, 3, 10)), &[10], &[word]);
    round_trip(&mut c, "flat_store_dword", &[(1, 4096), (2, word)], join(store(STORE_DWORD, 0, 1, 2), load(LOAD_DWORD, 0, 1, 10)), &[10], &[0]);
    round_trip(&mut c, "flat_load_dword", &[(1, 32)], load(LOAD_DWORD, 0, 1, 10).to_vec(), &[10], &[0]);
    for (op, off, want) in [
        (LOAD_UBYTE, 0i32, 0xddu32),
        (LOAD_UBYTE, 1, 0xcc),
        (LOAD_UBYTE, 3, 0xaa),
        (LOAD_SBYTE, 3, 0xffff_ffaa),
        (LOAD_SBYTE, 0, 0xffff_ffdd),
        (LOAD_USHORT, 2, 0xaabb),
        (LOAD_USHORT, 0, 0xccdd),
        (LOAD_SSHORT, 0, 0xffff_ccdd),
        (LOAD_SSHORT, 2, 0xffff_aabb),
    ] {
        round_trip(&mut c, "flat_store_dword", &[(1, 16), (2, word)], join(store(STORE_DWORD, 0, 1, 2), load(op, off, 1, 10)), &[10], &[want]);
    }
    round_trip(
        &mut c,
        "flat_store_dword",
        &[(1, 16), (2, 0x1122_3344), (3, 0xab)],
        [store(STORE_DWORD, 0, 1, 2), store(STORE_BYTE, 1, 1, 3), load(LOAD_DWORD, 0, 1, 10)].concat(),
        &[10],
        &[0x1122_ab44],
    );
    round_trip(
        &mut c,
        "flat_store_dword",
        &[(1, 16), (2, 0x1122_3344), (3, 0xbeef)],
        [store(STORE_DWORD, 0, 1, 2), store(STORE_SHORT, 2, 1, 3), load(LOAD_DWORD, 0, 1, 10)].concat(),
        &[10],
        &[0xbeef_3344],
    );
    round_trip(
        &mut c,
        "flat_store_dword",
        &[(1, 18), (2, 0xddcc_bbaa)],
        [store(STORE_DWORD, 0, 1, 2), load(LOAD_DWORD, 0, 1, 10), load(LOAD_DWORD, -2, 1, 11), load(LOAD_DWORD, 2, 1, 12)].concat(),
        &[10, 11, 12],
        &[0xddcc_bbaa, 0xbbaa_0000, 0x0000_ddcc],
    );
    round_trip(
        &mut c,
        "flat_store_dwordx2",
        &[(1, 40), (2, 0x1111_1111), (3, 0x2222_2222)],
        [store(STORE_DWORDX2, 0, 1, 2), load(LOAD_DWORDX2, 0, 1, 10)].concat(),
        &[10, 11],
        &[0x1111_1111, 0x2222_2222],
    );
    round_trip(
        &mut c,
        "flat_store_dwordx4",
        &[(1, 64), (2, 1), (3, 2), (4, 3), (5, 4)],
        [store(STORE_DWORDX4, 4, 1, 2), load(LOAD_DWORDX4, 4, 1, 10)].concat(),
        &[10, 11, 12, 13],
        &[1, 2, 3, 4],
    );
    round_trip(
        &mut c,
        "flat_store_dwordx4",
        &[(1, 64), (2, 1), (3, 2), (4, 3), (5, 4)],
        [store(STORE_DWORDX4, 4, 1, 2), load(LOAD_DWORD, 12, 1, 10)].concat(),
        &[10],
        &[3],
    );
    c.run("scratch_memory");
}

#[test]
fn scratch_memory_addressed_through_an_sgpr() {
    let mut c = Cases::new();
    let value = 0x5a5a_1234u32;
    let code = vec![
        sopk(0, 20, 32),
        scratch(STORE_DWORD, 8, 0, 2, 20, 0)[0],
        scratch(STORE_DWORD, 8, 0, 2, 20, 0)[1],
        load(LOAD_DWORD, 0, 1, 10)[0],
        load(LOAD_DWORD, 0, 1, 10)[1],
    ];
    round_trip(&mut c, "s_movk_i32", &[(1, 40), (2, value)], code, &[10], &[value]);
    let code = vec![
        sopk(0, 20, 64),
        store(STORE_DWORD, 0, 1, 2)[0],
        store(STORE_DWORD, 0, 1, 2)[1],
        scratch(LOAD_DWORD, -4, 0, 0, 20, 10)[0],
        scratch(LOAD_DWORD, -4, 0, 0, 20, 10)[1],
    ];
    round_trip(&mut c, "s_movk_i32", &[(1, 60), (2, value)], code, &[10], &[value]);
    c.run("scratch_memory_sgpr");
}
