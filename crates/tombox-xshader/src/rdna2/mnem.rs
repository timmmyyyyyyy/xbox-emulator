//! GFX10.3 (RDNA2) opcode → mnemonic tables. Numbers follow the public RDNA2 ISA
//! reference / LLVM AMDGPU / Mesa ACO for GFX10. Opcodes not listed are reported as
//! `family<0xNN>` — never guessed.
use super::Enc;

pub fn vop2(op: u32) -> Option<&'static str> {
    Some(match op {
        0x01 => "v_cndmask_b32", 0x03 => "v_add_f32", 0x04 => "v_sub_f32", 0x05 => "v_subrev_f32",
        0x06 => "v_mac_legacy_f32", 0x07 => "v_mul_legacy_f32", 0x08 => "v_mul_f32", 0x09 => "v_mul_i32_i24",
        0x0a => "v_mul_hi_i32_i24", 0x0b => "v_mul_u32_u24", 0x0c => "v_mul_hi_u32_u24",
        0x0f => "v_min_f32", 0x10 => "v_max_f32", 0x11 => "v_min_i32", 0x12 => "v_max_i32",
        0x13 => "v_min_u32", 0x14 => "v_max_u32", 0x16 => "v_lshrrev_b32", 0x18 => "v_ashrrev_i32",
        0x1a => "v_lshlrev_b32", 0x1b => "v_and_b32", 0x1c => "v_or_b32", 0x1d => "v_xor_b32",
        0x1e => "v_xnor_b32", 0x1f => "v_mac_f32", 0x20 => "v_madmk_f32", 0x21 => "v_madak_f32",
        0x25 => "v_add_nc_u32", 0x26 => "v_sub_nc_u32", 0x27 => "v_subrev_nc_u32",
        0x28 => "v_add_co_ci_u32", 0x29 => "v_sub_co_ci_u32", 0x2a => "v_subrev_co_ci_u32",
        0x2b => "v_fmac_f32", 0x2c => "v_fmamk_f32", 0x2d => "v_fmaak_f32",
        0x2f => "v_cvt_pkrtz_f16_f32",
        0x32 => "v_add_f16", 0x33 => "v_sub_f16", 0x34 => "v_subrev_f16", 0x35 => "v_mul_f16",
        0x36 => "v_fmac_f16", 0x37 => "v_fmamk_f16", 0x38 => "v_fmaak_f16", 0x39 => "v_max_f16", 0x3a => "v_min_f16",
        0x3b => "v_ldexp_f16", 0x3c => "v_pk_fmac_f16",
        _ => return None,
    })
}

pub fn vop1(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "v_nop", 0x01 => "v_mov_b32", 0x02 => "v_readfirstlane_b32",
        0x05 => "v_cvt_f32_i32", 0x06 => "v_cvt_f32_u32", 0x07 => "v_cvt_u32_f32",
        0x08 => "v_cvt_i32_f32", 0x0a => "v_cvt_f16_f32", 0x0b => "v_cvt_f32_f16",
        0x0c => "v_cvt_rpi_i32_f32", 0x0d => "v_cvt_flr_i32_f32", 0x0e => "v_cvt_off_f32_i4",
        0x11 => "v_cvt_f32_ubyte0", 0x12 => "v_cvt_f32_ubyte1", 0x13 => "v_cvt_f32_ubyte2",
        0x14 => "v_cvt_f32_ubyte3",
        0x20 => "v_fract_f32", 0x21 => "v_trunc_f32", 0x22 => "v_ceil_f32", 0x23 => "v_rndne_f32",
        0x24 => "v_floor_f32", 0x25 => "v_exp_f32", 0x27 => "v_log_f32", 0x2a => "v_rcp_f32",
        0x2b => "v_rcp_iflag_f32", 0x2e => "v_rsq_f32", 0x33 => "v_sqrt_f32", 0x35 => "v_sin_f32",
        0x36 => "v_cos_f32", 0x37 => "v_not_b32", 0x38 => "v_bfrev_b32", 0x39 => "v_ffbh_u32",
        0x3a => "v_ffbl_b32", 0x3b => "v_ffbh_i32", 0x65 => "v_swap_b32",
        0x42 => "v_movreld_b32", 0x43 => "v_movrels_b32", 0x44 => "v_movrelsd_b32", 0x48 => "v_movrelsd_2_b32", 0x68 => "v_swaprel_b32",
        0x50 => "v_cvt_f16_u16", 0x51 => "v_cvt_f16_i16", 0x52 => "v_cvt_u16_f16", 0x53 => "v_cvt_i16_f16",
        0x54 => "v_rcp_f16", 0x55 => "v_sqrt_f16", 0x56 => "v_rsq_f16", 0x5b => "v_floor_f16",
        0x5c => "v_ceil_f16", 0x5d => "v_trunc_f16", 0x5e => "v_rndne_f16", 0x5f => "v_fract_f16",
        0x1b => "v_pipeflush", 0x3f => "v_frexp_exp_i32_f32", 0x40 => "v_frexp_mant_f32", 0x41 => "v_clrexcp",
        0x57 => "v_log_f16", 0x58 => "v_exp_f16", 0x59 => "v_frexp_mant_f16", 0x5a => "v_frexp_exp_i16_f16",
        0x60 => "v_sin_f16", 0x61 => "v_cos_f16", 0x62 => "v_sat_pk_u8_i16", 0x63 => "v_cvt_norm_i16_f16",
        0x64 => "v_cvt_norm_u16_f16",
        0x03 => "v_cvt_i32_f64", 0x04 => "v_cvt_f64_i32", 0x0f => "v_cvt_f32_f64", 0x10 => "v_cvt_f64_f32",
        0x15 => "v_cvt_u32_f64", 0x16 => "v_cvt_f64_u32", 0x17 => "v_trunc_f64", 0x18 => "v_ceil_f64",
        0x19 => "v_rndne_f64", 0x1a => "v_floor_f64", 0x2f => "v_rcp_f64", 0x31 => "v_rsq_f64", 0x34 => "v_sqrt_f64",
        0x3c => "v_frexp_exp_i32_f64", 0x3d => "v_frexp_mant_f64", 0x3e => "v_fract_f64",
        _ => return None,
    })
}

pub fn vopc(op: u32) -> Option<&'static str> {
    const F: [&str; 16] = [
        "v_cmp_f_f32", "v_cmp_lt_f32", "v_cmp_eq_f32", "v_cmp_le_f32", "v_cmp_gt_f32",
        "v_cmp_lg_f32", "v_cmp_ge_f32", "v_cmp_o_f32", "v_cmp_u_f32", "v_cmp_nge_f32",
        "v_cmp_nlg_f32", "v_cmp_ngt_f32", "v_cmp_nle_f32", "v_cmp_neq_f32", "v_cmp_nlt_f32",
        "v_cmp_tru_f32",
    ];
    const H: [&str; 16] = [
        "v_cmp_f_f16", "v_cmp_lt_f16", "v_cmp_eq_f16", "v_cmp_le_f16", "v_cmp_gt_f16",
        "v_cmp_lg_f16", "v_cmp_ge_f16", "v_cmp_o_f16", "v_cmp_u_f16", "v_cmp_nge_f16",
        "v_cmp_nlg_f16", "v_cmp_ngt_f16", "v_cmp_nle_f16", "v_cmp_neq_f16", "v_cmp_nlt_f16",
        "v_cmp_tru_f16",
    ];
    const I: [&str; 8] = [
        "v_cmp_f_i32", "v_cmp_lt_i32", "v_cmp_eq_i32", "v_cmp_le_i32", "v_cmp_gt_i32",
        "v_cmp_ne_i32", "v_cmp_ge_i32", "v_cmp_t_i32",
    ];
    const U: [&str; 8] = [
        "v_cmp_f_u32", "v_cmp_lt_u32", "v_cmp_eq_u32", "v_cmp_le_u32", "v_cmp_gt_u32",
        "v_cmp_ne_u32", "v_cmp_ge_u32", "v_cmp_t_u32",
    ];
    const FX: [&str; 16] = [
        "v_cmpx_f_f32", "v_cmpx_lt_f32", "v_cmpx_eq_f32", "v_cmpx_le_f32", "v_cmpx_gt_f32",
        "v_cmpx_lg_f32", "v_cmpx_ge_f32", "v_cmpx_o_f32", "v_cmpx_u_f32", "v_cmpx_nge_f32",
        "v_cmpx_nlg_f32", "v_cmpx_ngt_f32", "v_cmpx_nle_f32", "v_cmpx_neq_f32", "v_cmpx_nlt_f32",
        "v_cmpx_tru_f32",
    ];
    const HX: [&str; 16] = [
        "v_cmpx_f_f16", "v_cmpx_lt_f16", "v_cmpx_eq_f16", "v_cmpx_le_f16", "v_cmpx_gt_f16",
        "v_cmpx_lg_f16", "v_cmpx_ge_f16", "v_cmpx_o_f16", "v_cmpx_u_f16", "v_cmpx_nge_f16",
        "v_cmpx_nlg_f16", "v_cmpx_ngt_f16", "v_cmpx_nle_f16", "v_cmpx_neq_f16", "v_cmpx_nlt_f16",
        "v_cmpx_tru_f16",
    ];
    const IX: [&str; 8] = [
        "v_cmpx_f_i32", "v_cmpx_lt_i32", "v_cmpx_eq_i32", "v_cmpx_le_i32", "v_cmpx_gt_i32",
        "v_cmpx_ne_i32", "v_cmpx_ge_i32", "v_cmpx_t_i32",
    ];
    const UX: [&str; 8] = [
        "v_cmpx_f_u32", "v_cmpx_lt_u32", "v_cmpx_eq_u32", "v_cmpx_le_u32", "v_cmpx_gt_u32",
        "v_cmpx_ne_u32", "v_cmpx_ge_u32", "v_cmpx_t_u32",
    ];
    const D: [&str; 16] = [
        "v_cmp_f_f64", "v_cmp_lt_f64", "v_cmp_eq_f64", "v_cmp_le_f64", "v_cmp_gt_f64",
        "v_cmp_lg_f64", "v_cmp_ge_f64", "v_cmp_o_f64", "v_cmp_u_f64", "v_cmp_nge_f64",
        "v_cmp_nlg_f64", "v_cmp_ngt_f64", "v_cmp_nle_f64", "v_cmp_neq_f64", "v_cmp_nlt_f64",
        "v_cmp_tru_f64",
    ];
    const DX: [&str; 16] = [
        "v_cmpx_f_f64", "v_cmpx_lt_f64", "v_cmpx_eq_f64", "v_cmpx_le_f64", "v_cmpx_gt_f64",
        "v_cmpx_lg_f64", "v_cmpx_ge_f64", "v_cmpx_o_f64", "v_cmpx_u_f64", "v_cmpx_nge_f64",
        "v_cmpx_nlg_f64", "v_cmpx_ngt_f64", "v_cmpx_nle_f64", "v_cmpx_neq_f64", "v_cmpx_nlt_f64",
        "v_cmpx_tru_f64",
    ];
    const I64: [&str; 8] = [
        "v_cmp_f_i64", "v_cmp_lt_i64", "v_cmp_eq_i64", "v_cmp_le_i64", "v_cmp_gt_i64",
        "v_cmp_ne_i64", "v_cmp_ge_i64", "v_cmp_t_i64",
    ];
    const I64X: [&str; 8] = [
        "v_cmpx_f_i64", "v_cmpx_lt_i64", "v_cmpx_eq_i64", "v_cmpx_le_i64", "v_cmpx_gt_i64",
        "v_cmpx_ne_i64", "v_cmpx_ge_i64", "v_cmpx_t_i64",
    ];
    const U64: [&str; 8] = [
        "v_cmp_f_u64", "v_cmp_lt_u64", "v_cmp_eq_u64", "v_cmp_le_u64", "v_cmp_gt_u64",
        "v_cmp_ne_u64", "v_cmp_ge_u64", "v_cmp_t_u64",
    ];
    const U64X: [&str; 8] = [
        "v_cmpx_f_u64", "v_cmpx_lt_u64", "v_cmpx_eq_u64", "v_cmpx_le_u64", "v_cmpx_gt_u64",
        "v_cmpx_ne_u64", "v_cmpx_ge_u64", "v_cmpx_t_u64",
    ];
    const I16: [&str; 6] = ["v_cmp_lt_i16", "v_cmp_eq_i16", "v_cmp_le_i16", "v_cmp_gt_i16", "v_cmp_ne_i16", "v_cmp_ge_i16"];
    const I16X: [&str; 6] = ["v_cmpx_lt_i16", "v_cmpx_eq_i16", "v_cmpx_le_i16", "v_cmpx_gt_i16", "v_cmpx_ne_i16", "v_cmpx_ge_i16"];
    const U16: [&str; 6] = ["v_cmp_lt_u16", "v_cmp_eq_u16", "v_cmp_le_u16", "v_cmp_gt_u16", "v_cmp_ne_u16", "v_cmp_ge_u16"];
    const U16X: [&str; 6] = ["v_cmpx_lt_u16", "v_cmpx_eq_u16", "v_cmpx_le_u16", "v_cmpx_gt_u16", "v_cmpx_ne_u16", "v_cmpx_ge_u16"];
    Some(match op {
        0x00..=0x0f => F[op as usize],
        0x10..=0x1f => FX[(op - 0x10) as usize],
        0x80..=0x87 => I[(op - 0x80) as usize],
        0x90..=0x97 => IX[(op - 0x90) as usize],
        0xc0..=0xc7 => U[(op - 0xc0) as usize],
        0xd0..=0xd7 => UX[(op - 0xd0) as usize],
        0x20..=0x2f => D[(op - 0x20) as usize],
        0x30..=0x3f => DX[(op - 0x30) as usize],
        0x88 => "v_cmp_class_f32",
        0x89..=0x8e => I16[(op - 0x89) as usize],
        0x8f => "v_cmp_class_f16",
        0x98 => "v_cmpx_class_f32",
        0x99..=0x9e => I16X[(op - 0x99) as usize],
        0x9f => "v_cmpx_class_f16",
        0xa0..=0xa7 => I64[(op - 0xa0) as usize],
        0xa8 => "v_cmp_class_f64",
        0xa9..=0xae => U16[(op - 0xa9) as usize],
        0xb0..=0xb7 => I64X[(op - 0xb0) as usize],
        0xb8 => "v_cmpx_class_f64",
        0xb9..=0xbe => U16X[(op - 0xb9) as usize],
        0xe0..=0xe7 => U64[(op - 0xe0) as usize],
        0xf0..=0xf7 => U64X[(op - 0xf0) as usize],
        0xc8..=0xcf => H[(op - 0xc8) as usize],
        0xd8..=0xdf => HX[(op - 0xd8) as usize],
        0xe8..=0xef => H[(op - 0xe0) as usize],
        0xf8..=0xff => HX[(op - 0xf0) as usize],
        _ => return None,
    })
}

pub fn vop3(op: u32) -> Option<&'static str> {
    if op < 0x100 {
        return vopc(op);
    }
    if (0x100..0x140).contains(&op) {
        return if matches!(op - 0x100, 0x20 | 0x21 | 0x2c | 0x2d | 0x37 | 0x38 | 0x3c) { None } else { vop2(op - 0x100) };
    }
    if (0x180..0x200).contains(&op) {
        return if matches!(op - 0x180, 0x65 | 0x68) { None } else { vop1(op - 0x180) };
    }
    Some(match op {
        0x200 => "v_interp_p1_f32", 0x201 => "v_interp_p2_f32", 0x202 => "v_interp_mov_f32",
        0x342 => "v_interp_p1ll_f16", 0x343 => "v_interp_p1lv_f16", 0x35a => "v_interp_p2_f16",
        0x140 => "v_mad_legacy_f32", 0x141 => "v_mad_f32", 0x142 => "v_mad_i32_i24",
        0x143 => "v_mad_u32_u24", 0x144 => "v_cubeid_f32", 0x145 => "v_cubesc_f32",
        0x146 => "v_cubetc_f32", 0x147 => "v_cubema_f32", 0x148 => "v_bfe_u32", 0x149 => "v_bfe_i32",
        0x14a => "v_bfi_b32", 0x14b => "v_fma_f32", 0x14d => "v_lerp_u8", 0x14e => "v_alignbit_b32",
        0x14f => "v_alignbyte_b32", 0x150 => "v_mullit_f32", 0x151 => "v_min3_f32",
        0x152 => "v_min3_i32", 0x153 => "v_min3_u32", 0x154 => "v_max3_f32", 0x155 => "v_max3_i32",
        0x156 => "v_max3_u32", 0x157 => "v_med3_f32", 0x158 => "v_med3_i32", 0x159 => "v_med3_u32",
        0x169 => "v_mul_lo_u32", 0x16a => "v_mul_hi_u32", 0x16b => "v_mul_lo_i32", 0x16c => "v_mul_hi_i32",
        0x178 => "v_xor3_b32", 0x345 => "v_xad_u32", 0x346 => "v_lshl_add_u32", 0x347 => "v_add_lshl_u32",
        0x36d => "v_add3_u32", 0x36f => "v_lshl_or_b32", 0x371 => "v_and_or_b32", 0x372 => "v_or3_b32",
        0x30f => "v_add_co_u32", 0x310 => "v_sub_co_u32", 0x319 => "v_subrev_co_u32",
        0x344 => "v_perm_b32", 0x351 => "v_min3_f16", 0x354 => "v_max3_f16", 0x357 => "v_med3_f16",
        0x360 => "v_readlane_b32", 0x361 => "v_writelane_b32", 0x363 => "v_bfm_b32", 0x364 => "v_bcnt_u32_b32",
        0x34b => "v_fma_f16",
        0x400 => "v_pk_mad_i16", 0x401 => "v_pk_mul_lo_u16", 0x402 => "v_pk_add_i16", 0x403 => "v_pk_sub_i16",
        0x404 => "v_pk_lshlrev_b16", 0x405 => "v_pk_lshrrev_b16", 0x406 => "v_pk_ashrrev_i16",
        0x407 => "v_pk_max_i16", 0x408 => "v_pk_min_i16", 0x409 => "v_pk_mad_u16", 0x40a => "v_pk_add_u16",
        0x40b => "v_pk_sub_u16", 0x40c => "v_pk_max_u16", 0x40d => "v_pk_min_u16",
        0x40e => "v_pk_fma_f16", 0x40f => "v_pk_add_f16", 0x410 => "v_pk_mul_f16", 0x411 => "v_pk_min_f16", 0x412 => "v_pk_max_f16",
        0x421 => "v_fma_mixlo_f16", 0x422 => "v_fma_mixhi_f16",
        0x420 => "v_fma_mix_f32", // VOP3P opcode 0x20 (see rdna2.rs: VOP3P opcodes are offset by 0x400)
        0x362 => "v_ldexp_f32", 0x365 => "v_mbcnt_lo_u32_b32", 0x366 => "v_mbcnt_hi_u32_b32",
        0x2ff => "v_lshlrev_b64", 0x300 => "v_lshrrev_b64", 0x301 => "v_ashrrev_i64",
        0x15a => "v_sad_u8", 0x15b => "v_sad_hi_u8", 0x15c => "v_sad_u16", 0x15d => "v_sad_u32",
        0x15e => "v_cvt_pk_u8_f32", 0x15f => "v_div_fixup_f32", 0x368 => "v_cvt_pknorm_i16_f32", 0x369 => "v_cvt_pknorm_u16_f32",
        0x36a => "v_cvt_pk_u16_u32", 0x36b => "v_cvt_pk_i16_i32", 0x373 => "v_mad_u32_u16", 0x375 => "v_mad_i32_i16",
        0x376 => "v_sub_nc_i32", 0x37f => "v_add_nc_i32", 0x377 => "v_permlane16_b32", 0x378 => "v_permlanex16_b32",
        0x303 => "v_add_nc_u16", 0x304 => "v_sub_nc_u16", 0x305 => "v_mul_lo_u16", 0x307 => "v_lshrrev_b16",
        0x308 => "v_ashrrev_i16", 0x309 => "v_max_u16", 0x30a => "v_max_i16", 0x30b => "v_min_u16",
        0x30c => "v_min_i16", 0x30d => "v_add_nc_i16", 0x30e => "v_sub_nc_i16", 0x314 => "v_lshlrev_b16",
        0x311 => "v_pack_b32_f16", 0x312 => "v_cvt_pknorm_i16_f16", 0x313 => "v_cvt_pknorm_u16_f16",
        0x340 => "v_mad_u16", 0x35e => "v_mad_i16", 0x352 => "v_min3_i16", 0x353 => "v_min3_u16",
        0x355 => "v_max3_i16", 0x356 => "v_max3_u16", 0x358 => "v_med3_i16", 0x359 => "v_med3_u16",
        0x171 => "v_msad_u8", 0x172 => "v_qsad_pk_u16_u8", 0x173 => "v_mqsad_pk_u16_u8", 0x175 => "v_mqsad_u32_u8",
        0x14c => "v_fma_f64", 0x160 => "v_div_fixup_f64", 0x164 => "v_add_f64", 0x165 => "v_mul_f64",
        0x166 => "v_min_f64", 0x167 => "v_max_f64", 0x168 => "v_ldexp_f64", 0x16d => "v_div_scale_f32", 0x16e => "v_div_scale_f64", 0x16f => "v_div_fmas_f32", 0x170 => "v_div_fmas_f64",
        0x174 => "v_trig_preop_f64", 0x176 => "v_mad_u64_u32", 0x177 => "v_mad_i64_i32",
        _ => return None,
    })
}

pub fn sop2(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "s_add_u32", 0x01 => "s_sub_u32", 0x02 => "s_add_i32", 0x03 => "s_sub_i32",
        0x04 => "s_addc_u32", 0x05 => "s_subb_u32", 0x06 => "s_min_i32", 0x07 => "s_min_u32",
        0x08 => "s_max_i32", 0x09 => "s_max_u32", 0x0a => "s_cselect_b32", 0x0b => "s_cselect_b64",
        0x0e => "s_and_b32", 0x0f => "s_and_b64", 0x10 => "s_or_b32", 0x11 => "s_or_b64",
        0x12 => "s_xor_b32", 0x13 => "s_xor_b64", 0x14 => "s_andn2_b32", 0x15 => "s_andn2_b64",
        0x16 => "s_orn2_b32", 0x17 => "s_orn2_b64", 0x18 => "s_nand_b32", 0x19 => "s_nand_b64",
        0x1a => "s_nor_b32", 0x1b => "s_nor_b64", 0x1c => "s_xnor_b32", 0x1d => "s_xnor_b64",
        0x1e => "s_lshl_b32", 0x1f => "s_lshl_b64", 0x20 => "s_lshr_b32", 0x21 => "s_lshr_b64",
        0x22 => "s_ashr_i32", 0x23 => "s_ashr_i64", 0x24 => "s_bfm_b32", 0x25 => "s_bfm_b64",
        0x26 => "s_mul_i32", 0x27 => "s_bfe_u32", 0x28 => "s_bfe_i32", 0x29 => "s_bfe_u64",
        0x2a => "s_bfe_i64", 0x2c => "s_absdiff_i32", 0x2e => "s_lshl1_add_u32",
        0x2f => "s_lshl2_add_u32", 0x30 => "s_lshl3_add_u32", 0x31 => "s_lshl4_add_u32",
        0x32 => "s_pack_ll_b32_b16", 0x33 => "s_pack_lh_b32_b16", 0x34 => "s_pack_hh_b32_b16",
        0x35 => "s_mul_hi_u32", 0x36 => "s_mul_hi_i32",
        _ => return None,
    })
}

pub fn sop1(op: u32) -> Option<&'static str> {
    Some(match op {
        0x03 => "s_mov_b32", 0x04 => "s_mov_b64", 0x05 => "s_cmov_b32", 0x06 => "s_cmov_b64",
        0x07 => "s_not_b32", 0x08 => "s_not_b64", 0x09 => "s_wqm_b32", 0x0a => "s_wqm_b64",
        0x0b => "s_brev_b32", 0x0c => "s_brev_b64", 0x0d => "s_bcnt0_i32_b32", 0x0e => "s_bcnt0_i32_b64",
        0x11 => "s_ff0_i32_b32", 0x12 => "s_ff0_i32_b64", 0x16 => "s_flbit_i32_b64", 0x17 => "s_flbit_i32",
        0x18 => "s_flbit_i32_i64", 0x1c => "s_bitset0_b64", 0x1e => "s_bitset1_b64",
        0x38 => "s_orn1_saveexec_b64", 0x39 => "s_andn1_wrexec_b64", 0x3a => "s_andn2_wrexec_b64",
        0x40 => "s_orn2_saveexec_b32", 0x41 => "s_nand_saveexec_b32", 0x42 => "s_nor_saveexec_b32",
        0x43 => "s_xnor_saveexec_b32", 0x44 => "s_andn1_saveexec_b32", 0x45 => "s_orn1_saveexec_b32",
        0x46 => "s_andn1_wrexec_b32", 0x47 => "s_andn2_wrexec_b32",
        0x0f => "s_bcnt1_i32_b32", 0x10 => "s_bcnt1_i32_b64", 0x13 => "s_ff1_i32_b32", 0x14 => "s_ff1_i32_b64",
        0x15 => "s_flbit_i32_b32", 0x19 => "s_sext_i32_i8", 0x1a => "s_sext_i32_i16",
        0x1b => "s_bitset0_b32", 0x1d => "s_bitset1_b32",
        0x1f => "s_getpc_b64", 0x20 => "s_setpc_b64", 0x21 => "s_swappc_b64",
        0x24 => "s_and_saveexec_b64", 0x25 => "s_or_saveexec_b64", 0x26 => "s_xor_saveexec_b64",
        0x27 => "s_andn2_saveexec_b64", 0x28 => "s_orn2_saveexec_b64", 0x29 => "s_nand_saveexec_b64",
        0x2a => "s_nor_saveexec_b64", 0x2b => "s_xnor_saveexec_b64",
        0x2c => "s_quadmask_b32", 0x2d => "s_quadmask_b64",
        0x34 => "s_abs_i32", 0x37 => "s_andn1_saveexec_b64", 0x3b => "s_bitreplicate_b64_b32", 0x3c => "s_and_saveexec_b32",
        0x3d => "s_or_saveexec_b32", 0x3e => "s_xor_saveexec_b32", 0x3f => "s_andn2_saveexec_b32",
        _ => return None,
    })
}

pub fn sopk(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "s_movk_i32", 0x01 => "s_version", 0x02 => "s_cmovk_i32", 0x03 => "s_cmpk_eq_i32",
        0x04 => "s_cmpk_lg_i32", 0x05 => "s_cmpk_gt_i32", 0x06 => "s_cmpk_ge_i32",
        0x07 => "s_cmpk_lt_i32", 0x08 => "s_cmpk_le_i32", 0x09 => "s_cmpk_eq_u32",
        0x0a => "s_cmpk_lg_u32", 0x0b => "s_cmpk_gt_u32", 0x0c => "s_cmpk_ge_u32",
        0x0d => "s_cmpk_lt_u32", 0x0e => "s_cmpk_le_u32", 0x0f => "s_addk_i32", 0x10 => "s_mulk_i32",
        0x12 => "s_getreg_b32", 0x13 => "s_setreg_b32", 0x15 => "s_setreg_imm32_b32",
        0x17 => "s_waitcnt_vscnt", 0x18 => "s_waitcnt_vmcnt", 0x19 => "s_waitcnt_expcnt",
        0x1a => "s_waitcnt_lgkmcnt",
        _ => return None,
    })
}

pub fn sopc(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "s_cmp_eq_i32", 0x01 => "s_cmp_lg_i32", 0x02 => "s_cmp_gt_i32", 0x03 => "s_cmp_ge_i32",
        0x04 => "s_cmp_lt_i32", 0x05 => "s_cmp_le_i32", 0x06 => "s_cmp_eq_u32", 0x07 => "s_cmp_lg_u32",
        0x08 => "s_cmp_gt_u32", 0x09 => "s_cmp_ge_u32", 0x0a => "s_cmp_lt_u32", 0x0b => "s_cmp_le_u32",
        0x0c => "s_bitcmp0_b32", 0x0d => "s_bitcmp1_b32", 0x0e => "s_bitcmp0_b64", 0x0f => "s_bitcmp1_b64", 0x12 => "s_cmp_eq_u64", 0x13 => "s_cmp_lg_u64",
        _ => return None,
    })
}

pub fn sopp(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "s_nop", 0x01 => "s_endpgm", 0x02 => "s_branch", 0x04 => "s_cbranch_scc0",
        0x05 => "s_cbranch_scc1", 0x06 => "s_cbranch_vccz", 0x07 => "s_cbranch_vccnz",
        0x08 => "s_cbranch_execz", 0x09 => "s_cbranch_execnz", 0x0a => "s_barrier", 0x03 => "s_wakeup", 0x0b => "s_setkill",
        0x0e => "s_sleep", 0x0f => "s_setprio", 0x13 => "s_icache_inv", 0x14 => "s_incperflevel", 0x15 => "s_decperflevel",
        0x0c => "s_waitcnt", 0x10 => "s_sendmsg", 0x1f => "s_code_end", 0x20 => "s_inst_prefetch", 0x21 => "s_clause",
        0x23 => "s_waitcnt_depctr", 0x24 => "s_round_mode", 0x25 => "s_denorm_mode",
        0x0d => "s_sethalt", 0x11 => "s_sendmsghalt", 0x12 => "s_trap", 0x16 => "s_ttracedata",
        0x17 => "s_cbranch_cdbgsys", 0x18 => "s_cbranch_cdbguser", 0x19 => "s_cbranch_cdbgsys_or_user",
        0x1a => "s_cbranch_cdbgsys_and_user", 0x1b => "s_endpgm_saved", 0x1e => "s_endpgm_ordered_ps_done",
        0x22 => "s_wait_idle", 0x28 => "s_ttracedata_imm",
        _ => return None,
    })
}

pub fn smem(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "s_load_dword", 0x01 => "s_load_dwordx2", 0x02 => "s_load_dwordx4",
        0x03 => "s_load_dwordx8", 0x04 => "s_load_dwordx16", 0x08 => "s_buffer_load_dword",
        0x09 => "s_buffer_load_dwordx2", 0x0a => "s_buffer_load_dwordx4",
        0x0b => "s_buffer_load_dwordx8", 0x0c => "s_buffer_load_dwordx16",
        0x10 => "s_store_dword", 0x11 => "s_store_dwordx2", 0x12 => "s_store_dwordx4",
        0x18 => "s_buffer_store_dword", 0x19 => "s_buffer_store_dwordx2", 0x1a => "s_buffer_store_dwordx4",
        0x1f => "s_gl1_inv", 0x20 => "s_dcache_inv", 0x21 => "s_dcache_wb", 0x24 => "s_memtime", 0x25 => "s_memrealtime",
        0x26 => "s_atc_probe", 0x27 => "s_atc_probe_buffer",
        _ => return None,
    })
}

pub fn mimg(op: u32) -> Option<&'static str> {
    Some(match op {
        0x0f => "image_atomic_swap", 0x10 => "image_atomic_cmpswap", 0x11 => "image_atomic_add",
        0x12 => "image_atomic_sub", 0x14 => "image_atomic_smin", 0x15 => "image_atomic_umin",
        0x16 => "image_atomic_smax", 0x17 => "image_atomic_umax", 0x18 => "image_atomic_and",
        0x19 => "image_atomic_or", 0x1a => "image_atomic_xor", 0x1b => "image_atomic_inc",
        0x1c => "image_atomic_dec", 0x1d => "image_atomic_fcmpswap", 0x1e => "image_atomic_fmin",
        0x1f => "image_atomic_fmax",
        0x00 => "image_load", 0x01 => "image_load_mip", 0x08 => "image_store", 0x09 => "image_store_mip",
        0x02 => "image_load_pck", 0x03 => "image_load_pck_sgn", 0x04 => "image_load_mip_pck",
        0x05 => "image_load_mip_pck_sgn", 0x0a => "image_store_pck", 0x0b => "image_store_mip_pck",
        0x0e => "image_get_resinfo", 0x20 => "image_sample", 0x21 => "image_sample_cl",
        0x22 => "image_sample_d", 0x23 => "image_sample_d_cl", 0x24 => "image_sample_l",
        0x25 => "image_sample_b", 0x26 => "image_sample_b_cl", 0x27 => "image_sample_lz",
        0x28 => "image_sample_c", 0x2c => "image_sample_c_l", 0x2f => "image_sample_c_lz",
        0x30 => "image_sample_o", 0x34 => "image_sample_l_o", 0x37 => "image_sample_lz_o",
        0x40 => "image_gather4", 0x47 => "image_gather4_lz", 0x48 => "image_gather4_c",
        0x4f => "image_gather4_c_lz", 0x60 => "image_get_lod",
        0xa2 => "image_sample_d_g16", 0xa3 => "image_sample_d_cl_g16",
        0xaa => "image_sample_c_d_g16", 0xab => "image_sample_c_d_cl_g16",
        0xe6 => "image_bvh_intersect_ray", 0xe7 => "image_bvh64_intersect_ray",
        _ => return None,
    })
}

pub fn mubuf(op: u32) -> Option<&'static str> {
    Some(match op {
        0x00 => "buffer_load_format_x", 0x01 => "buffer_load_format_xy",
        0x02 => "buffer_load_format_xyz", 0x03 => "buffer_load_format_xyzw",
        0x04 => "buffer_store_format_x", 0x05 => "buffer_store_format_xy",
        0x06 => "buffer_store_format_xyz", 0x07 => "buffer_store_format_xyzw",
        0x08 => "buffer_load_ubyte", 0x09 => "buffer_load_sbyte", 0x0a => "buffer_load_ushort",
        0x26 => "buffer_load_format_d16_hi_x", 0x27 => "buffer_store_format_d16_hi_x",
        0x80 => "buffer_load_format_d16_x", 0x84 => "buffer_store_format_d16_x",
        0x0b => "buffer_load_sshort", 0x0c => "buffer_load_dword", 0x0d => "buffer_load_dwordx2",
        0x0e => "buffer_load_dwordx4", 0x0f => "buffer_load_dwordx3", 0x18 => "buffer_store_byte",
        0x19 => "buffer_store_byte_d16_hi", 0x1a => "buffer_store_short", 0x1b => "buffer_store_short_d16_hi",
        0x1c => "buffer_store_dword", 0x1d => "buffer_store_dwordx2",
        0x20 => "buffer_load_ubyte_d16", 0x21 => "buffer_load_ubyte_d16_hi", 0x22 => "buffer_load_sbyte_d16",
        0x23 => "buffer_load_sbyte_d16_hi", 0x24 => "buffer_load_short_d16", 0x25 => "buffer_load_short_d16_hi",
        0x1e => "buffer_store_dwordx4", 0x1f => "buffer_store_dwordx3",
        0x30 => "buffer_atomic_swap", 0x31 => "buffer_atomic_cmpswap", 0x32 => "buffer_atomic_add", 0x71 => "buffer_gl0_inv", 0x72 => "buffer_gl1_inv",
        0x33 => "buffer_atomic_sub", 0x35 => "buffer_atomic_smin", 0x36 => "buffer_atomic_umin",
        0x37 => "buffer_atomic_smax", 0x38 => "buffer_atomic_umax", 0x39 => "buffer_atomic_and",
        0x3a => "buffer_atomic_or", 0x3b => "buffer_atomic_xor", 0x3c => "buffer_atomic_inc",
        0x3d => "buffer_atomic_dec",
        0x50 => "buffer_atomic_swap_x2", 0x51 => "buffer_atomic_cmpswap_x2", 0x52 => "buffer_atomic_add_x2",
        0x53 => "buffer_atomic_sub_x2", 0x55 => "buffer_atomic_smin_x2", 0x56 => "buffer_atomic_umin_x2",
        0x57 => "buffer_atomic_smax_x2", 0x58 => "buffer_atomic_umax_x2", 0x59 => "buffer_atomic_and_x2",
        0x5a => "buffer_atomic_or_x2", 0x5b => "buffer_atomic_xor_x2", 0x5c => "buffer_atomic_inc_x2",
        0x5d => "buffer_atomic_dec_x2",
        _ => return None,
    })
}

pub fn lookup(enc: Enc, op: u32) -> Option<&'static str> {
    match enc {
        Enc::Vop2 => vop2(op),
        Enc::Vop1 => vop1(op),
        Enc::Vopc => vopc(op),
        Enc::Vop3 => vop3(op),
        Enc::Sop2 => sop2(op),
        Enc::Sop1 => sop1(op),
        Enc::Sopk => sopk(op),
        Enc::Sopc => sopc(op),
        Enc::Sopp => sopp(op),
        Enc::Smem => smem(op),
        Enc::Mimg => mimg(op),
        Enc::Mubuf => mubuf(op),
        Enc::Mtbuf => Some([
            "tbuffer_load_format_x", "tbuffer_load_format_xy", "tbuffer_load_format_xyz", "tbuffer_load_format_xyzw",
            "tbuffer_store_format_x", "tbuffer_store_format_xy", "tbuffer_store_format_xyz", "tbuffer_store_format_xyzw",
            "tbuffer_load_format_d16_x", "tbuffer_load_format_d16_xy", "tbuffer_load_format_d16_xyz", "tbuffer_load_format_d16_xyzw",
            "tbuffer_store_format_d16_x", "tbuffer_store_format_d16_xy", "tbuffer_store_format_d16_xyz", "tbuffer_store_format_d16_xyzw",
        ][(op & 15) as usize]),
        Enc::Exp => Some("exp"),
        Enc::Flat => match op & 0x7f {
            0x0c => Some("flat_load_dword"),
            0x0d => Some("flat_load_dwordx2"),
            0x0e => Some("flat_load_dwordx4"),
            0x0f => Some("flat_load_dwordx3"),
            0x08 => Some("flat_load_ubyte"),
            0x09 => Some("flat_load_sbyte"),
            0x0a => Some("flat_load_ushort"),
            0x0b => Some("flat_load_sshort"),
            0x20 => Some("flat_load_ubyte_d16"),
            0x21 => Some("flat_load_ubyte_d16_hi"),
            0x22 => Some("flat_load_sbyte_d16"),
            0x23 => Some("flat_load_sbyte_d16_hi"),
            0x24 => Some("flat_load_short_d16"),
            0x25 => Some("flat_load_short_d16_hi"),
            0x18 => Some("flat_store_byte"),
            0x19 => Some("flat_store_byte_d16_hi"),
            0x1a => Some("flat_store_short"),
            0x1b => Some("flat_store_short_d16_hi"),
            0x1c => Some("flat_store_dword"),
            0x1d => Some("flat_store_dwordx2"),
            0x1e => Some("flat_store_dwordx4"),
            0x1f => Some("flat_store_dwordx3"),
            _ => None,
        },
        Enc::Ds => match op {
            0x35 => Some("ds_swizzle_b32"),
            0x00 => Some("ds_add_u32"),
            0x20 => Some("ds_add_rtn_u32"),
            // Same opcode order as ds_add (0x00) .. ds_write (0x0d): min/max/and/or/xor, and +0x20 for the value-returning forms.
            0x07 => Some("ds_min_u32"), 0x08 => Some("ds_max_u32"), 0x09 => Some("ds_and_b32"),
            0x0a => Some("ds_or_b32"), 0x0b => Some("ds_xor_b32"),
            0x27 => Some("ds_min_rtn_u32"), 0x28 => Some("ds_max_rtn_u32"), 0x29 => Some("ds_and_rtn_b32"),
            0x2a => Some("ds_or_rtn_b32"), 0x2b => Some("ds_xor_rtn_b32"), 0x34 => Some("ds_wrap_rtn_b32"),
            0x0d => Some("ds_write_b32"),
            0x0e => Some("ds_write2_b32"),
            0x36 => Some("ds_read_b32"),
            0x37 => Some("ds_read2_b32"),
            0x38 => Some("ds_read2st64_b32"),
            0x0f => Some("ds_write2st64_b32"),
            0x4d => Some("ds_write_b64"), 0x4e => Some("ds_write2_b64"),
            0x4f => Some("ds_write2st64_b64"), 0x76 => Some("ds_read_b64"), 0x77 => Some("ds_read2_b64"), 0x78 => Some("ds_read2st64_b64"),
            0xde => Some("ds_write_b96"), 0xdf => Some("ds_write_b128"), 0xfe => Some("ds_read_b96"), 0xff => Some("ds_read_b128"),
            0xb2 => Some("ds_permute_b32"), 0xb3 => Some("ds_bpermute_b32"),
            0x01 => Some("ds_sub_u32"), 0x02 => Some("ds_rsub_u32"), 0x03 => Some("ds_inc_u32"), 0x04 => Some("ds_dec_u32"),
            0x05 => Some("ds_min_i32"), 0x06 => Some("ds_max_i32"), 0x10 => Some("ds_cmpst_b32"),
            0x21 => Some("ds_sub_rtn_u32"), 0x22 => Some("ds_rsub_rtn_u32"), 0x23 => Some("ds_inc_rtn_u32"), 0x24 => Some("ds_dec_rtn_u32"),
            0x25 => Some("ds_min_rtn_i32"), 0x26 => Some("ds_max_rtn_i32"), 0x2d => Some("ds_wrxchg_rtn_b32"), 0x30 => Some("ds_cmpst_rtn_b32"),
            0x0c => Some("ds_mskor_b32"), 0x2c => Some("ds_mskor_rtn_b32"), 0x2e => Some("ds_wrxchg2_rtn_b32"), 0x2f => Some("ds_wrxchg2st64_rtn_b32"),
            0x40 => Some("ds_add_u64"), 0x41 => Some("ds_sub_u64"), 0x42 => Some("ds_rsub_u64"), 0x43 => Some("ds_inc_u64"), 0x44 => Some("ds_dec_u64"),
            0x45 => Some("ds_min_i64"), 0x46 => Some("ds_max_i64"), 0x47 => Some("ds_min_u64"), 0x48 => Some("ds_max_u64"),
            0x49 => Some("ds_and_b64"), 0x4a => Some("ds_or_b64"), 0x4b => Some("ds_xor_b64"), 0x4c => Some("ds_mskor_b64"), 0x50 => Some("ds_cmpst_b64"),
            0x60 => Some("ds_add_rtn_u64"), 0x61 => Some("ds_sub_rtn_u64"), 0x62 => Some("ds_rsub_rtn_u64"), 0x63 => Some("ds_inc_rtn_u64"), 0x64 => Some("ds_dec_rtn_u64"),
            0x65 => Some("ds_min_rtn_i64"), 0x66 => Some("ds_max_rtn_i64"), 0x67 => Some("ds_min_rtn_u64"), 0x68 => Some("ds_max_rtn_u64"),
            0x69 => Some("ds_and_rtn_b64"), 0x6a => Some("ds_or_rtn_b64"), 0x6b => Some("ds_xor_rtn_b64"), 0x6c => Some("ds_mskor_rtn_b64"),
            0x6d => Some("ds_wrxchg_rtn_b64"), 0x6e => Some("ds_wrxchg2_rtn_b64"), 0x6f => Some("ds_wrxchg2st64_rtn_b64"), 0x70 => Some("ds_cmpst_rtn_b64"),
            0x18 => Some("ds_gws_sema_release_all"), 0x19 => Some("ds_gws_init"), 0x1a => Some("ds_gws_sema_v"),
            0x1b => Some("ds_gws_sema_br"), 0x1c => Some("ds_gws_sema_p"), 0x1d => Some("ds_gws_barrier"),
            0x3d => Some("ds_consume"), 0x3e => Some("ds_append"), 0x3f => Some("ds_ordered_count"),
            0x1e => Some("ds_write_b8"), 0x1f => Some("ds_write_b16"), 0xa0 => Some("ds_write_b8_d16_hi"), 0xa1 => Some("ds_write_b16_d16_hi"),
            0xa2 => Some("ds_read_u8_d16"), 0xa3 => Some("ds_read_u8_d16_hi"), 0xa4 => Some("ds_read_i8_d16"), 0xa5 => Some("ds_read_i8_d16_hi"),
            0xa6 => Some("ds_read_u16_d16"), 0xa7 => Some("ds_read_u16_d16_hi"),
            0x39 => Some("ds_read_i8"), 0x3a => Some("ds_read_u8"), 0x3b => Some("ds_read_i16"), 0x3c => Some("ds_read_u16"),
            0x80 => Some("ds_add_src2_u32"), 0x81 => Some("ds_sub_src2_u32"), 0x82 => Some("ds_rsub_src2_u32"),
            0x83 => Some("ds_inc_src2_u32"), 0x84 => Some("ds_dec_src2_u32"), 0x85 => Some("ds_min_src2_i32"),
            0x86 => Some("ds_max_src2_i32"), 0x87 => Some("ds_min_src2_u32"), 0x88 => Some("ds_max_src2_u32"),
            0x89 => Some("ds_and_src2_b32"), 0x8a => Some("ds_or_src2_b32"), 0x8b => Some("ds_xor_src2_b32"),
            0x8d => Some("ds_write_src2_b32"),
            _ => None,
        },
        Enc::Vinterp => match op {
            0 => Some("v_interp_p1_f32"),
            1 => Some("v_interp_p2_f32"),
            2 => Some("v_interp_mov_f32"),
            _ => None,
        },
        _ => None,
    }
}
