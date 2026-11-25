use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use compat_common::record;
use tombox_xshader::hlsl::InputInterp;

pub const TRANSLATOR: &str = tombox_xshader::pack_key::TRANSLATOR_HASH;
#[used]
pub static TRANSLATOR_TAG: [u8; tombox_xshader::pack_key::TAG_LEN] = tombox_xshader::pack_key::translator_tag();
pub const KEY_VERSION: u32 = 2;

pub fn translator_tag() -> String {
    let t = unsafe { core::ptr::read_volatile(&TRANSLATOR_TAG) };
    String::from_utf8_lossy(&t).into_owned()
}
const MAGIC: &[u8; 8] = b"TBXIDX01";
const ENV_KEYS: [&str; 12] = [
    "TOMBOX_LOOP_GRAD", "TOMBOX_SHADER_COMPILER", "TOMBOX_DXC_BRANCH_FREE", "TOMBOX_DXC_CS_PARALLEL", "TOMBOX_DXC_DIR", "TOMBOX_FAST_FMA",
    "TOMBOX_XLAT_GUARD_BRANCH", "TOMBOX_GCN_SHOW", "TOMBOX_EXPORT_FMT", "TB_DFDEBUG", "XPAIR_DEBUG", "FXC",
];

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORES: AtomicU64 = AtomicU64::new(0);
static SAVED_US: AtomicU64 = AtomicU64::new(0);

fn bypass_reason_from(vars: impl Iterator<Item = (String, String)>, disabled: Option<String>) -> Option<String> {
    if disabled.as_deref().map(str::trim) == Some("0") {
        return Some("TOMBOX_XLAT_INDEX=0".into());
    }
    let knobs: Vec<String> = vars.map(|(k, _)| k).filter(|k| k.starts_with("TOMBOX_XS_") || k.starts_with("TOMBOX_CBRAW_") || k == "TOMBOX_INFER_ROOT_SIG" || k == "TOMBOX_FSSM_ROOT_SIG").collect();
    if knobs.is_empty() { None } else { Some(format!("translator debug knobs set ({})", knobs.join(", "))) }
}

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        let why = bypass_reason_from(std::env::vars(), std::env::var("TOMBOX_XLAT_INDEX").ok());
        record("d3d12_xs", "xlat_index", &match &why {
            None => format!("pre-translate index on ({}): a shader whose input bytes, render-target formats, linkage, compiler and translator build all match a stored translation is served from the shader pack without re-translating (TOMBOX_XLAT_INDEX=0 disables)", translator_tag()),
            Some(w) => format!("pre-translate index bypassed: {w}; every shader is translated"),
        });
        why.is_none()
    })
}

fn env_part() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| ENV_KEYS.iter().map(|k| format!("{k}={};", std::env::var(k).unwrap_or_default())).collect())
}

fn fnv(seed: u64, parts: &[&[u8]]) -> u64 {
    let mut h = seed;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= p.len() as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn key_with(translator: &str, compiler: &str, ctx: &str, blob: &[u8], rtfmt: &[u8; 8], link: &InputInterp) -> u128 {
    let mut l = String::new();
    for (a, b) in &link.0 {
        l.push_str(a);
        l.push('\u{1}');
        l.push_str(b);
        l.push('\u{2}');
    }
    let parts: [&[u8]; 7] = [MAGIC, translator.as_bytes(), compiler.as_bytes(), ctx.as_bytes(), rtfmt, l.as_bytes(), blob];
    let a = fnv(0xcbf2_9ce4_8422_2325, &parts);
    let b = fnv(0x84222325_cbf29ce4 ^ 0x9e37_79b9_7f4a_7c15, &parts);
    ((a as u128) << 64) | b as u128
}

pub fn key(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp, ctx: &str) -> Option<u128> {
    if !enabled() {
        return None;
    }
    if std::env::temp_dir().join("tombox_shader_override").join(format!("{hash:016x}.hlsl")).exists() {
        return None;
    }
    let ctx = format!("v{KEY_VERSION};{ctx};{}", env_part());
    Some(key_with(TRANSLATOR, tombox_xshader::dxc::identity(), &ctx, blob, rtfmt, link))
}

pub fn get(k: u128) -> Option<Vec<u8>> {
    let v = tombox_xshader::shader_cache::get(k).filter(|v| !v.is_empty());
    let n = if v.is_some() { HITS.fetch_add(1, Relaxed) } else { MISSES.fetch_add(1, Relaxed) } + 1;
    if n == 1 || n % 200 == 0 {
        record("d3d12_xs", "xlat_index", &summary());
    }
    v
}

pub fn put(k: u128, code: &[u8], translate_ms: f64) {
    if tombox_xshader::shader_cache::put(k, code).is_ok() {
        STORES.fetch_add(1, Relaxed);
    }
    SAVED_US.fetch_add((translate_ms * 1000.0) as u64, Relaxed);
}

pub fn summary() -> String {
    format!("pre-translate index: {} hits, {} misses, {} stored (translator {TRANSLATOR}, {:.0} ms of translation behind the stored entries)", HITS.load(Relaxed), MISSES.load(Relaxed), STORES.load(Relaxed), SAVED_US.load(Relaxed) as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_changes_with_every_input() {
        let blob = [1u8, 2, 3, 4];
        let rt = [0u8; 8];
        let none = InputInterp::default();
        let base = key_with("t1", "dxc a", "c", &blob, &rt, &none);
        assert_eq!(base, key_with("t1", "dxc a", "c", &blob, &rt, &none));
        assert_ne!(base, key_with("t2", "dxc a", "c", &blob, &rt, &none));
        assert_ne!(base, key_with("t1", "dxc b", "c", &blob, &rt, &none));
        assert_ne!(base, key_with("t1", "dxc a", "d", &blob, &rt, &none));
        assert_ne!(base, key_with("t1", "dxc a", "c", &[1, 2, 3, 5], &rt, &none));
        assert_ne!(base, key_with("t1", "dxc a", "c", &blob, &[1, 0, 0, 0, 0, 0, 0, 0], &none));
        let cen = InputInterp(vec![("TEXCOORD1".into(), "centroid".into())]);
        assert_ne!(base, key_with("t1", "dxc a", "c", &blob, &rt, &cen));
        assert_eq!(TRANSLATOR.len(), 16);
    }

    #[test]
    fn debug_knobs_bypass_the_index() {
        let v = |k: &str| vec![(k.to_string(), "1".to_string())].into_iter();
        assert!(bypass_reason_from(v("TOMBOX_XS_STUB"), None).is_some());
        assert!(bypass_reason_from(v("TOMBOX_XS_DROP"), None).is_some());
        assert!(bypass_reason_from(v("TOMBOX_INFER_ROOT_SIG"), None).is_some());
        assert!(bypass_reason_from(v("TOMBOX_FSSM_ROOT_SIG"), None).is_some());
        assert!(bypass_reason_from(v("TOMBOX_CBRAW_CLAMP"), None).is_some());
        assert!(bypass_reason_from(v("TOMBOX_FRAME_CSV"), None).is_none());
        assert!(bypass_reason_from(v("TOMBOX_FRAME_CSV"), Some("0".into())).is_some());
        assert!(bypass_reason_from(std::iter::empty(), Some("1".into())).is_none());
        assert!(bypass_reason_from(v("TOMBOX_XS_F16_NATIVE"), None).is_some());
    }

    #[test]
    fn env_keys_change_only_with_a_key_version_bump() {
        let unset: String = ENV_KEYS.iter().map(|k| format!("{k}=;")).collect();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in format!("v{KEY_VERSION};{unset}").bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!((KEY_VERSION, h), (2, PINNED_ENV_HASH), "ENV_KEYS or KEY_VERSION changed: bump KEY_VERSION and re-pin PINNED_ENV_HASH to {h:#018x} in the same change");
        assert!(!ENV_KEYS.iter().any(|k| k.starts_with("TOMBOX_XS_") || k.starts_with("TOMBOX_CBRAW_")), "TOMBOX_XS_* and TOMBOX_CBRAW_* bypass the index, they never belong in ENV_KEYS");
    }

    const PINNED_ENV_HASH: u64 = 0x569816d392b976da;

    #[test]
    fn translator_tag_is_embedded() {
        let t = unsafe { core::ptr::read_volatile(&TRANSLATOR_TAG) };
        assert_eq!(tombox_xshader::pack_key::find_translator_tag(&t).as_deref(), Some(TRANSLATOR));
    }
}
