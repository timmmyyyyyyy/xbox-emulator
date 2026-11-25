pub const DIGEST_AT: usize = 4;
pub const HASHED_FROM: usize = 20;
pub const BYPASS: [u8; 16] = [1; 16];
pub const PREVIEW_BYPASS: [u8; 16] = [2; 16];

const SHIFT: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];

const SINE: [u32; 64] = [
    0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee, 0xf57c_0faf, 0x4787_c62a, 0xa830_4613, 0xfd46_9501,
    0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be, 0x6b90_1122, 0xfd98_7193, 0xa679_438e, 0x49b4_0821,
    0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa, 0xd62f_105d, 0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8,
    0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed, 0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a,
    0xfffa_3942, 0x8771_f681, 0x6d9d_6122, 0xfde5_380c, 0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70,
    0x289b_7ec6, 0xeaa1_27fa, 0xd4ef_3085, 0x0488_1d05, 0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665,
    0xf429_2244, 0x432a_ff97, 0xab94_23a7, 0xfc93_a039, 0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
    0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1, 0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb, 0xeb86_d391,
];

const START: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

fn block(state: &mut [u32; 4], bytes: &[u8]) {
    let mut x = [0u32; 16];
    for (word, chunk) in x.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    for i in 0..64 {
        let (mix, index) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((b & d) | (c & !d), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let sum = a.wrapping_add(mix).wrapping_add(x[index]).wrapping_add(SINE[i]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(SHIFT[(i / 16) * 4 + i % 4]));
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

fn bytes_of(state: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (slot, word) in out.chunks_exact_mut(4).zip(state) {
        slot.copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn digest(data: &[u8], first: u32, last: u32) -> [u8; 16] {
    let mut state = START;
    let mut blocks = data.chunks_exact(64);
    for full in &mut blocks {
        block(&mut state, full);
    }
    let rest = blocks.remainder();
    let mut tail = [0u8; 64];
    if rest.len() < 56 {
        tail[4..4 + rest.len()].copy_from_slice(rest);
        tail[4 + rest.len()] = 0x80;
    } else {
        tail[..rest.len()].copy_from_slice(rest);
        tail[rest.len()] = 0x80;
        block(&mut state, &tail);
        tail = [0u8; 64];
    }
    tail[..4].copy_from_slice(&first.to_le_bytes());
    tail[60..].copy_from_slice(&last.to_le_bytes());
    block(&mut state, &tail);
    bytes_of(state)
}

pub fn retail(data: &[u8]) -> [u8; 16] {
    let n = data.len() as u32;
    digest(data, n << 3, (n << 1) | 1)
}

pub fn debug(data: &[u8]) -> [u8; 16] {
    let n = data.len() as u32;
    digest(data, (n << 4) | 0xf, (n << 2) | 0x1000_0000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Retail,
    Debug,
    Bypass,
    PreviewBypass,
    Zero,
    Mismatch,
}

fn hashed(container: &[u8]) -> Result<&[u8], String> {
    if container.len() < 32 || container[..4] != *b"DXBC" {
        return Err("not a DXBC container".into());
    }
    let size = u32::from_le_bytes([container[24], container[25], container[26], container[27]]) as usize;
    if size != container.len() {
        return Err(format!("container size field {size:#x}, buffer {:#x} bytes", container.len()));
    }
    Ok(&container[HASHED_FROM..])
}

pub fn stored(container: &[u8]) -> Result<Stored, String> {
    let body = hashed(container)?;
    let have = &container[DIGEST_AT..HASHED_FROM];
    Ok(if have == retail(body) {
        Stored::Retail
    } else if have == debug(body) {
        Stored::Debug
    } else if have == BYPASS {
        Stored::Bypass
    } else if have == PREVIEW_BYPASS {
        Stored::PreviewBypass
    } else if have == [0u8; 16] {
        Stored::Zero
    } else {
        Stored::Mismatch
    })
}

pub fn stamp(container: &mut [u8]) -> Result<(), String> {
    let hash = retail(hashed(container)?);
    container[DIGEST_AT..HASHED_FROM].copy_from_slice(&hash);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfc1321(data: &[u8]) -> [u8; 16] {
        let mut padded = data.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&((data.len() as u64) << 3).to_le_bytes());
        let mut state = START;
        for full in padded.chunks_exact(64) {
            block(&mut state, full);
        }
        bytes_of(state)
    }

    fn hex(bytes: [u8; 16]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_block_function_is_the_md5_one() {
        assert_eq!(hex(rfc1321(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(rfc1321(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(rfc1321(b"message digest")), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(
            hex(rfc1321(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn retail_and_debug_differ_from_md5_and_from_each_other() {
        for n in [0usize, 1, 55, 56, 63, 64, 119, 120, 200] {
            let data: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            let (r, d, m) = (retail(&data), debug(&data), rfc1321(&data));
            assert!(r != d && r != m && d != m, "length {n}");
        }
    }

    fn container(parts: usize) -> Vec<u8> {
        let mut c = b"DXBC".to_vec();
        c.extend_from_slice(&[0u8; 16]);
        c.extend_from_slice(&1u32.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend((0..parts).map(|i| (i * 13 + 1) as u8));
        let size = c.len() as u32;
        c[24..28].copy_from_slice(&size.to_le_bytes());
        c
    }

    #[test]
    fn stored_names_every_kind_of_digest() {
        let mut c = container(100);
        assert_eq!(stored(&c), Ok(Stored::Zero));
        stamp(&mut c).unwrap();
        assert_eq!(stored(&c), Ok(Stored::Retail));
        let last = c.len() - 1;
        c[last] ^= 1;
        assert_eq!(stored(&c), Ok(Stored::Mismatch));
        let body = debug(&c[HASHED_FROM..]);
        c[DIGEST_AT..HASHED_FROM].copy_from_slice(&body);
        assert_eq!(stored(&c), Ok(Stored::Debug));
        c[DIGEST_AT..HASHED_FROM].copy_from_slice(&BYPASS);
        assert_eq!(stored(&c), Ok(Stored::Bypass));
        c[DIGEST_AT..HASHED_FROM].copy_from_slice(&PREVIEW_BYPASS);
        assert_eq!(stored(&c), Ok(Stored::PreviewBypass));
    }

    #[test]
    fn a_buffer_that_is_not_one_whole_container_is_refused() {
        let mut c = container(40);
        c.push(0);
        assert!(stored(&c).unwrap_err().contains("size field"));
        assert!(stamp(&mut c).unwrap_err().contains("size field"));
        c.pop();
        c[0] = b'X';
        assert!(stored(&c).unwrap_err().contains("not a DXBC container"));
        assert!(stored(&c[..20]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn retail_is_the_digest_dxc_stores_for_both_tail_layouts() {
        assert!(crate::dxc::available(), "dxcompiler.dll + dxil.dll not found");
        let (mut one_block, mut two_blocks) = (0, 0);
        for terms in 1..=96u32 {
            let body: String = (0..terms).map(|k| format!(" r = r * {}.5 + uv.x * {}.25;", k + 2, k + 3)).collect();
            let hlsl = format!("float4 main(float2 uv : TEXCOORD0) : SV_Target {{ float r = uv.y;{body} return r; }}");
            let code = crate::dxc::compile(&hlsl, "ps_6_0").unwrap();
            assert_eq!(stored(&code), Ok(Stored::Retail), "{terms} terms, {} bytes", code.len());
            if (code.len() - HASHED_FROM) % 64 < 56 {
                one_block += 1;
            } else {
                two_blocks += 1;
            }
            if one_block >= 4 && two_blocks >= 2 {
                return;
            }
        }
        panic!("not proven for both tail layouts: {one_block} containers with a one-block tail, {two_blocks} with a two-block tail");
    }
}
