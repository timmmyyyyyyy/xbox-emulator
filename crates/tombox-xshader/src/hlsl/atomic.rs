use super::bufref::BufRef;
use super::{r, Emitter};
use crate::rdna2::Opnd;

impl<'a> Emitter<'a> {
    pub(super) fn atomic_rmw(&mut self, op: &str, target: &BufRef, vdata: Opnd) -> Result<String, String> {
        self.touch(&vdata);
        let val = r(&vdata)?;
        let t = self.tmp;
        self.tmp += 1;
        if op == "cmpswap" {
            let cmp = match vdata {
                Opnd::Vgpr(n) => Opnd::Vgpr(n + 1),
                _ => return Err("bad vdata".into()),
            };
            self.touch(&cmp);
            let cas = target.call("InterlockedCompareExchange", &format!("{}, {val}, t{t}", r(&cmp)?));
            self.line(&format!("uint t{t} = 0u; if (exec_lo & 1u) {{ {cas} }}"));
            return Ok(format!("t{t}"));
        }
        let c = format!("t{t}c");
        let looped = match op {
            "smin" => Some(format!("asuint(min(asint({c}), asint({val})))")),
            "smax" => Some(format!("asuint(max(asint({c}), asint({val})))")),
            "inc" => Some(format!("(({c} >= {val}) ? 0u : ({c} + 1u))")),
            "dec" => Some(format!("((({c} == 0u) || ({c} > {val})) ? {val} : ({c} - 1u))")),
            _ => None,
        };
        if let Some(new) = looped {
            self.line(&format!(
                "uint t{t} = 0u; if (exec_lo & 1u) {{ uint {c} = {}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 1024u; t{t}i++) {{ uint t{t}p = 0u; {} if (t{t}p == {c}) break; {c} = t{t}p; }} t{t} = {c}; }}",
                target.read(),
                target.call("InterlockedCompareExchange", &format!("{c}, {new}, t{t}p"))
            ));
            return Ok(format!("t{t}"));
        }
        let (func, arg) = match op {
            "add" => ("InterlockedAdd", val),
            "sub" => ("InterlockedAdd", format!("(0u - {val})")),
            "umin" => ("InterlockedMin", val),
            "umax" => ("InterlockedMax", val),
            "and" => ("InterlockedAnd", val),
            "or" => ("InterlockedOr", val),
            "xor" => ("InterlockedXor", val),
            "swap" => ("InterlockedExchange", val),
            other => return Err(format!("atomic {other} unsupported")),
        };
        let call = target.call(func, &format!("{arg}, t{t}"));
        self.line(&format!("uint t{t} = 0u; if (exec_lo & 1u) {{ {call} }}"));
        Ok(format!("t{t}"))
    }

    pub(super) fn atomic_rmw64(&mut self, op: &str, target: &str, vd: u8) -> Result<String, String> {
        self.touch(&Opnd::Vgpr(vd));
        self.touch(&Opnd::Vgpr(vd + 1));
        let v64 = format!("(((uint64_t)v{} << 32) | (uint64_t)v{vd})", vd + 1);
        let t = self.tmp;
        self.tmp += 1;
        if op == "cmpswap" {
            self.touch(&Opnd::Vgpr(vd + 2));
            self.touch(&Opnd::Vgpr(vd + 3));
            let cmp64 = format!("(((uint64_t)v{} << 32) | (uint64_t)v{})", vd + 3, vd + 2);
            self.line(&format!("uint64_t t{t} = (uint64_t)0; if (exec_lo & 1u) {{ InterlockedCompareExchange({target}, {cmp64}, {v64}, t{t}); }}"));
            return Ok(format!("t{t}"));
        }
        let c = format!("t{t}c");
        let sign = "((uint64_t)0x80000000u << 32)";
        let looped = match op {
            "smin" => Some(format!("((({c} ^ {sign}) < ({v64} ^ {sign})) ? {c} : {v64})")),
            "smax" => Some(format!("((({c} ^ {sign}) > ({v64} ^ {sign})) ? {c} : {v64})")),
            "inc" => Some(format!("(({c} >= {v64}) ? (uint64_t)0 : ({c} + (uint64_t)1))")),
            "dec" => Some(format!("((({c} == (uint64_t)0) || ({c} > {v64})) ? {v64} : ({c} - (uint64_t)1))")),
            _ => None,
        };
        if let Some(new) = looped {
            self.line(&format!(
                "uint64_t t{t} = (uint64_t)0; if (exec_lo & 1u) {{ uint64_t {c} = {target}; [allow_uav_condition] for (uint t{t}i = 0u; t{t}i < 1024u; t{t}i++) {{ uint64_t t{t}p = (uint64_t)0; InterlockedCompareExchange({target}, {c}, {new}, t{t}p); if (t{t}p == {c}) break; {c} = t{t}p; }} t{t} = {c}; }}"
            ));
            return Ok(format!("t{t}"));
        }
        let (func, arg) = match op {
            "add" => ("InterlockedAdd", v64),
            "sub" => ("InterlockedAdd", format!("(uint64_t)0 - ({v64})")),
            "umin" => ("InterlockedMin", v64),
            "umax" => ("InterlockedMax", v64),
            "and" => ("InterlockedAnd", v64),
            "or" => ("InterlockedOr", v64),
            "xor" => ("InterlockedXor", v64),
            "swap" => ("InterlockedExchange", v64),
            other => return Err(format!("atomic {other} unsupported")),
        };
        self.line(&format!("uint64_t t{t} = (uint64_t)0; if (exec_lo & 1u) {{ {func}({target}, {arg}, t{t}); }}"));
        Ok(format!("t{t}"))
    }
}
