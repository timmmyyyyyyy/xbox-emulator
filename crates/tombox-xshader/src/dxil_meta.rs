use crate::meta::{GsInfo, MeshInfo};
use std::collections::HashMap;

pub const TAG_MS_STATE: u32 = 9;
pub const TAG_AS_STATE: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStageProps {
    pub tag: u32,
    pub num_threads: [u32; 3],
    pub max_vertices: u32,
    pub max_primitives: u32,
    pub topology: u32,
    pub payload_bytes: u32,
}

fn nodes(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for l in text.lines() {
        let l = l.trim();
        let Some(rest) = l.strip_prefix('!') else { continue };
        let Some((id, body)) = rest.split_once(" = ") else { continue };
        let body = body.trim_start_matches("distinct ").trim();
        if let Some(inner) = body.strip_prefix("!{").and_then(|b| b.strip_suffix('}')) {
            m.insert(id.trim().to_string(), inner.to_string());
        }
    }
    m
}

fn operands(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    for c in s.chars() {
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn int(op: &str) -> Option<u32> {
    let mut it = op.split_whitespace();
    let ty = it.next()?;
    if !ty.starts_with('i') {
        return None;
    }
    let v: i64 = it.next()?.parse().ok()?;
    u32::try_from(v).ok()
}

fn node_ref(op: &str) -> Option<&str> {
    op.strip_prefix('!').filter(|r| r.chars().all(|c| c.is_ascii_digit()))
}

pub fn parse_entry_stage_props(disasm: &str) -> Option<EntryStageProps> {
    let entry_line = disasm.lines().find_map(|l| l.trim().strip_prefix("!dx.entryPoints = !{"))?;
    let entry_id = node_ref(entry_line.trim_end_matches('}').split(',').next()?.trim())?;
    let n = nodes(disasm);
    let entry = operands(n.get(entry_id)?);
    let props = operands(n.get(node_ref(entry.get(4)?)?)?);
    for pair in props.chunks(2) {
        let [k, v] = pair else { break };
        let tag = int(k)?;
        if tag != TAG_MS_STATE && tag != TAG_AS_STATE {
            continue;
        }
        let st = operands(n.get(node_ref(v)?)?);
        let tg = operands(n.get(node_ref(st.first()?)?)?);
        if tg.len() != 3 {
            return None;
        }
        let num_threads = [int(&tg[0])?, int(&tg[1])?, int(&tg[2])?];
        return Some(if tag == TAG_MS_STATE {
            if st.len() != 5 {
                return None;
            }
            EntryStageProps { tag, num_threads, max_vertices: int(&st[1])?, max_primitives: int(&st[2])?, topology: int(&st[3])?, payload_bytes: int(&st[4])? }
        } else {
            if st.len() != 2 {
                return None;
            }
            EntryStageProps { tag, num_threads, max_vertices: 0, max_primitives: 0, topology: 0, payload_bytes: int(&st[1])? }
        });
    }
    None
}

pub fn check_against_psv0(m: &MeshInfo, p: &EntryStageProps) -> Result<[u32; 3], String> {
    let want_tag = match m.stage {
        13 => TAG_MS_STATE,
        14 => TAG_AS_STATE,
        s => return Err(format!("PSV0 stage {s} is not mesh/amplification")),
    };
    if p.tag != want_tag {
        return Err(format!("DXIL entry tag {} does not match PSV0 stage {}", p.tag, m.stage));
    }
    if p.payload_bytes != m.payload_bytes {
        return Err(format!("DXIL entry payload {} bytes, PSV0 {} bytes", p.payload_bytes, m.payload_bytes));
    }
    if m.stage == 13 && (p.max_vertices != m.max_vertices as u32 || p.max_primitives != m.max_primitives as u32 || p.topology != m.topology as u32) {
        return Err(format!(
            "DXIL mesh state {}v/{}p/topology {} does not match PSV0 {}v/{}p/topology {}",
            p.max_vertices, p.max_primitives, p.topology, m.max_vertices, m.max_primitives, m.topology
        ));
    }
    crate::meta::mesh_thread_count_for(m.stage, p.num_threads)?;
    Ok(p.num_threads)
}

pub fn fill_num_threads(gs: &mut Option<GsInfo>, disasm: impl FnOnce() -> Option<String>) -> Result<bool, String> {
    let Some(m) = gs.as_mut().and_then(|g| g.mesh.as_mut()) else { return Ok(false) };
    if m.num_threads != [0; 3] {
        crate::meta::mesh_thread_count_for(m.stage, m.num_threads)?;
        return Ok(false);
    }
    let Some(p) = disasm().as_deref().and_then(parse_entry_stage_props) else { return Ok(false) };
    m.num_threads = check_against_psv0(m, &p)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: &str = "!dx.version = !{!3}\n!dx.entryPoints = !{!7}\n!7 = !{void ()* @main, !\"main\", !8, null, !14}\n!8 = !{null, !9, null}\n!14 = !{i32 9, !15}\n!15 = !{!16, i32 3, i32 1, i32 2, i32 16}\n!16 = !{i32 32, i32 1, i32 1}\n";
    const AS: &str = "!dx.entryPoints = !{!6}\n!6 = !{void ()* @main, !\"main\", null, null, !7}\n!7 = !{i32 0, i64 16, i32 10, !8}\n!8 = !{!9, i32 40}\n!9 = !{i32 8, i32 4, i32 2}\n";

    fn mi(stage: u8, payload: u32) -> MeshInfo {
        MeshInfo { stage, groupshared_bytes: 0, payload_bytes: payload, max_vertices: 3, max_primitives: 1, prim_vectors: 0, topology: 2, num_threads: [0; 3] }
    }

    #[test]
    fn parses_mesh_and_amp_entry_state() {
        let p = parse_entry_stage_props(MS).unwrap();
        assert_eq!(p, EntryStageProps { tag: 9, num_threads: [32, 1, 1], max_vertices: 3, max_primitives: 1, topology: 2, payload_bytes: 16 });
        let a = parse_entry_stage_props(AS).unwrap();
        assert_eq!((a.tag, a.num_threads, a.payload_bytes), (10, [8, 4, 2], 40));
    }

    #[test]
    fn mismatches_are_refused() {
        let p = parse_entry_stage_props(MS).unwrap();
        assert!(check_against_psv0(&mi(13, 32), &p).unwrap_err().contains("payload"));
        assert!(check_against_psv0(&mi(14, 16), &p).unwrap_err().contains("tag"));
        let mut m = mi(13, 16);
        m.max_vertices = 4;
        assert!(check_against_psv0(&m, &p).unwrap_err().contains("mesh state"));
        assert_eq!(check_against_psv0(&mi(13, 16), &p), Ok([32, 1, 1]));
    }

    #[test]
    fn invalid_dimensions_do_not_fall_back_or_overflow() {
        let mut m = mi(13, 16);
        m.num_threads = [0, 2, 1];
        let mut gs = Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: Some(m), tess: None });
        assert!(fill_num_threads(&mut gs, || panic!("consulted")).is_err());
        m.num_threads = [0; 3];
        let mut p = parse_entry_stage_props(MS).unwrap();
        p.num_threads = [u32::MAX; 3];
        assert!(check_against_psv0(&m, &p).is_err());
    }

    #[test]
    fn psv0_size_wins_and_disassembly_is_not_consulted() {
        let mut m = mi(13, 16);
        m.num_threads = [128, 1, 1];
        let mut gs = Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: Some(m), tess: None });
        assert_eq!(fill_num_threads(&mut gs, || panic!("consulted")), Ok(false));
        let mut none = Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: Some(mi(13, 16)), tess: None });
        assert_eq!(fill_num_threads(&mut none, || None), Ok(false));
        assert_eq!(fill_num_threads(&mut none, || Some("garbage".into())), Ok(false));
        assert_eq!(fill_num_threads(&mut none, || Some(MS.into())), Ok(true));
        assert_eq!(none.unwrap().mesh.unwrap().num_threads, [32, 1, 1]);
    }
}
