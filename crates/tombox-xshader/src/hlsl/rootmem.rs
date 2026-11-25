use super::{i32_result_of_64, sreg_num, sreg_opnd, Emitter, Sym, BINDLESS};
use crate::meta::RootParamKind;
use crate::rdna2::{Enc, Insn, Opnd};
use crate::Stage;
use std::fmt::Write as _;

pub(super) const RM_TABLE: u8 = 0x80;

fn sgpr(o: Option<&Opnd>) -> Option<u8> {
    match o {
        Some(Opnd::Sgpr(n)) => Some(*n),
        _ => None,
    }
}

impl Emitter<'_> {
    pub(super) fn rootmem(&self) -> bool {
        if self.dg().is_some() {
            return self.dg_has_mem();
        }
        if let Some(v) = self.rm_mode.get() {
            return v;
        }
        let v = self.detect_rootmem();
        self.rm_mode.set(Some(v));
        v
    }

    fn detect_rootmem(&self) -> bool {
        if !self.is_cs || self.ex.root_sig.as_ref().map_or(true, |r| r.params.is_empty()) {
            return false;
        }
        if self.ex.stage == Stage::Amplification {
            return true;
        }
        let mut ptr: Vec<u8> = vec![0];
        for ins in &self.insns {
            match ins.enc {
                Enc::Smem if ins.op <= 4 => {
                    if sgpr(ins.src.first()).is_some_and(|b| ptr.contains(&b)) {
                        return true;
                    }
                }
                Enc::Sop1 if ins.mnemonic == "s_mov_b32" => {
                    let d = sgpr(ins.dst.first());
                    let s = sgpr(ins.src.first());
                    if let Some(d) = d {
                        ptr.retain(|x| *x != d);
                        if s.is_some_and(|s| ptr.contains(&s)) {
                            ptr.push(d);
                        }
                    }
                    continue;
                }
                Enc::Sopp if ins.mnemonic.starts_with("s_branch") || ins.mnemonic.starts_with("s_cbranch") || ins.mnemonic == "s_endpgm" => return false,
                _ => {}
            }
            if let Some(d) = sgpr(ins.dst.first()) {
                ptr.retain(|x| *x != d);
            }
        }
        false
    }

    pub(super) fn rm_ptr(&self, table: u8) -> bool {
        if self.dg().is_some() {
            return table == 0 && self.dg_has_mem();
        }
        (table as usize) < self.user_base() && self.rootmem()
    }

    fn rm_param(&self, dword: u32) -> Option<(usize, u32)> {
        if self.dg().is_some() {
            return self.dg_mem(dword);
        }
        let mut at = dword;
        for (i, p) in self.ex.root_sig.as_ref()?.params.iter().enumerate() {
            let n = match &p.kind {
                RootParamKind::Table(_) => return None,
                RootParamKind::Descriptor { .. } => 2,
                RootParamKind::Constants { num32, .. } => *num32,
            };
            if at < n {
                return Some((i, at));
            }
            at -= n;
        }
        None
    }

    fn rm_kind_of(&self, p: usize) -> Option<&RootParamKind> {
        self.ex.root_sig.as_ref()?.params.get(p).map(|x| &x.kind)
    }

    pub(super) fn rm_desc(&self, s: Option<&Sym>) -> Option<Option<Vec<(u8, u32)>>> {
        let Some(Sym::Desc { table, off, k }) = s else { return None };
        if !self.rm_ptr(*table) {
            return None;
        }
        if *off & BINDLESS != 0 || off % 4 != 0 {
            return Some(None);
        }
        let Some((p, 0)) = self.rm_param(off / 4 + k) else { return Some(None) };
        match self.rm_kind_of(p) {
            Some(RootParamKind::Descriptor { .. }) if p < RM_TABLE as usize => Some(Some(vec![(RM_TABLE | p as u8, 0)])),
            _ => Some(None),
        }
    }

    pub(super) fn rm_table_kind(&self, table: u8) -> Option<Option<u32>> {
        if table < RM_TABLE {
            return None;
        }
        Some(match self.rm_kind_of((table & !RM_TABLE) as usize) {
            Some(RootParamKind::Table(_)) => return None,
            Some(RootParamKind::Descriptor { kind: 2, .. }) => Some(2),
            Some(RootParamKind::Descriptor { kind: 3, .. }) => Some(0),
            Some(RootParamKind::Descriptor { kind: 4, .. }) => Some(1),
            _ => None,
        })
    }

    pub(super) fn rm_bind(&self, table: u8) -> Option<u32> {
        if table < RM_TABLE {
            return None;
        }
        match self.rm_kind_of((table & !RM_TABLE) as usize) {
            Some(RootParamKind::Descriptor { register, space, .. }) => Some((space << 16) | register),
            Some(RootParamKind::Table(_)) => None,
            _ => Some(u32::MAX),
        }
    }

    pub(super) fn rm_note_load(&mut self, idx: usize, ins: &Insn, table: u8) -> Result<(), String> {
        if !self.rm_ptr(table) {
            return Ok(());
        }
        if (ins.raw.get(1).copied().unwrap_or(0) >> 25) != 0x7d {
            return Err(format!("s_load at {:#x}: root-memory read with a dynamic soffset", ins.offset));
        }
        let off = ins.aux.imm as u32;
        if off % 4 != 0 {
            return Err(format!("s_load at {:#x}: unaligned root-memory offset {off:#x}", ins.offset));
        }
        let n = 1u32 << ins.op;
        let base = ins.dst.first().and_then(sreg_num).ok_or("s_load: bad dst")? as u32;
        for k in 0..n {
            if self.rm_param(off / 4 + k).is_none() && !self.sgpr_dead_after(idx, base + k) {
                return Err(format!("s_load at {:#x}: root-memory dword {} is outside the root signature and s{} is read before it is rewritten", ins.offset, off / 4 + k, base + k));
            }
        }
        self.rm_loads.insert(idx, off);
        Ok(())
    }

    pub(super) fn sgpr_dead_after(&self, idx: usize, r: u32) -> bool {
        let reads = |ins: &Insn| {
            let span = match ins.enc {
                Enc::Smem | Enc::Mubuf | Enc::Mtbuf => 4,
                Enc::Mimg => 8,
                Enc::Vop1 | Enc::Vop2 | Enc::Vopc | Enc::Sop1 | Enc::Sop2 | Enc::Sopc | Enc::Sopk if !ins.mnemonic.contains("64") => 1,
                _ => 2,
            };
            ins.src.iter().any(|o| matches!(o, Opnd::Sgpr(n) if (*n as u32..*n as u32 + span).contains(&r)))
        };
        let writes = |ins: &Insn| {
            let extra = match ins.enc {
                Enc::Smem if ins.op <= 4 => (1u32 << ins.op) - 1,
                _ if ins.mnemonic.ends_with("_b64") && !i32_result_of_64(&ins.mnemonic) => 1,
                _ => 0,
            };
            ins.dst.iter().any(|o| matches!(o, Opnd::Sgpr(n) if (*n as u32..=*n as u32 + extra).contains(&r)))
        };
        let at: std::collections::HashMap<usize, usize> = self.insns.iter().enumerate().map(|(i, x)| (x.offset, i)).collect();
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![idx + 1];
        while let Some(i) = todo.pop() {
            if !seen.insert(i) {
                continue;
            }
            let Some(ins) = self.insns.get(i) else { return false };
            if reads(ins) {
                return false;
            }
            if ins.mnemonic.starts_with("s_setpc") || ins.mnemonic.starts_with("s_swappc") || ins.mnemonic.starts_with("s_call") {
                return false;
            }
            let jump = ins.enc == Enc::Sopp && (ins.mnemonic == "s_branch" || ins.mnemonic.starts_with("s_cbranch"));
            if jump {
                let simm = (ins.raw.first().copied().unwrap_or(0) & 0xffff) as u16 as i16 as i64;
                let target = ins.offset as i64 + 4 + simm * 4;
                let Some(&t) = usize::try_from(target).ok().and_then(|t| at.get(&t)) else { return false };
                todo.push(t);
                if ins.mnemonic != "s_branch" {
                    todo.push(i + 1);
                }
                continue;
            }
            if ins.enc == Enc::Sopp && ins.mnemonic == "s_endpgm" {
                continue;
            }
            if writes(ins) {
                continue;
            }
            todo.push(i + 1);
        }
        true
    }

    pub(super) fn emit_rm_load(&mut self, idx: usize, ins: &Insn) -> Result<Option<()>, String> {
        let Some(&off) = self.rm_loads.get(&idx) else { return Ok(None) };
        let base = ins.dst.first().and_then(sreg_num).ok_or("s_load: bad dst")?;
        let n = 1u32 << ins.op;
        for k in 0..n {
            let Some((p, i)) = self.rm_param(off / 4 + k) else { continue };
            if let Some(RootParamKind::Constants { .. }) = self.rm_kind_of(p) {
                self.rm_used.insert(p);
                self.ws(&sreg_opnd(base + k as u8), &format!("tb_rc{p}_{i}"))?;
            }
        }
        Ok(Some(()))
    }

    pub(super) fn rm_header(&self) -> Option<String> {
        if self.rm_used.is_empty() {
            return None;
        }
        let mut h = String::new();
        for &p in &self.rm_used {
            if let Some(RootParamKind::Constants { register, space, num32 }) = self.rm_kind_of(p) {
                let fields: String = (0..*num32).map(|i| format!(" uint tb_rc{p}_{i};")).collect();
                let _ = write!(h, "cbuffer TB_RC{p} : register(b{register}, space{space}) {{{fields} }};\n");
            }
        }
        Some(h)
    }

    pub(super) fn cs_tgid_base(&self, nparams: usize) -> usize {
        if let Some(e) = self.dg_sgpr_end() {
            return if self.dg_has_mem() { super::durango::SGPR_END as usize } else { e };
        }
        if self.rootmem() {
            4
        } else {
            4 + nparams
        }
    }
}
