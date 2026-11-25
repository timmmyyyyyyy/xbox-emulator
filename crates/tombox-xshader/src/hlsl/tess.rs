use super::{semantic, sig_type, Emitter};
use crate::meta::{SigElement, TessInfo};
use crate::tess_abi::{factor_counts, patch_stride_bytes, TfLayout, TB_TESS_CB_HLSL_DECL, TB_TESS_TF_HLSL_DECL};
use crate::{ShaderExtract, Stage};
use std::fmt::Write as _;

pub const DS_TESS_U: &str = "v0";
pub const DS_TESS_V: &str = "v1";
pub const DS_REL_PATCH_ID: &str = "v2";
pub const DS_PATCH_ID: &str = "v3";

pub fn domain_attribute(domain: u32) -> Result<&'static str, String> {
    match domain {
        1 => Ok("isoline"),
        2 => Ok("tri"),
        3 => Ok("quad"),
        other => Err(format!("tessellator domain {other} unsupported")),
    }
}

pub fn is_tess_factor(e: &SigElement) -> bool {
    (11..=16).contains(&e.sysval)
}

pub fn patch_struct(domain: u32, sig: &[SigElement]) -> Result<String, String> {
    let (outer, inner) = factor_counts(domain).ok_or_else(|| format!("tessellator domain {domain} unsupported"))?;
    let mut h = String::from("struct SPatch {\n");
    let _ = writeln!(h, "    float tb_tf0[{outer}] : SV_TessFactor;");
    match inner {
        0 => {}
        1 => {
            let _ = writeln!(h, "    float tb_tf1 : SV_InsideTessFactor;");
        }
        n => {
            let _ = writeln!(h, "    float tb_tf1[{n}] : SV_InsideTessFactor;");
        }
    }
    for (ei, e) in sig.iter().enumerate().filter(|(_, e)| !is_tess_factor(e)) {
        let (ty, _) = sig_type(e);
        let _ = writeln!(h, "    {ty} p{ei} : {};", semantic(e));
    }
    h.push_str("};\n");
    Ok(h)
}

pub fn control_point_struct(sig: &[SigElement]) -> String {
    let mut h = String::from("struct SCp {\n");
    if sig.is_empty() {
        h.push_str("    uint tb_pad : TBPAD;\n");
    }
    for (ei, e) in sig.iter().enumerate() {
        let (ty, _) = sig_type(e);
        let _ = writeln!(h, "    {ty} i{ei} : {};", semantic(e));
    }
    h.push_str("};\n");
    h
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Partitioning {
    Integer,
    Pow2,
    FractionalOdd,
    FractionalEven,
}

impl Partitioning {
    pub fn attribute(self) -> &'static str {
        match self {
            Partitioning::Integer => "integer",
            Partitioning::Pow2 => "pow2",
            Partitioning::FractionalOdd => "fractional_odd",
            Partitioning::FractionalEven => "fractional_even",
        }
    }
}

pub fn output_topology_attribute(domain: u32, output_primitive: u32) -> Result<&'static str, String> {
    match (domain, output_primitive) {
        (1..=3, 1) => Ok("point"),
        (1, 2) => Ok("line"),
        (2 | 3, 3) => Ok("triangle_cw"),
        (2 | 3, 4) => Ok("triangle_ccw"),
        _ => Err(format!("tessellator output primitive {output_primitive} is not valid for domain {domain}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassthroughHull {
    pub vs: String,
    pub hs: String,
}

pub const PASSTHROUGH_VS: &str = "struct TbVsOut { uint id : TBVERTEXID; };\nTbVsOut main(uint id : SV_VertexID) { TbVsOut o; o.id = id; return o; }\n";

pub fn passthrough_hull(hs: &TessInfo, ds: &ShaderExtract, partitioning: Partitioning, layout: TfLayout) -> Result<PassthroughHull, String> {
    if hs.stage != 3 {
        return Err("pass-through hull needs hull-shader PSV0 tessellator info".into());
    }
    if ds.stage != Stage::Domain {
        return Err(format!("pass-through hull feeds a domain shader, not {:?}", ds.stage));
    }
    let dt = ds.gs_info.and_then(|g| g.tess).filter(|t| t.stage == 4).ok_or("Domain shader without PSV0 tessellator info")?;
    if dt.domain != hs.domain {
        return Err(format!("hull domain {} differs from domain shader domain {}", hs.domain, dt.domain));
    }
    if !ds.input_sig.is_empty() && dt.input_control_points != hs.output_control_points {
        return Err(format!("hull outputs {} control points, domain shader reads {}", hs.output_control_points, dt.input_control_points));
    }
    let domain = domain_attribute(hs.domain)?;
    let topology = output_topology_attribute(hs.domain, hs.output_primitive)?;
    let (outer, inner) = factor_counts(hs.domain).ok_or("unsupported domain")?;
    let stride = patch_stride_bytes(hs.domain).ok_or("unsupported domain")?;
    let mut h = String::new();
    let _ = writeln!(h, "{TB_TESS_TF_HLSL_DECL}\n{TB_TESS_CB_HLSL_DECL}");
    h.push_str("struct TbVsOut { uint id : TBVERTEXID; };\n");
    h.push_str(&control_point_struct(&ds.input_sig));
    h.push_str(&patch_struct(hs.domain, &ds.prim_sig)?);
    let nin = hs.input_control_points;
    let _ = writeln!(h, "SPatch tb_pcf(InputPatch<TbVsOut, {nin}> ip, uint pid : SV_PrimitiveID) {{");
    h.push_str("    SPatch o = (SPatch)0;\n");
    let _ = writeln!(h, "    uint b = tb_tf_offset + {}u + (tb_patch_offset + pid) * {stride}u;", layout.factor_base());
    for k in 0..outer {
        let _ = writeln!(h, "    o.tb_tf0[{k}] = asfloat(tb_tf.Load(b + {}u));", 4 * k);
    }
    for k in 0..inner {
        let dst = if inner == 1 { "o.tb_tf1".to_string() } else { format!("o.tb_tf1[{k}]") };
        let _ = writeln!(h, "    {dst} = asfloat(tb_tf.Load(b + {}u));", 4 * (outer + k));
    }
    h.push_str("    return o;\n}\n");
    let _ = writeln!(
        h,
        "[domain(\"{domain}\")]\n[partitioning(\"{}\")]\n[outputtopology(\"{topology}\")]\n[outputcontrolpoints({})]\n[patchconstantfunc(\"tb_pcf\")]\n[maxtessfactor(64.0)]",
        partitioning.attribute(),
        hs.output_control_points
    );
    let _ = writeln!(h, "SCp main(InputPatch<TbVsOut, {nin}> ip, uint i : SV_OutputControlPointID) {{ return (SCp)0; }}");
    Ok(PassthroughHull { vs: PASSTHROUGH_VS.to_string(), hs: h })
}

impl<'a> Emitter<'a> {
    pub(super) fn is_ds(&self) -> bool {
        self.ex.stage == Stage::Domain
    }

    pub(super) fn tess_info(&self) -> Result<TessInfo, String> {
        let want = if self.is_ds() { 4 } else { 3 };
        self.ex
            .gs_info
            .and_then(|g| g.tess)
            .filter(|t| t.stage == want)
            .ok_or_else(|| format!("{:?} shader without PSV0 tessellator info", self.ex.stage))
    }

    pub(super) fn ds_check(&self) -> Result<(), String> {
        self.tess_info()?;
        if self.ex.output_sig.is_empty() {
            return Err("domain shader without outputs unsupported".into());
        }
        Ok(())
    }

    pub(super) fn ds_entry(&self) -> Result<String, String> {
        let t = self.tess_info()?;
        let loc_ty = if t.domain == 2 { "float3" } else { "float2" };
        let mut params = vec![format!("{loc_ty} tb_loc : SV_DomainLocation"), "uint tb_pid : SV_PrimitiveID".to_string()];
        if !self.ex.input_sig.is_empty() {
            params.push(format!("const OutputPatch<SIn, {}> i", t.input_control_points));
        }
        let patch = patch_struct(t.domain, &self.ex.prim_sig)?;
        params.push("SPatch tb_pc".to_string());
        Ok(format!(
            "{patch}[domain(\"{}\")]\nSOut main({}) {{\n    SOut o = (SOut)0;\n",
            domain_attribute(t.domain)?,
            params.join(", ")
        ))
    }

    pub(super) fn ds_init(&mut self) -> String {
        let mut init = String::new();
        let seeds = [
            (DS_TESS_U, "asuint(tb_loc.x)"),
            (DS_TESS_V, "asuint(tb_loc.y)"),
            (DS_REL_PATCH_ID, "tb_pid"),
            (DS_PATCH_ID, "tb_pid"),
        ];
        for (v, val) in seeds {
            if self.live.iter().any(|l| l == v) {
                self.regs.insert(v.to_string());
                let _ = writeln!(init, "    {v} = {val};");
            }
        }
        init
    }
}
