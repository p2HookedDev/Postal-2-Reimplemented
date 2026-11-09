use std::borrow::Borrow;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// Map key whose hash and equality ignore ASCII case; maps keyed by it are looked up with `&name as &dyn Text`.
#[derive(Debug, Clone)]
pub(crate) struct Caseless(pub(crate) Box<str>);

pub(crate) trait Text {
    fn text(&self) -> &str;
}

impl Text for Caseless {
    fn text(&self) -> &str {
        &self.0
    }
}

impl Text for &str {
    fn text(&self) -> &str {
        self
    }
}

fn hash_folded<H: Hasher>(text: &str, state: &mut H) {
    for byte in text.bytes() {
        state.write_u8(byte.to_ascii_lowercase());
    }
}

impl Hash for dyn Text + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_folded(self.text(), state);
    }
}

impl PartialEq for dyn Text + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.text().eq_ignore_ascii_case(other.text())
    }
}

impl Eq for dyn Text + '_ {}

impl Hash for Caseless {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_folded(&self.0, state);
    }
}

impl PartialEq for Caseless {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for Caseless {}

impl<'a> Borrow<dyn Text + 'a> for Caseless {
    fn borrow(&self) -> &(dyn Text + 'a) {
        self
    }
}

pub(crate) fn caseless_cmp(a: &str, b: &str) -> Ordering {
    a.bytes().map(|byte| byte.to_ascii_lowercase()).cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()))
}
