use super::{r, Emitter};
use crate::rdna2::{Insn, Opnd};

pub const DWORDS: u32 = 1024;

pub const HELPERS: &str = r#"static uint tb_scr[SCRATCH_DWORDS];
uint tb_sbyte(uint a) { return a < SCRATCH_DWORDS * 4u ? (tb_scr[a >> 2u] >> ((a & 3u) * 8u)) & 0xffu : 0u; }
void tb_sput(uint a, uint v) { if (a < SCRATCH_DWORDS * 4u) { uint s = (a & 3u) * 8u; tb_scr[a >> 2u] = (tb_scr[a >> 2u] & ~(0xffu << s)) | ((v & 0xffu) << s); } }
uint tb_sload(uint a, uint n) {
    if (n == 4u && (a & 3u) == 0u) return a < SCRATCH_DWORDS * 4u ? tb_scr[a >> 2u] : 0u;
    uint v = 0u;
    for (uint i = 0u; i < n; i++) v |= tb_sbyte(a + i) << (i * 8u);
    return v;
}
void tb_sstore(uint a, uint v, uint n) {
    if (n == 4u && (a & 3u) == 0u) { if (a < SCRATCH_DWORDS * 4u) tb_scr[a >> 2u] = v; return; }
    for (uint i = 0u; i < n; i++) tb_sput(a + i, v >> (i * 8u));
}
"#;

pub fn helpers() -> String {
    HELPERS.replace("SCRATCH_DWORDS", &format!("{DWORDS}u"))
}

fn width(m: &str) -> Option<(bool, u8, u8, bool)> {
    Some(match m {
        "flat_load_ubyte" => (false, 1, 1, false),
        "flat_load_sbyte" => (false, 1, 1, true),
        "flat_load_ushort" => (false, 1, 2, false),
        "flat_load_sshort" => (false, 1, 2, true),
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

impl<'a> Emitter<'a> {
    pub(super) fn emit_scratch(&mut self, ins: &Insn) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        if ins.raw.first().is_some_and(|w| (w >> 13) & 1 != 0) {
            return Err(format!("{m}: LDS=1 (memory <-> LDS transfer) is not lowered"));
        }
        let (store, n, bytes, signed) = width(m).ok_or_else(|| format!("{m}: scratch operation unsupported"))?;
        let base = match ins.src.get(2) {
            Some(Opnd::Sgpr(s)) => {
                let o = Opnd::Sgpr(*s);
                self.touch(&o);
                r(&o)?
            }
            Some(Opnd::Null) | None => {
                let o = match ins.src.first() {
                    Some(Opnd::Vgpr(v)) => Opnd::Vgpr(*v),
                    _ => return Err(format!("{m}: scratch access without a VGPR or SGPR address")),
                };
                self.touch(&o);
                r(&o)?
            }
            Some(o) => return Err(format!("{m}: scratch address operand {o:?} unsupported")),
        };
        self.uses_scratch = true;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint sa{t} = {base} + {:#x}u;", ins.aux.imm as i32 as u32));
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
                body.push(format!("tb_sstore(sa{t} + {}u, {v}, {bytes}u);", 4 * k as u32));
            }
            self.line(&format!("if (exec_lo & 1u) {{ {} }}", body.join(" ")));
            return Ok(());
        }
        let vd = match ins.dst.first() {
            Some(Opnd::Vgpr(v)) => *v,
            _ => return Err(format!("{m}: bad vdst")),
        };
        let decl: Vec<String> = (0..n).map(|k| format!("sv{t}_{k} = 0u")).collect();
        self.line(&format!("uint {};", decl.join(", ")));
        let loads: Vec<String> = (0..n)
            .map(|k| {
                let raw = format!("tb_sload(sa{t} + {}u, {bytes}u)", 4 * k as u32);
                let v = match (bytes, signed) {
                    (1, true) => format!("(uint)(((int)({raw} << 24u)) >> 24u)"),
                    (2, true) => format!("(uint)(((int)({raw} << 16u)) >> 16u)"),
                    _ => raw,
                };
                format!("sv{t}_{k} = {v};")
            })
            .collect();
        self.line(&format!("if (exec_lo & 1u) {{ {} }}", loads.join(" ")));
        for k in 0..n {
            self.wv(&Opnd::Vgpr(vd + k), &format!("sv{t}_{k}"))?;
        }
        Ok(())
    }
}
