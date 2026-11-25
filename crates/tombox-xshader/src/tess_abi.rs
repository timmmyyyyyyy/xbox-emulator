pub const TB_TESS_SPACE: u32 = 4_000_006;
pub const TB_TESS_TF_REGISTER: u32 = 0;
pub const TB_TESS_CB_REGISTER: u32 = 0;
pub const TB_TESS_CB_DWORDS: u32 = 2;
pub const TB_TESS_TF_HLSL_DECL: &str = "ByteAddressBuffer tb_tf : register(t0, space4000006);";
pub const TB_TESS_CB_HLSL_DECL: &str = "cbuffer TbTess : register(b0, space4000006) { uint tb_tf_offset; uint tb_patch_offset; };";
pub const TB_TESS_GFX6_CONTROL_WORD: u32 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TfLayout {
    Gfx6To8,
    Gfx9Plus,
}

impl TfLayout {
    pub const fn factor_base(self) -> u32 {
        match self {
            TfLayout::Gfx6To8 => 4,
            TfLayout::Gfx9Plus => 0,
        }
    }
}

pub const fn factor_counts(domain: u32) -> Option<(u32, u32)> {
    match domain {
        1 => Some((2, 0)),
        2 => Some((3, 1)),
        3 => Some((4, 2)),
        _ => None,
    }
}

pub const fn patch_stride_bytes(domain: u32) -> Option<u32> {
    match factor_counts(domain) {
        Some((outer, inner)) => Some((outer + inner) * 4),
        None => None,
    }
}

pub const fn factor_byte_offset(layout: TfLayout, tf_offset: u32, rel_patch_id: u32, domain: u32, factor: u32) -> Option<u32> {
    match patch_stride_bytes(domain) {
        Some(stride) => Some(tf_offset + layout.factor_base() + rel_patch_id * stride + factor * 4),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_name_the_tess_space_and_registers() {
        assert_eq!(TB_TESS_TF_HLSL_DECL, format!("ByteAddressBuffer tb_tf : register(t{TB_TESS_TF_REGISTER}, space{TB_TESS_SPACE});"));
        assert!(TB_TESS_CB_HLSL_DECL.starts_with(&format!("cbuffer TbTess : register(b{TB_TESS_CB_REGISTER}, space{TB_TESS_SPACE})")));
        assert_eq!(TB_TESS_CB_HLSL_DECL.matches("uint ").count() as u32, TB_TESS_CB_DWORDS);
        for other in [crate::gds_abi::TB_GDS_SPACE, crate::global_abi::TB_GLOBAL_SPACE, crate::centroid_abi::TB_CENTROID_SPACE, crate::user_data_abi::TB_USER_DATA_SPACE] {
            assert_ne!(TB_TESS_SPACE, other);
        }
    }

    #[test]
    fn ring_offsets_follow_the_hardware_tess_factor_layout() {
        assert_eq!(patch_stride_bytes(1), Some(8));
        assert_eq!(patch_stride_bytes(2), Some(16));
        assert_eq!(patch_stride_bytes(3), Some(24));
        assert_eq!(patch_stride_bytes(0), None);
        assert_eq!(factor_byte_offset(TfLayout::Gfx9Plus, 0, 0, 3, 0), Some(0));
        assert_eq!(factor_byte_offset(TfLayout::Gfx9Plus, 0x100, 2, 3, 5), Some(0x100 + 48 + 20));
        assert_eq!(factor_byte_offset(TfLayout::Gfx6To8, 0x100, 2, 2, 3), Some(0x100 + 4 + 32 + 12));
        assert_eq!(factor_byte_offset(TfLayout::Gfx6To8, 0, 0, 4, 0), None);
    }
}
