use super::{Emitter, BINDLESS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BufRef {
    Elem(String),
    Raw(String, String),
}

impl BufRef {
    pub(super) fn read(&self) -> String {
        match self {
            BufRef::Elem(e) => e.clone(),
            BufRef::Raw(nm, a) => format!("{nm}.Load({a})"),
        }
    }

    pub(super) fn store(&self, v: &str) -> String {
        match self {
            BufRef::Elem(e) => format!("{e} = {v};"),
            BufRef::Raw(nm, a) => format!("{nm}.Store({a}, {v});"),
        }
    }

    pub(super) fn call(&self, func: &str, args: &str) -> String {
        match self {
            BufRef::Elem(e) => format!("{func}({e}, {args});"),
            BufRef::Raw(nm, a) => format!("{nm}.{func}({a}, {args});"),
        }
    }
}

impl Emitter<'_> {
    pub(super) fn buf_is_raw(&self, is_uav: bool, slot: u32) -> bool {
        if slot & BINDLESS != 0 {
            return false;
        }
        let (reg, space) = (slot & 0xffff, slot >> 16);
        let want = if is_uav { 7 } else { 4 };
        self.ex.resources.iter().any(|p| p.res_type == want && p.space == space && p.lower <= reg && reg <= p.upper)
    }

    pub(super) fn raw_buf(&self, is_uav: bool, slot: u32, idxen: bool, byte: &str) -> Result<BufRef, String> {
        let nm = format!("{}{slot}", if is_uav { "ubuf" } else { "sbuf" });
        if idxen {
            return Err(format!("raw buffer {nm} addressed with IDXEN is unsupported (the element stride is not known)"));
        }
        Ok(BufRef::Raw(nm, format!("(({byte}) & ~3u)")))
    }
}
