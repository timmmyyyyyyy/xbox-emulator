use crate::meta::{RootParam, RootParamKind, RootRange, RootSig};
use crate::Stage;

const NO_TABLE: u64 = 0xDEAD_BEEF_DEAD_BEEF;
const LIST_BIAS: usize = 0x10;
const ENTRY: usize = 20;
const MAX_ENTRIES: usize = 1 << 16;
const MAX_PARAMS: usize = 64;
const STAGE_SLOTS: std::ops::Range<usize> = 0x18..0x60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Srv,
    Sampler,
    Cbv,
    Uav,
}

impl Class {
    fn from_code(c: u32) -> Option<Class> {
        Some(match c {
            0 => Class::Srv,
            1 => Class::Sampler,
            2 => Class::Cbv,
            3 => Class::Uav,
            _ => return None,
        })
    }

    pub fn range_type(self) -> u32 {
        match self {
            Class::Srv => 0,
            Class::Uav => 1,
            Class::Cbv => 2,
            Class::Sampler => 3,
        }
    }

    pub fn table_stride_dwords(self) -> u32 {
        if self == Class::Sampler {
            4
        } else {
            8
        }
    }

    fn root_descriptor_kind(self) -> Option<u32> {
        match self {
            Class::Cbv => Some(2),
            Class::Srv => Some(3),
            Class::Uav => Some(4),
            Class::Sampler => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Constants,
    Descriptor,
    Table,
    StaticSampler,
}

impl Source {
    fn from_code(c: u32) -> Option<Source> {
        Some(match c {
            0 => Source::Constants,
            1 => Source::Descriptor,
            3 => Source::Table,
            4 => Source::StaticSampler,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Constants { register: u32, num32: u32 },
    Descriptor { register: u32 },
    Table,
}

impl ParamKind {
    pub fn sgprs(&self) -> u32 {
        match self {
            ParamKind::Constants { num32, .. } => *num32,
            ParamKind::Descriptor { .. } => 2,
            ParamKind::Table => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub slot: u8,
    pub kind: ParamKind,
    pub table_entries: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    pub source: Source,
    pub class: Class,
    pub offset_dwords: u32,
    pub register: u32,
    pub stride: u32,
    pub param: u32,
    pub slot: u8,
    pub dwords: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticSamplerSlot {
    pub index: u32,
    pub register: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    pub hw_stage_slot: Option<u16>,
    pub user_sgprs: u8,
    pub params: Vec<Param>,
    pub resource_heap: Option<u8>,
    pub sampler_heap: Option<u8>,
    pub static_samplers: Vec<StaticSamplerSlot>,
    pub uses: Vec<Use>,
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("FSSM binding table: read past the end at {o:#x}"))
}

fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("FSSM binding table: read past the end at {o:#x}"))
}

fn list(fssm: &[u8], ptr: u64, count: u32, name: &str) -> Result<Vec<[u32; 5]>, String> {
    if ptr == NO_TABLE {
        return if count == 0 { Ok(Vec::new()) } else { Err(format!("FSSM binding table: {name} has {count} entries but no pointer")) };
    }
    let count = count as usize;
    if count > MAX_ENTRIES {
        return Err(format!("FSSM binding table: implausible {name} entry count {count}"));
    }
    let start = usize::try_from(ptr).ok().and_then(|p| p.checked_add(LIST_BIAS)).ok_or_else(|| format!("FSSM binding table: {name} pointer {ptr:#x} out of range"))?;
    if start.checked_add(count * ENTRY).map_or(true, |e| e > fssm.len()) {
        return Err(format!("FSSM binding table: {name} ({count} entries at {start:#x}) exceeds the {}-byte FSSM", fssm.len()));
    }
    (0..count)
        .map(|i| {
            let mut e = [0u32; 5];
            for (k, w) in e.iter_mut().enumerate() {
                *w = u32_at(fssm, start + i * ENTRY + k * 4)?;
            }
            Ok(e)
        })
        .collect()
}

pub fn parse(fssm: &[u8]) -> Result<Option<Bindings>, String> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return Err("FSSM binding table: not an FSSM container".into());
    }
    let p1 = u64_at(fssm, 0xE8)?;
    if p1 == NO_TABLE {
        return Ok(None);
    }
    let user_sgprs = *fssm.get(0xD6).ok_or("FSSM binding table: header too short")?;
    let mut hw_stage_slot = None;
    for at in STAGE_SLOTS.step_by(8) {
        if !matches!(u64_at(fssm, at)?, 0 | NO_TABLE) {
            if let Some(prev) = hw_stage_slot.replace(at as u16) {
                return Err(format!("FSSM binding table: more than one hardware stage slot is set ({prev:#x} and {at:#x})"));
            }
        }
    }
    let l1 = list(fssm, p1, u32_at(fssm, 0xF8)?, "parameter list")?;
    let l2 = list(fssm, u64_at(fssm, 0xF0)?, u32_at(fssm, 0xFC)?, "resource list")?;

    let mut params: Vec<Option<Param>> = Vec::new();
    let mut resource_heap = None;
    let mut sampler_heap = None;
    let mut static_samplers = Vec::new();
    for (i, &[kind, off, count, register, loc]) in l1.iter().enumerate() {
        let (p, slot) = ((loc >> 8) as usize, (loc & 0xFF) as u8);
        let pk = match kind {
            0 if count > 0 => ParamKind::Constants { register, num32: count },
            1 if count == 2 => ParamKind::Descriptor { register },
            3 => ParamKind::Table,
            4 => {
                if static_samplers.iter().any(|s: &StaticSamplerSlot| s.index == p as u32) {
                    return Err(format!("FSSM binding table: static sampler {p} listed twice"));
                }
                static_samplers.push(StaticSamplerSlot { index: p as u32, register });
                continue;
            }
            5 | 6 => {
                let h = if kind == 5 { &mut resource_heap } else { &mut sampler_heap };
                if h.replace(slot).is_some() {
                    return Err(format!("FSSM binding table: descriptor heap kind {kind} listed twice"));
                }
                continue;
            }
            _ => return Err(format!("FSSM binding table: entry {i} has unknown kind {kind} (count {count})")),
        };
        if p >= MAX_PARAMS {
            return Err(format!("FSSM binding table: entry {i} names root parameter {p}"));
        }
        if params.len() <= p {
            params.resize(p + 1, None);
        }
        match &mut params[p] {
            Some(prev) if prev.slot != slot || prev.kind != pk || pk != ParamKind::Table => {
                return Err(format!("FSSM binding table: root parameter {p} listed as {:?} at s{} and {pk:?} at s{slot}", prev.kind, prev.slot));
            }
            Some(prev) => prev.table_entries.push((off, register)),
            empty => *empty = Some(Param { slot, kind: pk, table_entries: if pk == ParamKind::Table { vec![(off, register)] } else { Vec::new() } }),
        }
    }
    let params: Vec<Param> = params.into_iter().enumerate().map(|(p, x)| x.ok_or_else(|| format!("FSSM binding table: root parameter {p} is missing"))).collect::<Result<_, _>>()?;
    let mut end = params.first().map(|p| p.slot as u32);
    for (p, prm) in params.iter().enumerate() {
        if let Some(e) = end {
            if prm.slot as u32 != e {
                return Err(format!("FSSM binding table: root parameter {p} at s{} but the previous parameter ends at s{e}", prm.slot));
            }
        }
        end = Some(prm.slot as u32 + prm.kind.sgprs());
    }
    let mut used: Vec<u32> = params.iter().flat_map(|p| (p.slot as u32)..(p.slot as u32 + p.kind.sgprs())).collect();
    for h in [resource_heap, sampler_heap].into_iter().flatten() {
        if used.contains(&(h as u32)) {
            return Err(format!("FSSM binding table: descriptor heap base s{h} overlaps a root parameter"));
        }
        used.push(h as u32);
    }
    if let Some(top) = used.iter().max() {
        if top + 1 != user_sgprs as u32 {
            return Err(format!("FSSM binding table: user SGPR count {user_sgprs} does not end at the last binding (s{top})"));
        }
    }

    let mut uses = Vec::with_capacity(l2.len());
    for (i, &[code, offset_dwords, register, stride, loc]) in l2.iter().enumerate() {
        let (source, class) = match (Source::from_code(code >> 8), Class::from_code(code & 0xFF)) {
            (Some(s), Some(c)) => (s, c),
            _ => return Err(format!("FSSM binding table: resource {i} has unknown code {code:#x}")),
        };
        let u = Use { source, class, offset_dwords, register, stride, param: loc >> 16, slot: (loc >> 8) as u8, dwords: loc as u8 };
        match source {
            Source::StaticSampler => {
                if class != Class::Sampler || !static_samplers.iter().any(|s| s.index == u.param && s.register == register) {
                    return Err(format!("FSSM binding table: resource {i} names static sampler {} (s{register}) that the parameter list does not declare", u.param));
                }
            }
            _ => {
                let prm = params.get(u.param as usize).ok_or_else(|| format!("FSSM binding table: resource {i} names root parameter {} of {}", u.param, params.len()))?;
                let fits = match (source, prm.kind) {
                    (Source::Constants, ParamKind::Constants { register: r, .. }) => class == Class::Cbv && r == register,
                    (Source::Descriptor, ParamKind::Descriptor { register: r }) => class != Class::Sampler && r == register,
                    (Source::Table, ParamKind::Table) => offset_dwords % class.table_stride_dwords() == 0,
                    _ => false,
                };
                if !fits || prm.slot != u.slot {
                    return Err(format!("FSSM binding table: resource {i} ({source:?} {class:?} register {register} at s{}) does not match root parameter {} ({:?} at s{})", u.slot, u.param, prm.kind, prm.slot));
                }
            }
        }
        if let Some(prev) = uses.iter().find(|o: &&Use| o.source == source && o.class == class && o.param == u.param && o.offset_dwords == offset_dwords && o.register != register) {
            return Err(format!("FSSM binding table: {class:?} at dword {offset_dwords} of parameter {} has registers {} and {register}", u.param, prev.register));
        }
        if let Some(prev) = uses.iter().find(|o: &&Use| o.source != Source::StaticSampler && source != Source::StaticSampler && o.class == class && o.register == register && (o.param, o.offset_dwords) != (u.param, offset_dwords)) {
            return Err(format!("FSSM binding table: {class:?} register {register} is used at parameter {} dword {} and parameter {} dword {offset_dwords}; the table has no register space", prev.param, prev.offset_dwords, u.param));
        }
        if !uses.contains(&u) {
            uses.push(u);
        }
    }
    Ok(Some(Bindings { hw_stage_slot, user_sgprs, params, resource_heap, sampler_heap, static_samplers, uses }))
}

impl Bindings {
    pub fn system_sgprs(&self, stage: Stage) -> Result<usize, String> {
        match (self.hw_stage_slot, stage) {
            (None, Stage::Compute) | (Some(0x38), Stage::Vertex) | (Some(0x40), Stage::Pixel) => Ok(0),
            (Some(0x28), Stage::Vertex) | (Some(0x30), Stage::Geometry) | (Some(0x58), Stage::Mesh) => Ok(8),
            (slot, stage) => Err(format!("FSSM binding table: hardware stage slot {slot:x?} is not known for a {stage:?} shader")),
        }
    }

    pub fn user_base(&self, stage: Stage) -> Result<Option<usize>, String> {
        let system = self.system_sgprs(stage)?;
        Ok(self.first_slot().map(|s| s as usize + system))
    }

    pub fn first_slot(&self) -> Option<u8> {
        self.params.first().map(|p| p.slot)
    }

    pub fn root_signature(&self) -> RootSig {
        let params = self
            .params
            .iter()
            .enumerate()
            .map(|(p, prm)| {
                let mine = || self.uses.iter().filter(move |u| u.param == p as u32 && u.source != Source::StaticSampler);
                let kind = match prm.kind {
                    ParamKind::Constants { register, num32 } => RootParamKind::Constants { register, space: 0, num32 },
                    ParamKind::Descriptor { register } => RootParamKind::Descriptor { kind: mine().find_map(|u| u.class.root_descriptor_kind()).unwrap_or(3), register, space: 0 },
                    ParamKind::Table => {
                        let mut ranges: Vec<RootRange> = mine()
                            .map(|u| RootRange { range_type: u.class.range_type(), num: 1, base_register: u.register, space: 0, offset_in_table: u.offset_dwords / u.class.table_stride_dwords() })
                            .collect();
                        ranges.sort_by_key(|r| (r.offset_in_table, r.range_type));
                        ranges.dedup_by_key(|r| (r.offset_in_table, r.range_type, r.base_register));
                        RootParamKind::Table(ranges)
                    }
                };
                RootParam { visibility: 0, kind }
            })
            .collect();
        RootSig { version: 2, flags: 0, params, num_static_samplers: 0, static_samplers: Vec::new() }
    }

    pub fn disagreements(&self, rs: &RootSig) -> Vec<String> {
        let mut out = Vec::new();
        if rs.params.len() != self.params.len() {
            out.push(format!("{} root parameters in the FSSM table, {} in the root signature", self.params.len(), rs.params.len()));
        }
        for (p, (mine, theirs)) in self.params.iter().zip(&rs.params).enumerate() {
            let ok = match (&mine.kind, &theirs.kind) {
                (ParamKind::Table, RootParamKind::Table(_)) => true,
                (ParamKind::Descriptor { register: a }, RootParamKind::Descriptor { register: b, space: 0, .. }) => a == b,
                (ParamKind::Constants { register: a, num32: n }, RootParamKind::Constants { register: b, space: 0, num32: m }) => a == b && n == m,
                _ => false,
            };
            if !ok {
                out.push(format!("parameter {p}: FSSM {:?}, root signature {:?}", mine.kind, theirs.kind));
            }
        }
        for u in &self.uses {
            let theirs = match u.source {
                Source::StaticSampler => rs.static_samplers.get(u.param as usize).map(|s| (3, s.register, s.space)),
                _ => match rs.params.get(u.param as usize).map(|x| &x.kind) {
                    Some(RootParamKind::Descriptor { kind, register, space }) => Some((match kind { 2 => 2, 3 => 0, _ => 1 }, *register, *space)),
                    Some(RootParamKind::Constants { register, space, .. }) => Some((2, *register, *space)),
                    Some(RootParamKind::Table(ranges)) => table_lookup(ranges, u.offset_dwords / u.class.table_stride_dwords(), u.class.range_type()),
                    None => None,
                },
            };
            if theirs != Some((u.class.range_type(), u.register, 0)) {
                out.push(format!("{:?} {:?} parameter {} dword {} register {}: root signature has {:?}", u.source, u.class, u.param, u.offset_dwords, u.register, theirs));
            }
        }
        out
    }
}

fn table_lookup(ranges: &[RootRange], idx: u32, want: u32) -> Option<(u32, u32, u32)> {
    let mut next = 0u32;
    let mut hit = None;
    for r in ranges {
        let start = if r.offset_in_table == u32::MAX { next } else { r.offset_in_table };
        next = start.saturating_add(r.num);
        if idx >= start && (r.num == u32::MAX || idx - start < r.num) {
            let found = (r.range_type, r.base_register + (idx - start), r.space);
            if r.range_type == want {
                return Some(found);
            }
            hit.get_or_insert(found);
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(p: u32, s: u32) -> u32 {
        (p << 8) | s
    }

    fn uloc(p: u32, s: u32, dwords: u32) -> u32 {
        (p << 16) | (s << 8) | dwords
    }

    fn blob(user_sgprs: u8, l1: &[[u32; 5]], l2: &[[u32; 5]]) -> Vec<u8> {
        let mut f = vec![0u8; 0x100];
        f[0..4].copy_from_slice(b"FSSM");
        f[0xD6] = user_sgprs;
        let p1 = (f.len() - LIST_BIAS) as u64;
        f[0xE8..0xF0].copy_from_slice(&p1.to_le_bytes());
        f[0xF8..0xFC].copy_from_slice(&(l1.len() as u32).to_le_bytes());
        for e in l1 {
            e.iter().for_each(|w| f.extend_from_slice(&w.to_le_bytes()));
        }
        let p2 = if l2.is_empty() { NO_TABLE } else { (f.len() - LIST_BIAS) as u64 };
        f[0xF0..0xF8].copy_from_slice(&p2.to_le_bytes());
        f[0xFC..0x100].copy_from_slice(&(l2.len() as u32).to_le_bytes());
        for e in l2 {
            e.iter().for_each(|w| f.extend_from_slice(&w.to_le_bytes()));
        }
        f.extend_from_slice(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
        f
    }

    fn compute_tables() -> Vec<u8> {
        let l1: Vec<[u32; 5]> = (0..4).map(|p| [3, 0, 1, 0, loc(p, 4 + p)]).collect();
        let l2 = [
            [0x300, 0, 0, 16, uloc(0, 4, 4)],
            [0x300, 8, 1, 4, uloc(0, 4, 8)],
            [0x302, 0, 0, 0, uloc(1, 5, 4)],
            [0x301, 4, 1, 0, uloc(2, 6, 4)],
            [0x303, 0, 0, 4, uloc(3, 7, 8)],
        ];
        blob(8, &l1, &l2)
    }

    fn unbounded(range_type: u32, num: u32) -> RootParam {
        RootParam { visibility: 0, kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]) }
    }

    fn engine_root_signature() -> RootSig {
        RootSig { version: 2, flags: 0, params: vec![unbounded(0, 64), unbounded(2, 16), unbounded(3, 16), unbounded(1, 16)], num_static_samplers: 0, static_samplers: Vec::new() }
    }

    #[test]
    fn no_table_marker_means_no_bindings() {
        let mut f = compute_tables();
        f[0xE8..0xF0].copy_from_slice(&NO_TABLE.to_le_bytes());
        assert_eq!(parse(&f), Ok(None));
        assert!(parse(b"DXBC").is_err());
    }

    #[test]
    fn one_table_per_parameter_with_typed_uses() {
        let b = parse(&compute_tables()).unwrap().unwrap();
        assert_eq!(b.user_sgprs, 8);
        assert_eq!(b.first_slot(), Some(4));
        assert_eq!(b.params.len(), 4);
        assert!(b.params.iter().enumerate().all(|(p, x)| x.kind == ParamKind::Table && x.slot as usize == 4 + p));
        assert_eq!(b.uses.len(), 5);
        assert_eq!(b.uses[3], Use { source: Source::Table, class: Class::Sampler, offset_dwords: 4, register: 1, stride: 0, param: 2, slot: 6, dwords: 4 });
        let rs = b.root_signature();
        let ranges = |p: usize| match &rs.params[p].kind {
            RootParamKind::Table(r) => r.iter().map(|r| (r.range_type, r.num, r.base_register, r.space, r.offset_in_table)).collect::<Vec<_>>(),
            other => panic!("parameter {p} is {other:?}"),
        };
        assert_eq!(ranges(0), vec![(0, 1, 0, 0, 0), (0, 1, 1, 0, 1)]);
        assert_eq!(ranges(1), vec![(2, 1, 0, 0, 0)]);
        assert_eq!(ranges(2), vec![(3, 1, 1, 0, 1)]);
        assert_eq!(ranges(3), vec![(1, 1, 0, 0, 0)]);
        assert!(b.disagreements(&engine_root_signature()).is_empty(), "{:?}", b.disagreements(&engine_root_signature()));
    }

    #[test]
    fn disagreements_name_the_mismatch() {
        let b = parse(&compute_tables()).unwrap().unwrap();
        let mut rs = engine_root_signature();
        if let RootParamKind::Table(r) = &mut rs.params[1].kind {
            r[0].base_register = 3;
        }
        let d = b.disagreements(&rs);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].contains("Cbv parameter 1"), "{d:?}");
        rs.params.pop();
        assert!(b.disagreements(&rs).iter().any(|x| x.contains("4 root parameters in the FSSM table, 3")));
    }

    #[test]
    fn constants_descriptors_heaps_and_static_samplers() {
        let l1 = [
            [4, 0, 0, 7, loc(0, 0)],
            [4, 0, 0, 2, loc(1, 0)],
            [0, 0, 2, 500, loc(0, 8)],
            [1, 0, 2, 4, loc(1, 10)],
            [3, 0x98, 1, 29, loc(2, 12)],
            [3, 0xA0, 1, 30, loc(2, 12)],
            [5, 0, 1, 0, loc(0, 13)],
            [6, 0, 1, 0, loc(0, 14)],
        ];
        let l2 = [
            [0x401, 0, 2, 0, uloc(1, 0, 4)],
            [0x002, 0, 500, 0, uloc(0, 8, 4)],
            [0x100, 0, 4, 1, uloc(1, 10, 4)],
            [0x300, 0x98, 29, 4, uloc(2, 12, 8)],
        ];
        let b = parse(&blob(15, &l1, &l2)).unwrap().unwrap();
        assert_eq!(b.params.iter().map(|p| (p.slot, p.kind)).collect::<Vec<_>>(), vec![(8, ParamKind::Constants { register: 500, num32: 2 }), (10, ParamKind::Descriptor { register: 4 }), (12, ParamKind::Table)]);
        assert_eq!(b.params[2].table_entries, vec![(0x98, 29), (0xA0, 30)]);
        assert_eq!((b.resource_heap, b.sampler_heap), (Some(13), Some(14)));
        assert_eq!(b.static_samplers, vec![StaticSamplerSlot { index: 0, register: 7 }, StaticSamplerSlot { index: 1, register: 2 }]);
        let rs = b.root_signature();
        assert!(matches!(rs.params[0].kind, RootParamKind::Constants { register: 500, space: 0, num32: 2 }));
        assert!(matches!(rs.params[1].kind, RootParamKind::Descriptor { kind: 3, register: 4, space: 0 }));
        match &rs.params[2].kind {
            RootParamKind::Table(r) => assert_eq!(r.iter().map(|r| (r.range_type, r.base_register, r.offset_in_table)).collect::<Vec<_>>(), vec![(0, 29, 0x13)]),
            other => panic!("{other:?}"),
        }
        assert_eq!(rs.num_static_samplers, 0);
    }

    #[test]
    fn inconsistent_layouts_are_refused() {
        let table = |p: u32, s: u32| [3, 0, 1, 0, loc(p, s)];
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("gap", blob(11, &[table(0, 8), table(1, 10)], &[])),
            ("unknown kind", blob(9, &[[2, 0, 1, 0, loc(0, 8)]], &[])),
            ("missing", blob(9, &[table(1, 8)], &[])),
            ("count", blob(12, &[table(0, 8)], &[])),
            ("twice", blob(9, &[table(0, 8), table(0, 9)], &[])),
            ("slot", blob(9, &[table(0, 8)], &[[0x300, 0, 0, 4, uloc(0, 9, 8)]])),
            ("stride", blob(9, &[table(0, 8)], &[[0x300, 4, 0, 4, uloc(0, 8, 8)]])),
            ("space", blob(10, &[table(0, 8), table(1, 9)], &[[0x300, 0, 3, 4, uloc(0, 8, 8)], [0x300, 0, 3, 4, uloc(1, 9, 8)]])),
            ("static", blob(9, &[table(0, 8)], &[[0x401, 0, 7, 0, uloc(0, 0, 4)]])),
            ("heap overlap", blob(9, &[table(0, 8), [5, 0, 1, 0, loc(0, 8)]], &[])),
        ];
        for (what, f) in cases {
            assert!(parse(&f).is_err(), "{what}: {:?}", parse(&f));
        }
        let mut f = blob(9, &[table(0, 8)], &[]);
        f[0xF8..0xFC].copy_from_slice(&1000u32.to_le_bytes());
        assert!(parse(&f).unwrap_err().contains("exceeds"));
        f[0xF8..0xFC].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&f).unwrap_err().contains("implausible"));
    }

    #[test]
    fn hardware_stage_slot_selects_the_user_data_base() {
        let with_slot = |slot: Option<usize>| {
            let mut f = blob(10, &[[3, 0, 1, 0, loc(0, 8)], [3, 0, 1, 0, loc(1, 9)]], &[]);
            for at in STAGE_SLOTS.step_by(8) {
                f[at..at + 8].copy_from_slice(&NO_TABLE.to_le_bytes());
            }
            if let Some(at) = slot {
                f[at..at + 8].copy_from_slice(&0xB0u64.to_le_bytes());
            }
            parse(&f).unwrap().unwrap()
        };
        assert_eq!(with_slot(Some(0x40)).user_base(Stage::Pixel), Ok(Some(8)));
        assert_eq!(with_slot(Some(0x38)).user_base(Stage::Vertex), Ok(Some(8)));
        assert_eq!(with_slot(Some(0x28)).user_base(Stage::Vertex), Ok(Some(16)));
        assert_eq!(with_slot(Some(0x30)).user_base(Stage::Geometry), Ok(Some(16)));
        assert_eq!(with_slot(Some(0x58)).user_base(Stage::Mesh), Ok(Some(16)));
        assert_eq!(with_slot(None).user_base(Stage::Compute), Ok(Some(8)));
        assert!(with_slot(Some(0x40)).user_base(Stage::Vertex).is_err());
        assert!(with_slot(Some(0x20)).user_base(Stage::Vertex).is_err());
        assert!(with_slot(None).user_base(Stage::Pixel).is_err());
        let mut f = blob(9, &[[3, 0, 1, 0, loc(0, 8)]], &[]);
        f[0x28..0x30].copy_from_slice(&0xB0u64.to_le_bytes());
        f[0x38..0x40].copy_from_slice(&0xC0u64.to_le_bytes());
        assert!(parse(&f).unwrap_err().contains("more than one hardware stage slot"));
    }
}
