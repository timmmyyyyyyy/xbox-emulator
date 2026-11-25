use super::{name, Emitter};
use crate::meta::MeshInfo;
use crate::rdna2::{Enc, Insn, Opnd};
use crate::Stage;
use std::fmt::Write as _;

pub const ARGS_HI: u32 = 0x10;
pub const PAY_HI: u32 = 0x20;
pub const MS_PAY_HI: u32 = 0x30;
pub const MAX_PAYLOAD: u32 = 16384;
pub const GLOBAL_SPACE: u32 = 4_000_003;

pub fn payload_dwords(bytes: u32) -> usize {
    (((bytes + 15) & !15) / 4) as usize
}

const VA_HELPERS: &str = r#"uint2 tb_va(uint lo, uint hi, uint off, uint offhi) { uint l = lo + off; return uint2(l, hi + offhi + (l < lo ? 1u : 0u)); }
#ifndef TB_GMEM
RWByteAddressBuffer tb_gfault : register(u2, spaceTB_GLOBAL_SPACE);
void tb_gmiss(uint2 va) { uint o; tb_gfault.InterlockedAdd(0, 1u, o); if (o == 0u) tb_gfault.Store2(4, va); }
uint tb_gmem_load(uint2 va, uint nbytes) { tb_gmiss(va); return 0u; }
void tb_gmem_store(uint2 va, uint v) { tb_gmiss(va); }
void tb_gmem_store_n(uint2 va, uint v, uint n) { tb_gmiss(va); }
#endif
uint tb_gload(uint2 va, uint nbytes) {
    uint s = (va.x & 3u) * 8u;
    uint2 a = uint2(va.x & ~3u, va.y);
    int k = tb_ring(a);
    if (k == -1000) return tb_gmem_load(va, nbytes);
    uint lo = tb_ring_get(k);
    if ((va.x & 3u) + nbytes <= 4u) return lo >> s;
    int k2 = tb_ring(tb_va(a.x, a.y, 4u, 0u));
    uint hi = k2 == -1000 ? tb_gmem_load(tb_va(a.x, a.y, 4u, 0u), 4u) : tb_ring_get(k2);
    return (lo >> s) | (hi << (32u - s));
}
void tb_gstore_m(uint2 a, uint v, uint m) {
    int k = tb_ring(a);
    if (k == -1000) {
        if (m == 0xffffffffu) tb_gmem_store(a, v);
        else { uint s = firstbitlow(m); tb_gmem_store_n(tb_va(a.x, a.y, s >> 3, 0u), v >> s, countbits(m) >> 3); }
        return;
    }
    tb_ring_put(k, v, m);
}
void tb_gstore_n(uint2 va, uint v, uint n) {
    uint s = (va.x & 3u) * 8u;
    uint m = n >= 4u ? 0xffffffffu : ((1u << (n * 8u)) - 1u);
    uint2 a = uint2(va.x & ~3u, va.y);
    if (m << s != 0u) tb_gstore_m(a, v << s, m << s);
    if (s != 0u && (m >> (32u - s)) != 0u) tb_gstore_m(tb_va(a.x, a.y, 4u, 0u), v >> (32u - s), m >> (32u - s));
}
void tb_gstore(uint2 va, uint v) { tb_gstore_n(va, v, 4u); }
"#;

impl<'a> Emitter<'a> {
    pub(super) fn is_as(&self) -> bool {
        self.ex.stage == Stage::Amplification
    }

    fn amp_info(&self) -> Result<MeshInfo, String> {
        self.ex
            .gs_info
            .and_then(|g| g.mesh)
            .filter(|m| m.stage == 14)
            .ok_or_else(|| "amplification shader without PSV0 amplification runtime info".to_string())
    }

    fn ms_payload_bytes(&self) -> u32 {
        if self.ex.stage != Stage::Mesh {
            return 0;
        }
        self.ex.gs_info.and_then(|g| g.mesh).filter(|m| m.stage == 13).map_or(0, |m| m.payload_bytes)
    }

    fn amp_threads(&self, m: &MeshInfo) -> Result<[u32; 3], String> {
        if m.num_threads != [0; 3] {
            return Ok(m.num_threads);
        }
        let f = &self.ex.fssm;
        let w = |o: usize| f.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let blk = self.ex.isa_offset_in_fssm.checked_sub(0x40).ok_or("amplification shader: no FSSM register block before the ISA")?;
        let stride = ((m.payload_bytes + 15) & !15).max(16);
        match (w(blk + 4), w(blk + 8), w(blk + 12), w(blk + 0x14), w(blk + 0x18)) {
            (Some(x), Some(y), Some(z), Some(ps), Some(16)) if ps == stride && (1..=128).contains(&x) && (1..=128).contains(&y) && (1..=64).contains(&z) => Ok([x, y, z]),
            v => Err(format!("amplification shader without thread-group size: PSV0 has none and the FSSM register block at {blk:#x} does not validate ({v:?})")),
        }
    }

    pub(super) fn amp_flat(&self, ins: &Insn) -> bool {
        if ins.enc != Enc::Flat || !(self.is_as() || self.ms_payload_bytes() != 0) {
            return false;
        }
        !(matches!(ins.op & 0x7f, 12..=15) && self.insns.iter().any(|i| i.mnemonic == "s_getpc_b64"))
    }

    pub(super) fn emit_amp_flat(&mut self, ins: &Insn) -> Result<(), String> {
        let op = ins.op & 0x7f;
        let seg = ins.aux.mask;
        if seg == 1 {
            return Err(format!("scratch-segment access {} at {:#x} in an amplification/mesh shader", ins.mnemonic, ins.offset));
        }
        let s = &ins.src;
        let a = match s.first() {
            Some(Opnd::Vgpr(v)) => *v,
            _ => return Err("flat: bad vaddr".into()),
        };
        for k in 0..2 {
            self.touch(&Opnd::Vgpr(a + k));
        }
        let (lo, hi) = match s.get(2) {
            Some(Opnd::Null) | None => (name(&Opnd::Vgpr(a))?, name(&Opnd::Vgpr(a + 1))?),
            Some(Opnd::Sgpr(n)) if seg == 2 => {
                self.touch(&Opnd::Sgpr(*n));
                self.touch(&Opnd::Sgpr(n + 1));
                let t = self.tmp;
                self.tmp += 1;
                self.line(&format!("uint2 tg{t} = tb_va(s{n}, s{}, v{a}, 0u);", n + 1));
                (format!("tg{t}.x"), format!("tg{t}.y"))
            }
            Some(o) => return Err(format!("flat: unsupported saddr {o:?}")),
        };
        let imm = ins.aux.imm as i32;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint2 ta{t} = tb_va({lo}, {hi}, {:#x}u, {}u);", imm as u32, if imm < 0 { "0xffffffff" } else { "0x0" }));
        let at = |k: u32| if k == 0 { format!("ta{t}") } else { format!("tb_va(ta{t}.x, ta{t}.y, {}u, 0u)", 4 * k) };
        let data = |k: u8| -> Result<String, String> {
            match s.get(1) {
                Some(Opnd::Vgpr(d)) => Ok(format!("v{}", d + k)),
                _ => Err("flat: bad vdata".into()),
            }
        };
        match op {
            12..=15 => {
                let n: u32 = match op {
                    12 => 1,
                    13 => 2,
                    14 => 4,
                    _ => 3,
                };
                let vd = match ins.dst.first() {
                    Some(Opnd::Vgpr(v)) => *v,
                    _ => return Err("flat: bad vdst".into()),
                };
                for k in 0..n {
                    self.line(&format!("uint tl{t}_{k} = (exec_lo & 1u) ? tb_gload({}, 4u) : 0u;", at(k)));
                }
                for k in 0..n {
                    self.wv(&Opnd::Vgpr(vd + k as u8), &format!("tl{t}_{k}"))?;
                }
            }
            8..=11 => {
                let vd = ins.dst.first().cloned().ok_or("flat: bad vdst")?;
                let bytes = if op < 10 { 1 } else { 2 };
                let raw = format!("((exec_lo & 1u) ? tb_gload(ta{t}, {bytes}u) : 0u)");
                let e = match op {
                    8 => format!("({raw} & 0xffu)"),
                    9 => format!("uint((int({raw} << 24u)) >> 24)"),
                    10 => format!("({raw} & 0xffffu)"),
                    _ => format!("uint((int({raw} << 16u)) >> 16)"),
                };
                self.line(&format!("uint tl{t}_0 = {e};"));
                self.wv(&vd, &format!("tl{t}_0"))?;
            }
            28..=31 => {
                let n: u32 = match op {
                    28 => 1,
                    29 => 2,
                    30 => 4,
                    _ => 3,
                };
                for k in 0..n {
                    let v = data(k as u8)?;
                    if let Some(Opnd::Vgpr(d)) = s.get(1) {
                        self.touch(&Opnd::Vgpr(d + k as u8));
                    }
                    self.line(&format!("if (exec_lo & 1u) tb_gstore({}, {v});", at(k)));
                }
            }
            24..=27 => {
                let v = data(0)?;
                if let Some(o) = s.get(1) {
                    self.touch(o);
                }
                let (e, nb) = match op {
                    24 => (format!("({v} & 0xffu)"), 1),
                    25 => (format!("(({v} >> 16u) & 0xffu)"), 1),
                    26 => (format!("({v} & 0xffffu)"), 2),
                    _ => (format!("({v} >> 16u)"), 2),
                };
                self.line(&format!("if (exec_lo & 1u) tb_gstore_n(ta{t}, {e}, {nb}u);"));
            }
            _ => return Err(format!("{} (flat op {op:#x}) in an amplification/mesh shader unsupported", ins.mnemonic)),
        }
        Ok(())
    }

    fn ring_helpers(&self, n: usize, ms: bool) -> String {
        let stride = n * 4;
        let mut h = String::new();
        if ms {
            let _ = writeln!(h, "static uint tb_plw[{n}];");
            let _ = writeln!(h, "int tb_ring(uint2 va) {{ if (va.y == {MS_PAY_HI:#x}u && va.x < {stride}u) return int(va.x >> 2); return -1000; }}");
            let _ = writeln!(h, "uint tb_ring_get(int k) {{ return tb_plw[k]; }}");
            let _ = writeln!(h, "void tb_ring_put(int k, uint v, uint m) {{ tb_plw[k] = (tb_plw[k] & ~m) | (v & m); }}");
        } else {
            let _ = writeln!(h, "struct TbPayload {{ uint w[{n}]; }};");
            let _ = writeln!(h, "groupshared TbPayload tb_pl;");
            let _ = writeln!(h, "groupshared uint tb_payload_ring[{n}];");
            let _ = writeln!(h, "groupshared uint tb_args[4];");
            let _ = writeln!(h, "static uint tb_gidx;");
            let _ = writeln!(
                h,
                "int tb_ring(uint2 va) {{ uint b = tb_gidx * {stride}u; if (va.y == {PAY_HI:#x}u && va.x >= b && va.x - b < {stride}u) return int((va.x - b) >> 2); b = tb_gidx * 16u; if (va.y == {ARGS_HI:#x}u && va.x >= b && va.x - b < 16u) return -1 - int((va.x - b) >> 2); return -1000; }}"
            );
            let _ = writeln!(h, "uint tb_ring_get(int k) {{ return k >= 0 ? tb_payload_ring[k] : tb_args[-1 - k]; }}");
            let _ = writeln!(
                h,
                "void tb_ring_put(int k, uint v, uint m) {{ if (m == 0xffffffffu) {{ if (k >= 0) tb_payload_ring[k] = v; else tb_args[-1 - k] = v; return; }} uint o; if (k >= 0) {{ InterlockedAnd(tb_payload_ring[k], ~m, o); InterlockedOr(tb_payload_ring[k], v & m, o); }} else {{ InterlockedAnd(tb_args[-1 - k], ~m, o); InterlockedOr(tb_args[-1 - k], v & m, o); }} }}"
            );
        }
        h.push_str(&VA_HELPERS.replace("TB_GLOBAL_SPACE", &GLOBAL_SPACE.to_string()));
        h
    }

    pub(super) fn amp_ms_payload(&self, bytes: u32) -> Result<String, String> {
        if bytes > MAX_PAYLOAD {
            return Err(format!("mesh shader payload of {bytes} bytes exceeds the D3D12 limit {MAX_PAYLOAD}"));
        }
        let n = payload_dwords(bytes);
        Ok(format!("struct TbPayload {{ uint w[{n}]; }};\n{}", self.ring_helpers(n, true)))
    }

    pub(super) fn amp_ms_regs(&mut self) {
        for reg in ["s10", "s11"] {
            self.regs.insert(reg.to_string());
        }
    }

    pub(super) fn amp_ms_prologue(&self, bytes: u32) -> String {
        let n = payload_dwords(bytes);
        format!("    for (uint tb_k = 0u; tb_k < {n}u; tb_k++) tb_plw[tb_k] = tb_pl.w[tb_k];\n    s10 = {:#x}u; s11 = 0u;\n", MS_PAY_HI >> 4)
    }

    pub(super) fn finish_amp(&mut self, mut h: String, static_init: &str) -> Result<String, String> {
        let m = self.amp_info()?;
        if m.payload_bytes > MAX_PAYLOAD {
            return Err(format!("amplification payload of {} bytes exceeds the D3D12 limit {MAX_PAYLOAD}", m.payload_bytes));
        }
        let [tx, ty, tz] = self.amp_threads(&m)?;
        let threads = crate::meta::mesh_thread_count([tx, ty, tz])?;
        let n = payload_dwords(m.payload_bytes.max(4));
        h.insert_str(0, "#define TB_AMP_GMEM 1\n");
        h.push_str(&self.ring_helpers(n, false));
        let _ = writeln!(h, "[numthreads({tx}, {ty}, {tz})]");
        let _ = writeln!(h, "void main(uint3 gid : SV_GroupID, uint3 gtid : SV_GroupThreadID, uint gi : SV_GroupIndex) {{");
        let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
        for reg in ["v0", "v1", "v2", "s2", "s3", "s4"] {
            self.regs.insert(reg.to_string());
        }
        let _ = writeln!(h, "{}", self.reg_decls());
        h.push_str(&self.ud_init());
        h.push_str(static_init);
        let _ = writeln!(h, "    v0 = gtid.x; v1 = gtid.y; v2 = gtid.z;");
        let _ = writeln!(h, "    s2 = {:#x}u; s3 = {:#x}u; s4 = gid.x; tb_gidx = gid.x;", ARGS_HI << 24, PAY_HI << 24);
        let _ = writeln!(h, "    for (uint tb_k = gi; tb_k < 4u; tb_k += {threads}u) tb_args[tb_k] = 0u;");
        let _ = writeln!(h, "    for (uint tb_k = gi; tb_k < {n}u; tb_k += {threads}u) tb_payload_ring[tb_k] = 0u;");
        let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
        h.push_str(&self.body);
        let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
        let _ = writeln!(h, "    for (uint tb_k = gi; tb_k < {n}u; tb_k += {threads}u) tb_pl.w[tb_k] = tb_payload_ring[tb_k];");
        let _ = writeln!(h, "    DispatchMesh(tb_args[0], max(tb_args[1], 1u), 1u, tb_pl);");
        h.push_str("}\n");
        Ok(h)
    }
}
