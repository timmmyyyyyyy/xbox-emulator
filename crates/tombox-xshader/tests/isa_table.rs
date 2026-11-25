
use std::collections::{BTreeMap, BTreeSet};
use tombox_xshader::{gcn, rdna2};

type Table = BTreeMap<(&'static str, u32), &'static str>;

fn parse(src: &'static str) -> (Table, Vec<(&'static str, u32, &'static str, &'static str)>) {
    let (mut t, mut not) = (Table::new(), Vec::new());
    for line in src.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let neg = line.starts_with("! ");
        let f: Vec<&str> = line.trim_start_matches("! ").splitn(4, ' ').collect();
        let op = u32::from_str_radix(f[1].trim_start_matches("0x"), 16).unwrap();
        if neg {
            not.push((f[0], op, f[2], f[3]));
        } else {
            assert!(t.insert((f[0], op), f[2]).is_none(), "duplicate row {line}");
        }
    }
    (t, not)
}

fn gfx1013() -> (Table, Vec<(&'static str, u32, &'static str, &'static str)>) {
    parse(include_str!("isa_table/gfx1013.txt"))
}

fn gfx10_words(fam: &str, op: u32) -> Vec<u32> {
    let v0 = 0x100;
    match fam {
        "sop2" => vec![0x8000_0000 | op << 23],
        "sopk" => vec![0xb000_0000 | op << 23, 0],
        "sop1" => vec![0xbe80_0000 | op << 8],
        "sopc" => vec![0xbf00_0000 | op << 16],
        "sopp" => vec![0xbf80_0000 | op << 16],
        "smem" => vec![0xf400_0000 | op << 18, 0],
        "vop2" => vec![op << 25 | v0, 0],
        "vop1" => vec![0x7e00_0000 | op << 9 | v0],
        "vopc" => vec![0x7c00_0000 | op << 17 | v0],
        "vop3" => vec![0xd400_0000 | op << 16, v0],
        "vop3p" => vec![0xcc00_0000 | op << 16, v0],
        "vintrp" => vec![0xc800_0000 | op << 16],
        "ds" => vec![0xd800_0000 | op << 18, 0],
        "mubuf" => vec![0xe000_0000 | (op & 0x7f) << 18 | (op >> 7) << 25, 0],
        "mtbuf" => vec![0xe800_0000 | (op & 7) << 16, (op >> 3) << 21],
        "mimg" => vec![0xf000_0000 | (op & 0x7f) << 18 | op >> 7, 0],
        "flat" => vec![0xdc00_0000 | op << 18, 0],
        "scratch" => vec![0xdc00_4000 | op << 18, 0],
        "global" => vec![0xdc00_8000 | op << 18, 0],
        "exp" => vec![0xf800_0000, 0],
        _ => unreachable!("{fam}"),
    }
}

const FAMILIES: &[(&str, u32)] = &[
    ("sop2", 0x60), ("sopk", 0x1d), ("sop1", 0x100), ("sopc", 0x80), ("sopp", 0x80), ("smem", 0x100),
    ("vop2", 0x3e), ("vop1", 0x100), ("vopc", 0x100), ("vop3", 0x400), ("vop3p", 0x80), ("vintrp", 4),
    ("ds", 0x100), ("mubuf", 0x100), ("mtbuf", 0x10), ("mimg", 0x100), ("flat", 0x80), ("global", 0x80),
    ("scratch", 0x80), ("exp", 1),
];

fn decode(words: &[u32]) -> rdna2::Insn {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    rdna2::disassemble(&isa).into_iter().next().unwrap()
}

fn as_decoded(fam: &str, name: &str) -> String {
    match fam {
        "global" => name.replacen("global_", "flat_", 1),
        "scratch" => name.replacen("scratch_", "flat_", 1),
        _ => name.to_string(),
    }
}

#[test]
fn rdna2_names_match_gfx1013() {
    let (refs, not) = gfx1013();
    let mut fails = Vec::new();
    let mut unnamed: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for &(fam, n) in FAMILIES {
        for op in 0..n {
            let got = decode(&gfx10_words(fam, op)).mnemonic;
            let named = !got.contains('<');
            match (refs.get(&(fam, op)), named) {
                (Some(want), true) if as_decoded(fam, want) != got => fails.push(format!("{fam} {op:#05x}: decodes as {got}, gfx1013 is {want}")),
                (Some(want), false) => unnamed.entry(fam).or_default().push(format!("{op:#x} {want}")),
                (None, true) => {
                    let why = not.iter().find(|r| r.0 == fam && r.1 == op && as_decoded(fam, r.2) == got).map_or("no such gfx1013 opcode", |r| r.3);
                    fails.push(format!("{fam} {op:#05x}: decodes as {got}: {why}"));
                }
                _ => {}
            }
        }
    }
    for (fam, ops) in &unnamed {
        println!("{fam}: {} gfx1013 opcodes not named by the decoder: {}", ops.len(), ops.join(", "));
    }
    assert!(fails.is_empty(), "{} decoder names disagree with gfx1013:\n{}", fails.len(), fails.join("\n"));
}

fn gfx7_equivalent(name: &str) -> Vec<String> {
    let renamed = match name {
        "v_add_i32" => "v_add_co_u32",
        "v_sub_i32" => "v_sub_co_u32",
        "v_subrev_i32" => "v_subrev_co_u32",
        "v_addc_u32" => "v_add_co_ci_u32",
        "v_subb_u32" => "v_sub_co_ci_u32",
        "v_subbrev_u32" => "v_subrev_co_ci_u32",
        "v_lshl_b32" => "v_lshlrev_b32",
        "v_lshr_b32" => "v_lshrrev_b32",
        "v_ashr_i32" => "v_ashrrev_i32",
        "v_lshl_b64" => "v_lshlrev_b64",
        "v_lshr_b64" => "v_lshrrev_b64",
        "v_ashr_i64" => "v_ashrrev_i64",
        "s_dcache_inv_vol" => "s_dcache_inv",
        "buffer_wbinvl1" | "buffer_wbinvl1_vol" => return vec!["buffer_gl0_inv".into(), "buffer_gl1_inv".into()],
        "v_min_legacy_f32" => return vec!["v_cmp_lt_f32".into(), "v_cndmask_b32".into()],
        "v_max_legacy_f32" => return vec!["v_cmp_ge_f32".into(), "v_cndmask_b32".into()],
        "v_rcp_clamp_f32" => return vec!["v_rcp_f32".into(), "v_cmp_eq_f32".into(), "v_cndmask_b32".into(), "v_cmp_eq_f32".into(), "v_cndmask_b32".into()],
        "v_rsq_clamp_f32" => return vec!["v_rsq_f32".into(), "v_cmp_eq_f32".into(), "v_cndmask_b32".into(), "v_cmp_eq_f32".into(), "v_cndmask_b32".into()],
        "v_log_clamp_f32" => return vec!["v_log_f32".into(), "v_cmp_eq_f32".into(), "v_cndmask_b32".into()],
        "v_cvt_pkaccum_u8_f32" => "v_cvt_pk_u8_f32",
        _ => name,
    };
    vec![renamed.replacen("v_cmps_", "v_cmp_", 1).replacen("v_cmpsx_", "v_cmpx_", 1)]
}

fn gfx7_words(fam: &str, op: u32) -> Vec<u32> {
    let v0 = 0x100;
    match fam {
        "sop2" => vec![0x8000_0000 | op << 23],
        "sopk" if op == 0x15 => vec![0xb000_0000 | op << 23, 0],
        "sopk" => vec![0xb000_0000 | op << 23],
        "sop1" => vec![0xbe80_0000 | op << 8],
        "sopc" => vec![0xbf00_0000 | op << 16],
        "sopp" => vec![0xbf80_0000 | op << 16],
        "smrd" => vec![0xc000_0100 | op << 22],
        "vop2" if matches!(op, 0x20 | 0x21) => vec![op << 25 | v0, 0],
        "vop2" => vec![op << 25 | v0],
        "vop1" => vec![0x7e00_0000 | op << 9 | v0],
        "vopc" => vec![0x7c00_0000 | op << 17 | v0],
        "vop3" => vec![0xd000_0000 | op << 17, v0],
        "vintrp" => vec![0xc800_0000 | op << 16],
        "ds" => vec![0xd800_0000 | op << 18, 0],
        "mubuf" => vec![0xe000_0000 | op << 18, 0],
        "mtbuf" => vec![0xe800_0000 | op << 16, 0],
        "mimg" => vec![0xf000_0000 | op << 18, 0],
        "flat" => vec![0xdc00_0000 | op << 18, 0],
        "exp" => vec![0xf800_0000, 0],
        _ => unreachable!("{fam}"),
    }
}

const GFX7_FAMILIES: &[(&str, u32)] = &[
    ("sop2", 0x60), ("sopk", 0x1d), ("sop1", 0x100), ("sopc", 0x80), ("sopp", 0x80), ("smrd", 0x20),
    ("vop2", 0x3e), ("vop1", 0x100), ("vopc", 0x100), ("vop3", 0x200), ("vintrp", 4), ("ds", 0x100),
    ("mubuf", 0x80), ("mtbuf", 8), ("mimg", 0x80), ("flat", 0x80), ("exp", 1),
];

fn gfx10_key(i: &rdna2::Insn) -> (&'static str, u32) {
    use rdna2::Enc::*;
    let w = i.raw[0];
    let fam = match i.enc {
        Sop2 => "sop2", Sopk => "sopk", Sop1 => "sop1", Sopc => "sopc", Sopp => "sopp", Smem => "smem",
        Vop2 => "vop2", Vop1 => "vop1", Vopc => "vopc", Vop3 if i.op >= 0x400 => return ("vop3p", i.op - 0x400),
        Vop3 => "vop3", Vinterp => "vintrp", Ds => "ds", Mubuf => "mubuf", Mtbuf => "mtbuf", Mimg => "mimg",
        Flat => ["flat", "scratch", "global", "flat"][((w >> 14) & 3) as usize], Exp => "exp", Unknown => "unknown",
    };
    (fam, if fam == "flat" || fam == "global" || fam == "scratch" { (w >> 18) & 0x7f } else { i.op })
}

#[test]
fn gcn_lowering_keeps_instruction() {
    let (refs7, _) = parse(include_str!("isa_table/gfx7.txt"));
    let (refs10, _) = gfx1013();
    let mut fails = Vec::new();
    let mut checked = 0;
    let mut refused = Vec::new();
    for &(fam, n) in GFX7_FAMILIES {
        for op in 0..n {
            let mut words = gfx7_words(fam, op);
            words.push(0xbf81_0000);
            let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let Some(name7) = refs7.get(&(fam, op)) else {
                continue;
            };
            let Ok(low) = gcn::to_rdna2(&code) else {
                refused.push(*name7);
                continue;
            };
            checked += 1;
            let mut got = rdna2::disassemble(&low.isa);
            assert_eq!(got.pop().map(|i| i.mnemonic).as_deref(), Some("s_endpgm"));
            let names: Vec<String> = got
                .iter()
                .map(|i| {
                    let key = gfx10_key(i);
                    refs10.get(&key).map_or_else(|| format!("{} {:#x} (not a gfx1013 opcode)", key.0, key.1), |n| n.to_string())
                })
                .collect();
            if names != gfx7_equivalent(name7) {
                fails.push(format!("gfx7 {fam} {op:#05x} {name7} lowers to {}", names.join(" + ")));
            }
        }
    }
    println!("gcn: {checked} lowered gfx7 opcodes checked; {} refused: {}", refused.len(), refused.join(", "));
    assert!(fails.is_empty(), "{} gfx7 lowering mismatches:\n{}", fails.len(), fails.join("\n"));
}

#[test]
fn reference_tables_cover_every_family() {
    let (refs, _) = gfx1013();
    let fams: BTreeSet<&str> = refs.keys().map(|k| k.0).collect();
    for &(fam, _) in FAMILIES {
        assert!(fams.contains(fam), "{fam} missing from gfx1013.txt");
    }
}
