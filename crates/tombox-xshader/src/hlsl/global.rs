use super::{r, Emitter};
use crate::global_abi::{TB_GLOBAL_FAULT_UAV, TB_GLOBAL_SPACE, TB_GLOBAL_WINDOWS};
use crate::rdna2::{Enc, Insn, Opnd};

pub const HELPERS: &str = r#"cbuffer TbGlobal : register(bCBV, spaceSPACE) { uint4 tb_gwin[NWIN]; uint4 tb_msin; };
RWByteAddressBuffer tb_gw0 : register(u0, spaceSPACE);
RWByteAddressBuffer tb_gw1 : register(u1, spaceSPACE);
RWByteAddressBuffer tb_gfault : register(uFAULT, spaceSPACE);
uint2 tb_gva(uint lo, uint hi, uint add, int off) {
    uint a = lo + add;
    uint c = a < lo ? 1u : 0u;
    uint b = a + (uint)off;
    uint d = off < 0 ? (b <= a ? 0u : 0xffffffffu) : (b < a ? 1u : 0u);
    return uint2(b, hi + c + d);
}
uint tb_gwhich(uint2 va, out uint o) {
    [unroll] for (uint i = 0u; i < NWINu; i++) {
        uint2 b = tb_gwin[i].xy;
        uint lo = va.x - b.x;
        uint hi = va.y - b.y - (va.x < b.x ? 1u : 0u);
        if (tb_gwin[i].z >= 4u && hi == 0u && lo <= tb_gwin[i].z - 4u) { o = lo; return i; }
    }
    o = 0u;
    return 0xffffffffu;
}
void tb_gmiss(uint2 va) {
    uint p;
    tb_gfault.InterlockedAdd(0, 1u, p);
    if (p == 0u) tb_gfault.Store2(4, va);
}
uint tb_gload_al(uint w, uint o) {
    if (w == 0u) return tb_gw0.Load(o);
    return tb_gw1.Load(o);
}
uint tb_gmem_load(uint2 va, uint nbytes) {
    uint2 a = tb_gva(va.x & ~3u, va.y, 0u, 0);
    uint o;
    uint w = tb_gwhich(a, o);
    if (w == 0xffffffffu) { tb_gmiss(va); return 0u; }
    uint sh = (va.x & 3u) * 8u;
    uint lo = tb_gload_al(w, o);
    if ((va.x & 3u) + nbytes <= 4u) return lo >> sh;
    uint2 n = tb_gva(a.x, a.y, 4u, 0);
    uint o2;
    uint w2 = tb_gwhich(n, o2);
    if (w2 == 0xffffffffu) { tb_gmiss(n); return lo >> sh; }
    return (lo >> sh) | (tb_gload_al(w2, o2) << (32u - sh));
}
void tb_gstore_bits(uint2 va, uint v, uint nbytes) {
    uint2 a = tb_gva(va.x & ~3u, va.y, 0u, 0);
    uint o;
    uint w = tb_gwhich(a, o);
    if (w == 0xffffffffu) { tb_gmiss(va); return; }
    uint sh = (va.x & 3u) * 8u;
    uint m = (nbytes >= 4u ? 0xffffffffu : ((1u << (nbytes * 8u)) - 1u)) << sh;
    uint p;
    if (m == 0xffffffffu) {
        if (w == 0u) tb_gw0.Store(o, v); else tb_gw1.Store(o, v);
        return;
    }
    uint keep = ~m;
    uint put = (v << sh) & m;
    if (w == 0u) { tb_gw0.InterlockedAnd(o, keep, p); tb_gw0.InterlockedOr(o, put, p); }
    else { tb_gw1.InterlockedAnd(o, keep, p); tb_gw1.InterlockedOr(o, put, p); }
}
void tb_gmem_store_n(uint2 va, uint v, uint nbytes) {
    uint sh = (va.x & 3u) * 8u;
    uint first = min(nbytes, 4u - (va.x & 3u));
    tb_gstore_bits(va, v, first);
    if (first < nbytes) tb_gstore_bits(tb_gva(va.x, va.y, first, 0), v >> (first * 8u), nbytes - first);
}
void tb_gmem_store(uint2 va, uint v) { tb_gmem_store_n(va, v, 4u); }
#define TB_GMEM 1
#ifndef TB_AMP_GMEM
#define tb_gload tb_gmem_load
#define tb_gstore tb_gmem_store
#define tb_gstore_n tb_gmem_store_n
#endif
"#;

pub fn helpers() -> String {
    HELPERS
        .replace("bCBV", &format!("b{}", crate::global_abi::TB_GLOBAL_CBV))
        .replace("uFAULT", &format!("u{TB_GLOBAL_FAULT_UAV}"))
        .replace("NWINu", &format!("{TB_GLOBAL_WINDOWS}u"))
        .replace("NWIN", &TB_GLOBAL_WINDOWS.to_string())
        .replace("SPACE", &TB_GLOBAL_SPACE.to_string())
}

fn width(m: &str) -> Option<(bool, u8, u8, bool)> {
    Some(match m {
        "flat_load_ubyte" | "flat_load_ubyte_d16" | "flat_load_ubyte_d16_hi" => (false, 1, 1, false),
        "flat_load_sbyte" | "flat_load_sbyte_d16" | "flat_load_sbyte_d16_hi" => (false, 1, 1, true),
        "flat_load_ushort" => (false, 1, 2, false),
        "flat_load_sshort" => (false, 1, 2, true),
        "flat_load_short_d16" | "flat_load_short_d16_hi" => (false, 1, 2, false),
        "flat_load_dword" => (false, 1, 4, false),
        "flat_load_dwordx2" => (false, 2, 4, false),
        "flat_load_dwordx3" => (false, 3, 4, false),
        "flat_load_dwordx4" => (false, 4, 4, false),
        "flat_store_byte" | "flat_store_byte_d16_hi" => (true, 1, 1, false),
        "flat_store_short" | "flat_store_short_d16_hi" => (true, 1, 2, false),
        "flat_store_dword" => (true, 1, 4, false),
        "flat_store_dwordx2" => (true, 2, 4, false),
        "flat_store_dwordx3" => (true, 3, 4, false),
        "flat_store_dwordx4" => (true, 4, 4, false),
        _ => return None,
    })
}

pub fn routes(ins: &Insn, insns: &[Insn]) -> bool {
    if ins.enc != Enc::Flat {
        return false;
    }
    let m = ins.mnemonic.as_str();
    if m.starts_with("flat_load_dword") && ins.aux.mask != 1 && insns.iter().any(|i| i.mnemonic == "s_getpc_b64") {
        return false;
    }
    ins.aux.mask != 1 || width(m).is_some_and(|w| w.0)
}

impl<'a> Emitter<'a> {
    fn global_va(&mut self, ins: &Insn) -> Result<String, String> {
        let off = ins.aux.imm as i32;
        let v = match ins.src.first() {
            Some(Opnd::Vgpr(v)) => *v,
            _ => return Err("global access without a VGPR address".into()),
        };
        match ins.src.get(2) {
            Some(Opnd::Sgpr(s)) => {
                let (s0, s1) = (Opnd::Sgpr(*s), Opnd::Sgpr(s + 1));
                self.touch(&s1);
                Ok(format!("tb_gva({}, {}, {}, {off})", r(&s0)?, r(&s1)?, r(&Opnd::Vgpr(v))?))
            }
            Some(Opnd::Null) | None => {
                let hi = Opnd::Vgpr(v + 1);
                self.touch(&hi);
                Ok(format!("tb_gva({}, {}, 0u, {off})", r(&Opnd::Vgpr(v))?, r(&hi)?))
            }
            Some(o) => Err(format!("global access with scalar address {o:?} unsupported")),
        }
    }

    pub(super) fn global_load_expr(&self, va: &str, k: u8, bytes: u8, signed: bool) -> String {
        let a = if k == 0 { va.to_string() } else { format!("tb_gva({va}.x, {va}.y, {}u, 0)", 4 * k as u32) };
        match (bytes, signed) {
            (4, _) => format!("tb_gload({a}, {bytes}u)"),
            (1, false) => format!("(tb_gload({a}, {bytes}u) & 0xffu)"),
            (2, false) => format!("(tb_gload({a}, {bytes}u) & 0xffffu)"),
            (1, true) => format!("(uint)(((int)(tb_gload({a}, {bytes}u) << 24u)) >> 24u)"),
            _ => format!("(uint)(((int)(tb_gload({a}, {bytes}u) << 16u)) >> 16u)"),
        }
    }

    pub(super) fn global_store_line(&self, va: &str, k: u8, val: &str, bytes: u8) -> String {
        let a = if k == 0 { va.to_string() } else { format!("tb_gva({va}.x, {va}.y, {}u, 0)", 4 * k as u32) };
        if bytes == 4 { format!("tb_gstore({a}, {val});") } else { format!("tb_gstore_n({a}, {val}, {bytes}u);") }
    }

    pub(super) fn emit_global(&mut self, ins: &Insn) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        if ins.aux.mask == 1 {
            return Err(format!("{m}: scratch-segment access unsupported"));
        }
        if ins.raw.first().is_some_and(|w| (w >> 13) & 1 != 0) {
            return Err(format!("{m}: LDS=1 (memory <-> LDS transfer) is not lowered"));
        }
        let (store, n, bytes, signed) = width(m).ok_or_else(|| format!("{m}: global/flat operation unsupported"))?;
        let va = self.global_va(ins)?;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint2 g{t} = {va};"));
        let g = format!("g{t}");
        if store {
            let data = match ins.src.get(1) {
                Some(Opnd::Vgpr(d)) => *d,
                _ => return Err(format!("{m}: data operand is not a VGPR")),
            };
            let mut body = Vec::new();
            for k in 0..n {
                let o = Opnd::Vgpr(data + k);
                self.touch(&o);
                let v = if m.ends_with("_d16_hi") { format!("({} >> 16u)", r(&o)?) } else { r(&o)? };
                body.push(self.global_store_line(&g, k, &v, bytes));
            }
            self.line(&format!("if (exec_lo & 1u) {{ {} }}", body.join(" ")));
            return Ok(());
        }
        let vd = match ins.dst.first() {
            Some(Opnd::Vgpr(v)) => *v,
            _ => return Err(format!("{m}: bad vdst")),
        };
        let decl: Vec<String> = (0..n).map(|k| format!("g{t}_{k} = 0u")).collect();
        self.line(&format!("uint {};", decl.join(", ")));
        let loads: Vec<String> = (0..n).map(|k| format!("g{t}_{k} = {};", self.global_load_expr(&g, k, bytes, signed))).collect();
        self.line(&format!("if (exec_lo & 1u) {{ {} }}", loads.join(" ")));
        for k in 0..n {
            let o = Opnd::Vgpr(vd + k);
            let e = format!("g{t}_{k}");
            let e = if m.ends_with("_d16_hi") {
                self.touch(&o);
                format!("((({}) & 0xffffu) | (({e}) << 16u))", r(&o)?)
            } else if m.ends_with("_d16") {
                self.touch(&o);
                format!("((({}) & 0xffff0000u) | (({e}) & 0xffffu))", r(&o)?)
            } else {
                e
            };
            self.wv(&o, &e)?;
        }
        Ok(())
    }
}
