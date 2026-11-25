use super::{f, r, semantic, sig_type, Emitter, XYZW};
use crate::meta::MeshInfo;
use crate::rdna2::{Insn, Opnd};
use crate::Stage;
use std::fmt::Write as _;

pub const GS_ALLOC_REQ: u32 = 9;
pub const EXP_PRIM: u32 = 20;
pub const MS_LDS_DWORDS: usize = 7167;

pub fn wave_size(insns: &[Insn]) -> u32 {
    let w64 = insns.iter().any(|i| {
        i.mnemonic == "v_mbcnt_hi_u32_b32"
            || i.mnemonic.ends_with("saveexec_b64")
            || (i.mnemonic.ends_with("_b64") && matches!(i.dst.first(), Some(Opnd::ExecLo)))
    });
    if w64 { 64 } else { 32 }
}

fn all_lanes(o: &Opnd) -> bool {
    matches!(o, Opnd::ConstInt(-1)) || matches!(o, Opnd::Literal(0xffff_ffff))
}

impl<'a> Emitter<'a> {
    pub(super) fn is_ms(&self) -> bool {
        self.ex.stage == Stage::Mesh
    }

    pub(super) fn mesh_info(&self) -> Result<MeshInfo, String> {
        self.ex
            .gs_info
            .and_then(|g| g.mesh)
            .filter(|m| m.stage == 13)
            .ok_or_else(|| "mesh shader without PSV0 mesh runtime info".to_string())
    }

    fn ms_threads(&self) -> Result<u32, String> {
        let m = self.mesh_info()?;
        crate::meta::mesh_thread_count_for(m.stage, m.num_threads)
    }

    fn ms_gi_expr(&self) -> Result<&'static str, String> {
        Ok(if self.ms_threads()? > 128 { "tb_gi" } else { "gi" })
    }

    pub(super) fn mesh_mbcnt(&self, ins: &Insn, b: String) -> Result<String, String> {
        if !all_lanes(&ins.src[0]) {
            return Err(format!("{} with a mask other than all lanes in a mesh shader at {:#x}", ins.mnemonic, ins.offset));
        }
        let w = wave_size(&self.insns);
        let gi = self.ms_gi_expr()?;
        Ok(match (ins.mnemonic.as_str(), w) {
            ("v_mbcnt_lo_u32_b32", 64) => format!("(min({gi} & 63u, 32u) + {b})"),
            ("v_mbcnt_lo_u32_b32", _) => format!("(({gi} & 31u) + {b})"),
            ("v_mbcnt_hi_u32_b32", 64) => format!("(max({gi} & 63u, 32u) - 32u + {b})"),
            _ => b,
        })
    }

    pub(super) fn mesh_readfirstlane(&self, src: &Opnd) -> Result<Option<String>, String> {
        if !self.is_ms() || !matches!(src, Opnd::Vgpr(5)) {
            return Ok(None);
        }
        let written = self.insns[..self.cur_idx.min(self.insns.len())].iter().any(|i| i.dst.iter().any(|d| matches!(d, Opnd::Vgpr(5))));
        if written {
            return Ok(None);
        }
        let w = wave_size(&self.insns);
        let gi = self.ms_gi_expr()?;
        Ok(Some(format!("(gid.x * {}u + ({gi} & {:#x}u))", self.ms_threads()?, !(w - 1))))
    }

    fn uniform_entry_allocation(&self) -> bool {
        let [load, send, tail @ ..] = self.insns.as_slice() else { return false };
        load.mnemonic == "s_mov_b32" && load.dst.first() == Some(&Opnd::M0)
            && matches!(load.src.first(), Some(Opnd::Literal(_) | Opnd::ConstInt(_)))
            && send.mnemonic == "s_sendmsg" && send.aux.imm == i64::from(GS_ALLOC_REQ)
            && !tail.iter().any(|i| i.mnemonic.starts_with("s_branch")
                || i.mnemonic.starts_with("s_cbranch") || i.mnemonic == "s_sendmsg")
    }

    pub(super) fn mesh_sendmsg(&mut self, ins: &Insn) -> Result<(), String> {
        let msg = ins.aux.imm as u32 & 0xf;
        if msg != GS_ALLOC_REQ {
            return Err(format!("s_sendmsg message {msg} in a mesh shader unsupported"));
        }
        self.line("if (exec_lo & 1u) tb_mscnt = m0;");
        Ok(())
    }

    pub(super) fn mesh_barrier(&mut self) -> Result<(), String> {
        if self.brace_depth == 0 && self.loop_depth == 0 {
            self.lds_w_pending = false;
            self.lds_r_pending = false;
            self.line("GroupMemoryBarrierWithGroupSync();");
            return Ok(());
        }
        use crate::rdna2::Enc;
        let writes = self.insns.iter().any(|i| {
            i.enc == Enc::Ds || (matches!(i.enc, Enc::Mubuf | Enc::Mtbuf | Enc::Flat | Enc::Mimg) && (i.mnemonic.contains("store") || i.mnemonic.contains("atomic") || i.mnemonic.contains('<')))
        });
        if writes {
            return Err("s_barrier on a divergent path of a mesh shader that writes LDS or memory".into());
        }
        Ok(())
    }

    fn distance_slots(&self) -> Vec<(usize, usize)> {
        let osig = &self.ex.output_sig;
        let mut out = Vec::new();
        for sv in [2u32, 3] {
            let mut els: Vec<usize> = (0..osig.len()).filter(|&i| osig[i].sysval == sv).collect();
            els.sort_by_key(|&i| (osig[i].register, osig[i].mask.trailing_zeros()));
            for ei in els {
                for n in 0..osig[ei].mask.count_ones() as usize {
                    out.push((ei, n));
                }
            }
        }
        out
    }

    pub(super) fn mesh_exp(&mut self, ins: &Insn) -> Result<bool, String> {
        let target = ins.aux.imm as u32;
        let en = ins.aux.mask;
        if target == EXP_PRIM {
            if en & 1 != 0 {
                let v = r(&ins.src[0])?;
                self.line(&format!("if (exec_lo & 1u) tb_prim = {v};"));
            }
            return Ok(true);
        }
        if (21..32).contains(&target) {
            return Err(format!("mesh export target {target} unsupported"));
        }
        if !(13..=15).contains(&target) {
            return Ok(false);
        }
        if self.ex.output_sig.iter().any(|e| matches!(e.sysval, 4 | 5)) {
            return Err("mesh shader with a layer/viewport output: position-export misc vector unsupported".into());
        }
        let slots = self.distance_slots();
        let k = (target - 13) as usize;
        for c in 0..4usize {
            if (en >> c) & 1 == 0 {
                continue;
            }
            let Some(&(ei, n)) = slots.get(4 * k + c) else {
                return Err(format!("position export {target}.{} has no clip/cull distance element", XYZW[c]));
            };
            let one = self.ex.output_sig[ei].mask.count_ones() == 1;
            let field = if one { format!("o.o{ei}") } else { format!("o.o{ei}.{}", XYZW[n]) };
            let v = f(&ins.src[c])?;
            self.line(&format!("if (exec_lo & 1u) {field} = {v};"));
        }
        Ok(true)
    }

    pub(super) fn finish_mesh(&mut self, mut h: String, static_init: &str) -> Result<String, String> {
        let m = self.mesh_info()?;
        let n = self.ms_threads()?;
        let passes: u32 = if n > 128 { 2 } else { 1 };
        if passes > 1 && self.uses_lds {
            return Err(format!("mesh shader with {n} threads and LDS unsupported"));
        }
        if m.prim_vectors != 0 && self.ex.prim_sig.is_empty() {
            return Err("mesh shader with per-primitive outputs unsupported without PSG1".into());
        }
        if self.ex.prim_sig.iter().any(|e| e.sysval != 0) {
            return Err("mesh shader with per-primitive system values unsupported".into());
        }
        let has_prim_attrs = !self.ex.prim_sig.is_empty();
        let pl = if m.payload_bytes != 0 { Some(self.amp_ms_payload(m.payload_bytes)?) } else { None };
        let (topo, idx_ty, nidx) = match m.topology {
            1 => ("line", "uint2", 2usize),
            2 => ("triangle", "uint3", 3usize),
            t => return Err(format!("mesh output topology {t} unsupported")),
        };
        let (mv, mp) = (m.max_vertices as u32, m.max_primitives as u32);
        if mv == 0 || mp == 0 {
            return Err(format!("mesh shader with max vertices {mv} / primitives {mp}"));
        }
        if mv > n || mp > n {
            return Err(format!("mesh shader exports {mv} vertices / {mp} primitives from {n} threads (multi-row export)"));
        }
        if self.ex.output_sig.is_empty() {
            return Err("mesh shader without vertex outputs".into());
        }
        if !self.insns.iter().any(|i| i.mnemonic == "s_sendmsg" && i.aux.imm as u32 & 0xf == GS_ALLOC_REQ) {
            return Err("mesh shader without s_sendmsg GS_ALLOC_REQ: no output allocation".into());
        }
        let lds_limit = MS_LDS_DWORDS * 4;
        if m.groupshared_bytes as usize > lds_limit {
            return Err(format!("mesh shader uses {} bytes of groupshared memory (PC limit {lds_limit})", m.groupshared_bytes));
        }
        if let Some(o) = self.insns.iter().filter(|i| i.enc == crate::rdna2::Enc::Ds).map(|i| i.aux.imm.max(0) as usize + 16).max() {
            if o > lds_limit {
                return Err(format!("mesh shader LDS offset {o} beyond the PC groupshared limit {lds_limit}"));
            }
        }
        if self.uses_lds {
            let decl = "groupshared uint tb_lds[8192];";
            if !h.contains(decl) {
                return Err("mesh shader LDS declaration not found".into());
            }
            h = h.replace(decl, &format!("groupshared uint tb_lds[{MS_LDS_DWORDS}];"));
        }
        if let Some(p) = &pl {
            h.insert_str(0, "#define TB_AMP_GMEM 1
");
            h.push_str(p);
        }
        let local_counts = self.uniform_entry_allocation() && !self.uses_lds;
        if !local_counts {
            let _ = writeln!(h, "groupshared uint tb_mscnt;");
        }
        let _ = writeln!(h, "struct SOut {{");
        for (ei, e) in self.ex.output_sig.iter().enumerate() {
            let (ty, _) = sig_type(e);
            if e.sysval == 1 {
                let _ = writeln!(h, "    precise {ty} o{ei} : {};", semantic(e));
            } else {
                let interp = self.out_interp.for_output(e).map(|m| format!("{m} ")).unwrap_or_default();
                let _ = writeln!(h, "    {interp}{ty} o{ei} : {};", semantic(e));
            }
        }
        let _ = writeln!(h, "}};");
        if !has_prim_attrs {
            let _ = writeln!(h, "struct SPrim {{ bool cull : SV_CullPrimitive; }};");
        } else {
            let _ = writeln!(h, "struct SPrim {{");
            let _ = writeln!(h, "    bool cull : SV_CullPrimitive;");
            for (ei, e) in self.ex.prim_sig.iter().enumerate() {
                let (ty, _) = sig_type(e);
                let _ = writeln!(h, "    {ty} p{ei} : {};", semantic(e));
            }
            let _ = writeln!(h, "}};");
        }
        let pc_n = n / passes;
        let [tx, ty_, tz] = if passes == 1 { m.num_threads } else { [pc_n, 1, 1] };
        let _ = writeln!(h, "[numthreads({tx}, {ty_}, {tz})]");
        let _ = writeln!(h, "[outputtopology(\"{topo}\")]");
        let _ = writeln!(
            h,
            "void main(uint gi : SV_GroupIndex, uint3 gid : SV_GroupID{}, out vertices SOut verts[{mv}], out indices {idx_ty} tris[{mp}], out primitives SPrim prims[{mp}]) {{",
            if pl.is_some() { ", in payload TbPayload tb_pl" } else { "" }
        );
        for reg in ["v5", "s3"] {
            self.regs.insert(reg.to_string());
        }
        if pl.is_some() {
            self.amp_ms_regs();
        }
        let w = wave_size(&self.insns);
        let prim_indices = |expr: &str| {
            let parts = [format!("{expr} & 0x1ffu"), format!("({expr} >> 10) & 0x1ffu"), format!("({expr} >> 20) & 0x1ffu")];
            parts[..nidx].join(", ")
        };
        let tg = n.div_ceil(w);
        let wave_info = |gi: &str| format!("s3 = {:#x}u | (({gi} / {w}u) << 24) | (min({w}u, {n}u - ({gi} & {:#x}u)) * 0x101u);", tg << 28, !(w - 1));
        if passes == 1 {
            let _ = writeln!(h, "    uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
            let _ = writeln!(h, "{}", self.reg_decls());
            h.push_str(&self.ud_init());
            h.push_str(static_init);
            if has_prim_attrs {
                let _ = writeln!(h, "    SOut o = (SOut)0; uint tb_prim = 0u; SPrim tb_pout = (SPrim)0;");
            } else {
                let _ = writeln!(h, "    SOut o = (SOut)0; uint tb_prim = 0u;");
            }
            if local_counts {
                let _ = writeln!(h, "    uint tb_mscnt = 0u;");
            } else {
                let _ = writeln!(h, "    if (gi == 0u) tb_mscnt = 0u;");
                let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
            }
            let _ = writeln!(h, "    v5 = gid.x * {n}u + gi; {}", wave_info("gi"));
            if pl.is_some() {
                h.push_str(&self.amp_ms_prologue(m.payload_bytes));
            }
            h.push_str(&self.body);
            if !local_counts {
                let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
            }
            let _ = writeln!(h, "    uint tb_nv = tb_mscnt & 0x3ffu, tb_np = (tb_mscnt >> 12) & 0x7ffu;");
            let _ = writeln!(h, "    SetMeshOutputCounts(tb_nv, tb_np);");
            let _ = writeln!(h, "    if (gi < tb_nv) verts[gi] = o;");
            if has_prim_attrs {
                let _ = writeln!(h, "    if (gi < tb_np) {{ tris[gi] = {idx_ty}({}); prims[gi] = tb_pout; prims[gi].cull = (tb_prim >> 31) != 0u; }}", prim_indices("tb_prim"));
            } else {
                let _ = writeln!(h, "    if (gi < tb_np) {{ tris[gi] = {idx_ty}({}); prims[gi].cull = (tb_prim >> 31) != 0u; }}", prim_indices("tb_prim"));
            }
        } else {
            let _ = writeln!(h, "    SOut tb_vo[2]; uint tb_pr[2];");
            if has_prim_attrs {
                let _ = writeln!(h, "    SPrim tb_po[2];");
            }
            if local_counts {
                let _ = writeln!(h, "    uint tb_mscnt = 0u;");
            } else {
                let _ = writeln!(h, "    if (gi == 0u) tb_mscnt = 0u;");
                let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
            }
            let _ = writeln!(h, "    for (uint tb_iter = 0u; tb_iter < 2u; tb_iter++) {{");
            let _ = writeln!(h, "        uint tb_gi = gi + tb_iter * {pc_n}u;");
            let _ = writeln!(h, "        uint exec_lo = 1u, exec_hi = 0u, vcc_lo = 0u, vcc_hi = 0u, scc = 0u, m0 = 0u, nul = 0u;");
            let _ = writeln!(h, "{}", self.reg_decls());
            h.push_str(&self.ud_init());
            h.push_str(static_init);
            if has_prim_attrs {
                let _ = writeln!(h, "        SOut o = (SOut)0; uint tb_prim = 0u; SPrim tb_pout = (SPrim)0;");
            } else {
                let _ = writeln!(h, "        SOut o = (SOut)0; uint tb_prim = 0u;");
            }
            let _ = writeln!(h, "        v5 = gid.x * {n}u + tb_gi; {}", wave_info("tb_gi"));
            if pl.is_some() {
                h.push_str(&self.amp_ms_prologue(m.payload_bytes));
            }
            h.push_str(&self.body);
            let _ = writeln!(h, "        tb_vo[tb_iter] = o; tb_pr[tb_iter] = tb_prim;");
            if has_prim_attrs {
                let _ = writeln!(h, "        tb_po[tb_iter] = tb_pout;");
            }
            let _ = writeln!(h, "    }}");
            if !local_counts {
                let _ = writeln!(h, "    GroupMemoryBarrierWithGroupSync();");
            }
            let _ = writeln!(h, "    uint tb_nv = tb_mscnt & 0x3ffu, tb_np = (tb_mscnt >> 12) & 0x7ffu;");
            let _ = writeln!(h, "    SetMeshOutputCounts(tb_nv, tb_np);");
            let _ = writeln!(h, "    if (gi < tb_nv) verts[gi] = tb_vo[0];");
            if mv > pc_n {
                let _ = writeln!(h, "    if (gi + {pc_n}u < tb_nv) verts[gi + {pc_n}u] = tb_vo[1];");
            }
            if has_prim_attrs {
                let _ = writeln!(h, "    if (gi < tb_np) {{ tris[gi] = {idx_ty}({}); prims[gi] = tb_po[0]; prims[gi].cull = (tb_pr[0] >> 31) != 0u; }}", prim_indices("tb_pr[0]"));
                if mp > pc_n {
                    let _ = writeln!(h, "    if (gi + {pc_n}u < tb_np) {{ tris[gi + {pc_n}u] = {idx_ty}({}); prims[gi + {pc_n}u] = tb_po[1]; prims[gi + {pc_n}u].cull = (tb_pr[1] >> 31) != 0u; }}", prim_indices("tb_pr[1]"));
                }
            } else {
                let _ = writeln!(h, "    if (gi < tb_np) {{ tris[gi] = {idx_ty}({}); prims[gi].cull = (tb_pr[0] >> 31) != 0u; }}", prim_indices("tb_pr[0]"));
                if mp > pc_n {
                    let _ = writeln!(h, "    if (gi + {pc_n}u < tb_np) {{ tris[gi + {pc_n}u] = {idx_ty}({}); prims[gi + {pc_n}u].cull = (tb_pr[1] >> 31) != 0u; }}", prim_indices("tb_pr[1]"));
                }
            }
        }
        h.push_str("}\n");
        Ok(h)
    }
}
