use super::bufref::BufRef;
use super::{r, Emitter, Res, BINDLESS};
use crate::rdna2::{Insn, Opnd};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Num {
    Unorm,
    Snorm,
    Uscaled,
    Sscaled,
    Uint,
    Sint,
    Float,
}

pub(super) struct Ufmt {
    pub fields: &'static [(u32, u32)],
    pub bytes: u32,
    pub num: Num,
}

const L8: &[(u32, u32)] = &[(0, 8)];
const L16: &[(u32, u32)] = &[(0, 16)];
const L8_8: &[(u32, u32)] = &[(0, 8), (8, 8)];
const L32: &[(u32, u32)] = &[(0, 32)];
const L16_16: &[(u32, u32)] = &[(0, 16), (16, 16)];
const L10_11_11: &[(u32, u32)] = &[(0, 11), (11, 11), (22, 10)];
const L11_11_10: &[(u32, u32)] = &[(0, 10), (10, 11), (21, 11)];
const L10_10_10_2: &[(u32, u32)] = &[(0, 2), (2, 10), (12, 10), (22, 10)];
const L2_10_10_10: &[(u32, u32)] = &[(0, 10), (10, 10), (20, 10), (30, 2)];
const L8_8_8_8: &[(u32, u32)] = &[(0, 8), (8, 8), (16, 8), (24, 8)];
const L32_32: &[(u32, u32)] = &[(0, 32), (32, 32)];
const L16_16_16_16: &[(u32, u32)] = &[(0, 16), (16, 16), (32, 16), (48, 16)];
const L32_32_32: &[(u32, u32)] = &[(0, 32), (32, 32), (64, 32)];
const L32_32_32_32: &[(u32, u32)] = &[(0, 32), (32, 32), (64, 32), (96, 32)];

const N6: [Num; 6] = [Num::Unorm, Num::Snorm, Num::Uscaled, Num::Sscaled, Num::Uint, Num::Sint];
const N7: [Num; 7] = [Num::Unorm, Num::Snorm, Num::Uscaled, Num::Sscaled, Num::Uint, Num::Sint, Num::Float];
const N3: [Num; 3] = [Num::Uint, Num::Sint, Num::Float];

pub(super) fn ufmt(f: u32) -> Option<Ufmt> {
    let groups: [(u32, &'static [(u32, u32)], u32, &[Num]); 14] = [
        (1, L8, 1, &N6),
        (7, L16, 2, &N7),
        (14, L8_8, 2, &N6),
        (20, L32, 4, &N3),
        (23, L16_16, 4, &N7),
        (30, L10_11_11, 4, &N7),
        (37, L11_11_10, 4, &N7),
        (44, L10_10_10_2, 4, &N6),
        (50, L2_10_10_10, 4, &N6),
        (56, L8_8_8_8, 4, &N6),
        (62, L32_32, 8, &N3),
        (65, L16_16_16_16, 8, &N7),
        (72, L32_32_32, 12, &N3),
        (75, L32_32_32_32, 16, &N3),
    ];
    groups.iter().find_map(|&(first, fields, bytes, nums)| {
        (f >= first && f < first + nums.len() as u32).then(|| Ufmt { fields, bytes, num: nums[(f - first) as usize] })
    })
}

fn comps(m: &str) -> Option<u32> {
    Some(match m.rsplit('_').next()? {
        "x" => 1,
        "xy" => 2,
        "xyz" => 3,
        "xyzw" => 4,
        _ => return None,
    })
}

fn mask(bits: u32) -> u32 {
    if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 }
}

fn sext(v: &str, bits: u32) -> String {
    if bits >= 32 { format!("asint({v})") } else { format!("(asint(({v}) << {}u) >> {}u)", 32 - bits, 32 - bits) }
}

fn small_float(v: &str, mb: u32) -> String {
    let den = 127 - 14 - mb;
    format!(
        "((((({v}) >> {mb}u) & 31u) == 0u) ? asuint((float)(({v}) & {m:#x}u) * asfloat({:#x}u)) : (((({v}) >> {mb}u) & 31u) == 31u) ? (0x7f800000u | ((({v}) & {m:#x}u) << {sh}u)) : (((((({v}) >> {mb}u) & 31u) + 112u) << 23) | ((({v}) & {m:#x}u) << {sh}u)))",
        den << 23,
        m = mask(mb),
        sh = 23 - mb
    )
}

fn unpack(v: &str, bits: u32, num: Num) -> String {
    let mx = mask(bits);
    match num {
        Num::Unorm => format!("asuint((float)((double)({v}) / {}.0L))", mx),
        Num::Snorm => format!("asuint(max((float)((double){} / {}.0L), -1.0))", sext(v, bits), mask(bits - 1)),
        Num::Uscaled => format!("asuint((float)({v}))"),
        Num::Sscaled => format!("asuint((float){})", sext(v, bits)),
        Num::Uint => v.to_string(),
        Num::Sint => format!("asuint({})", sext(v, bits)),
        Num::Float => match bits {
            32 => v.to_string(),
            16 => format!("asuint(f16tof32({v}))"),
            11 => small_float(v, 6),
            10 => small_float(v, 5),
            _ => unreachable!(),
        },
    }
}

fn pack(v: &str, bits: u32, num: Num) -> Result<String, String> {
    let mx = mask(bits);
    Ok(match num {
        Num::Unorm => format!("((uint)round(saturate(asfloat({v})) * {mx}.0))"),
        Num::Snorm => {
            let a = format!("(abs(clamp(asfloat({v}), -1.0, 1.0)) * {}.0)", mask(bits - 1));
            let q = format!("((int)(floor({a}) + ((({a}) - floor({a})) >= 0.5 ? 1.0 : 0.0)))");
            format!("(((({v}) & 0x7fffffffu) > 0x7f800000u) ? 0u : (asuint((({v}) >> 31) != 0u ? -{q} : {q}) & {mx:#x}u))")
        }
        Num::Uint if bits == 32 => v.to_string(),
        Num::Uint => format!("min({v}, {mx:#x}u)"),
        Num::Sint if bits == 32 => v.to_string(),
        Num::Sint => super::sat_int(&format!("asint({v})"), bits),
        Num::Float if bits == 32 => v.to_string(),
        Num::Float if bits == 16 => format!("(f32tof16(asfloat({v})) & 0xffffu)"),
        other => return Err(format!("tbuffer store to a {bits}-bit {other:?} component is not lowered")),
    })
}

pub(super) const D16_HELPERS: &str = r#"uint tb_q16(bool neg, uint num, uint den) {
    if (num == 0u) return 0u;
    int e = (int)firstbithigh(num) - (int)firstbithigh(den);
    uint64_t n64 = (uint64_t)num, d64 = (uint64_t)den;
    if (e >= 0 ? (n64 < (d64 << (uint)e)) : ((n64 << (uint)(-e)) < d64)) e -= 1;
    e = max(e, -14);
    int sh = 10 - e;
    uint64_t n = sh >= 0 ? (n64 << (uint)sh) : n64;
    uint64_t d = sh >= 0 ? d64 : (d64 << (uint)(-sh));
    uint64_t q = n / d; uint64_t r = n - q * d;
    if (2u * r > d || (2u * r == d && (q & 1u) != 0u)) q += 1u;
    if (q >= 2048u) { q >>= 1u; e += 1; }
    uint h = e > 15 ? 0x7bffu : (q < 1024u ? (uint)q : ((((uint)(e + 15)) << 10) | ((uint)q - 1024u)));
    return h | (neg ? 0x8000u : 0u);
}
uint tb_f16_rtz(uint u) {
    uint s = (u >> 16) & 0x8000u, e = (u >> 23) & 0xffu, m = u & 0x7fffffu;
    if (e == 255u) return s | 0x7c00u | (m != 0u ? max(m >> 13, 1u) : 0u);
    if (e >= 143u) return s | 0x7bffu;
    if (e >= 113u) return s | ((e - 112u) << 10) | (m >> 13);
    uint sh = 126u - e;
    return s | ((e != 0u && sh < 32u) ? ((m | 0x800000u) >> sh) : 0u);
}
uint tb_h2f(uint h) {
    uint s = (h & 0x8000u) << 16, e = (h >> 10) & 31u, m = h & 0x3ffu;
    if (e == 31u) return s | 0x7f800000u | (m << 13);
    if (e == 0u) return s | asuint((float)m * 5.9604644775390625e-8f);
    return s | ((e + 112u) << 23) | (m << 13);
}
uint tb_d16_un(uint h, uint mx) {
    uint e = (h >> 10) & 31u, m = h & 0x3ffu;
    if ((h & 0x8000u) != 0u || (e == 31u && m != 0u) || (e == 0u && m == 0u)) return 0u;
    if (e >= 15u) return mx;
    uint M = e == 0u ? m : (m | 0x400u), sh = e == 0u ? 24u : 25u - e;
    uint p = M * mx, q = p >> sh, r = p & ((1u << sh) - 1u), hf = 1u << (sh - 1u);
    if (r > hf || (r == hf && (q & 1u) != 0u)) q += 1u;
    return q;
}
uint tb_d16_sn(uint h, uint mh, uint mask) {
    uint e = (h >> 10) & 31u, m = h & 0x3ffu;
    if (e == 31u && m != 0u) return 0u;
    uint q;
    if (e >= 15u) {
        q = mh;
    } else {
        uint M = e == 0u ? m : (m | 0x400u), sh = e == 0u ? 24u : 25u - e;
        uint p = M * mh;
        q = p >> sh;
        if ((p & ((1u << sh) - 1u)) >= (1u << (sh - 1u))) q += 1u;
    }
    return asuint((h & 0x8000u) != 0u ? -(int)q : (int)q) & mask;
}
"#;

fn unpack16(v: &str, bits: u32, num: Num) -> String {
    let mx = mask(bits);
    match num {
        Num::Unorm => format!("tb_q16(false, {v}, {mx:#x}u)"),
        Num::Snorm => {
            let mh = mask(bits - 1);
            format!("tb_q16({s} < 0, min((uint)abs({s}), {mh:#x}u), {mh:#x}u)", s = sext(v, bits))
        }
        Num::Uscaled => format!("tb_q16(false, {v}, 1u)"),
        Num::Sscaled => format!("tb_q16({s} < 0, (uint)abs({s}), 1u)", s = sext(v, bits)),
        Num::Uint => format!("min({v}, 0xffffu)"),
        Num::Sint => format!("(asuint(clamp({}, -32768, 32767)) & 0xffffu)", sext(v, bits)),
        Num::Float => match bits {
            32 => format!("tb_f16_rtz({v})"),
            16 => v.to_string(),
            _ => {
                let mb = bits - 5;
                format!("(((({v}) >> {mb}u) << 10) | ((({v}) & {:#x}u) << {}u))", mask(mb), 10 - mb)
            }
        },
    }
}

fn pack16(h: &str, bits: u32, num: Num) -> Result<String, String> {
    let mx = mask(bits);
    Ok(match num {
        Num::Unorm => format!("tb_d16_un({h}, {mx:#x}u)"),
        Num::Snorm => format!("tb_d16_sn({h}, {:#x}u, {mx:#x}u)", mask(bits - 1)),
        Num::Uint if bits == 32 => h.to_string(),
        Num::Uint => format!("min({h}, {mx:#x}u)"),
        Num::Sint => {
            let s = format!("(asint(({h}) << 16u) >> 16u)");
            if bits == 32 {
                format!("asuint({s})")
            } else {
                format!("(asuint(clamp({s}, {}, {})) & {mx:#x}u)", -(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
            }
        }
        Num::Float if bits == 32 => format!("tb_h2f({h})"),
        Num::Float if bits == 16 => h.to_string(),
        other => return Err(format!("tbuffer d16 store to a {bits}-bit {other:?} component is not lowered")),
    })
}

impl<'a> Emitter<'a> {
    pub(super) fn mtbuf_res(&mut self, ins: &Insn, slot: u32, table: u8, off: u32) -> Result<Res, String> {
        let fmt = ufmt(ins.aux.imm2 as u32).ok_or_else(|| format!("mtbuf at {:#x}: invalid buffer format {}", ins.offset, ins.aux.imm2))?;
        let writes = ins.mnemonic.starts_with("tbuffer_store") || self.uav_force.contains(&slot);
        let kind = self.kind_at(table, off, writes)?;
        let is_uav = match kind {
            Some(0) => false,
            Some(1) => true,
            other => return Err(format!("mtbuf at {:#x}: buffer in table of kind {other:?}", ins.offset)),
        };
        if slot & BINDLESS != 0 {
            if is_uav { self.bl_uav.insert(slot); } else { self.bl_srv.insert(slot); }
        }
        let end = (ins.aux.imm as u32 + fmt.bytes).div_ceil(4);
        let e = self.sbuf.entry((is_uav, slot)).or_insert(0);
        *e = (*e).max(end);
        Ok(Res::Buf(slot, is_uav))
    }

    pub(super) fn emit_mtbuf(&mut self, idx: usize, ins: &Insn) -> Result<(), String> {
        let d16 = ins.mnemonic.contains("_d16_");
        let fmt = ufmt(ins.aux.imm2 as u32).ok_or_else(|| format!("mtbuf: invalid buffer format {}", ins.aux.imm2))?;
        let n = comps(&ins.mnemonic).ok_or("mtbuf: bad op")?;
        let store = ins.mnemonic.starts_with("tbuffer_store");
        let (slot, is_uav) = match self.res.get(&idx).and_then(|v| v.first()) {
            Some(Res::Buf(s, u)) => (*s, *u),
            _ => return Err("mtbuf: unresolved buffer".into()),
        };
        if store && !is_uav {
            return Err("tbuffer_store to a non-UAV table".into());
        }
        let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
        let a0 = ins.src[0];
        let a1 = match a0 {
            Opnd::Vgpr(v) => Opnd::Vgpr(v + 1),
            other => other,
        };
        let soff = ins.src[2];
        self.touch(&a0);
        self.touch(&soff);
        let index = if idxen { r(&a0)? } else { "0u".to_string() };
        let voff = match (idxen, offen) {
            (_, false) => None,
            (false, true) => Some(r(&a0)?),
            (true, true) => {
                self.touch(&a1);
                Some(r(&a1)?)
            }
        };
        let imm = ins.aux.imm as u32;
        let konst = voff.is_none() && matches!(soff, Opnd::ConstInt(0));
        let mut byte = format!("{imm:#x}u");
        if let Some(vo) = &voff {
            byte = format!("({vo}) + {byte}");
        }
        if !matches!(soff, Opnd::ConstInt(0)) {
            byte = format!("{byte} + {}", r(&soff)?);
        }
        let vd = match ins.dst[0] {
            Opnd::Vgpr(v) => v,
            _ => return Err("mtbuf: bad vdata".into()),
        };
        let nm = if is_uav { format!("ubuf{slot}") } else { format!("sbuf{slot}") };
        let nwords = self.sbuf_words((is_uav, slot));
        let raw = self.buf_is_raw(is_uav, slot);
        if raw {
            self.raw_buf(is_uav, slot, idxen, "0u")?;
        }
        let el = |w: String| {
            if raw {
                BufRef::Raw(nm.clone(), format!("(({w}) << 2)"))
            } else {
                BufRef::Elem(format!("{nm}[{index} + ({w}) / {nwords}u].w[({w}) % {nwords}u]"))
            }
        };
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint t{t}b = {byte}; uint t{t}w = t{t}b >> 2; uint t{t}s = (t{t}b & 3u) * 8u;"));
        let nc = fmt.fields.len() as u32;
        let ndw = fmt.bytes.div_ceil(4);
        if !store {
            for k in 0..ndw {
                let lo = el(format!("t{t}w + {k}u")).read();
                if konst && imm % 4 == 0 {
                    self.line(&format!("uint t{t}_{k} = {lo};"));
                } else {
                    let hi = el(format!("t{t}w + {}u", k + 1)).read();
                    self.line(&format!("uint t{t}_{k} = ({lo} >> t{t}s) | (({hi} << (31u - t{t}s)) << 1u);"));
                }
            }
            if d16 {
                return self.emit_mtbuf_d16_load(&fmt, n, t, vd);
            }
            let one = if matches!(fmt.num, Num::Uint | Num::Sint) { "1u" } else { "0x3f800000u" };
            for k in 0..n {
                let e = if k < nc {
                    let (off, bits) = fmt.fields[k as usize];
                    let raw = if bits == 32 {
                        format!("t{t}_{}", off / 32)
                    } else {
                        format!("((t{t}_{} >> {}u) & {:#x}u)", off / 32, off % 32, mask(bits))
                    };
                    self.line(&format!("uint t{t}c{k} = {raw};"));
                    self.line(&format!("uint t{t}v{k} = {};", unpack(&format!("t{t}c{k}"), bits, fmt.num)));
                    format!("t{t}v{k}")
                } else if k == 3 {
                    one.to_string()
                } else {
                    "0u".to_string()
                };
                if k >= nc {
                    self.line(&format!("uint t{t}v{k} = {e};"));
                }
            }
            for k in 0..n {
                self.wv(&Opnd::Vgpr(vd + k as u8), &format!("t{t}v{k}"))?;
            }
            return Ok(());
        }
        let w = n.min(nc);
        let len = fmt.bytes;
        let mut words = vec![String::new(); ndw as usize];
        if d16 {
            self.uses_d16 = true;
        }
        for k in 0..w {
            let (off, bits) = fmt.fields[k as usize];
            let p = if d16 {
                let o = Opnd::Vgpr(vd + (k / 2) as u8);
                self.touch(&o);
                let h = if k % 2 == 0 { format!("({} & 0xffffu)", r(&o)?) } else { format!("({} >> 16)", r(&o)?) };
                pack16(&h, bits, fmt.num)?
            } else {
                let o = Opnd::Vgpr(vd + k as u8);
                self.touch(&o);
                pack(&r(&o)?, bits, fmt.num)?
            };
            let part = if off % 32 == 0 { format!("({p})") } else { format!("(({p}) << {}u)", off % 32) };
            let slot_w = &mut words[(off / 32) as usize];
            if slot_w.is_empty() {
                *slot_w = part;
            } else {
                *slot_w = format!("{slot_w} | {part}");
            }
        }
        let init: Vec<String> = words.iter().map(|s| if s.is_empty() { "0u".to_string() } else { s.clone() }).collect();
        let full = len / 4;
        let mut body = format!("uint t{t}p[{}] = {{ {} }}; uint t{t}j0 = 0u; ", ndw.max(1), init.join(", "));
        if full > 0 {
            let direct: String = (0..full).map(|k| format!("{} ", el(format!("t{t}w + {k}u")).store(&format!("t{t}p[{k}]")))).collect();
            body += &format!("if (t{t}s == 0u) {{ {direct}t{t}j0 = {}u; }} ", full * 4);
        }
        let ew = el(format!("t{t}x"));
        let (ew_read, ew_cas) = (ew.read(), ew.call("InterlockedCompareExchange", &format!("t{t}o, (t{t}o & ~(0xffu << t{t}q)) | (t{t}d << t{t}q), t{t}r")));
        body += &format!(
            "[allow_uav_condition] for (uint t{t}j = t{t}j0; t{t}j < {len}u; t{t}j++) {{ uint t{t}y = t{t}b + t{t}j; uint t{t}x = t{t}y >> 2; uint t{t}q = (t{t}y & 3u) * 8u; uint t{t}d = (t{t}p[t{t}j >> 2] >> ((t{t}j & 3u) * 8u)) & 0xffu; uint t{t}o = {ew_read}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 4096u; t{t}i++) {{ uint t{t}r; {ew_cas} if (t{t}r == t{t}o) break; t{t}o = t{t}r; }} }}"
        );
        self.line(&format!("if (exec_lo & 1u) {{ {body}}}"));
        Ok(())
    }

    fn emit_mtbuf_d16_load(&mut self, fmt: &Ufmt, n: u32, t: usize, vd: u8) -> Result<(), String> {
        self.uses_d16 = true;
        let nc = fmt.fields.len() as u32;
        let one = if matches!(fmt.num, Num::Uint | Num::Sint) { "1u" } else { "0x3c00u" };
        for k in 0..n {
            let e = if k < nc {
                let (off, bits) = fmt.fields[k as usize];
                let raw = if bits == 32 { format!("t{t}_{}", off / 32) } else { format!("((t{t}_{} >> {}u) & {:#x}u)", off / 32, off % 32, mask(bits)) };
                self.line(&format!("uint t{t}c{k} = {raw};"));
                unpack16(&format!("t{t}c{k}"), bits, fmt.num)
            } else if k == 3 {
                one.to_string()
            } else {
                "0u".to_string()
            };
            self.line(&format!("uint t{t}h{k} = {e};"));
        }
        for reg in 0..n.div_ceil(2) {
            let d = Opnd::Vgpr(vd + reg as u8);
            let lo = format!("t{t}h{}", reg * 2);
            let v = if reg * 2 + 1 < n {
                format!("{lo} | (t{t}h{} << 16)", reg * 2 + 1)
            } else {
                self.touch(&d);
                format!("({} & 0xffff0000u) | {lo}", r(&d)?)
            };
            self.wv(&d, &v)?;
        }
        Ok(())
    }
}
