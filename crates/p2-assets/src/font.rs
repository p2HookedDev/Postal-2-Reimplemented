use p2_package::{Export, Package};

use crate::Error;
use crate::read::{array, invalid};

#[derive(Debug, Clone)]
pub struct Font {
    pub characters: Vec<FontCharacter>,
    pub textures: Vec<i32>,
    pub kerning: i32,
    /// `CharRemap`: character code → index into `characters`, used when `is_remapped`. Sorted by code when read, equal
    /// codes in file order.
    pub remap: Vec<(u16, u16)>,
    pub is_remapped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontCharacter {
    pub start_u: i32,
    pub start_v: i32,
    pub u_size: i32,
    pub v_size: i32,
    pub texture: u8,
}

impl Font {
    pub fn glyph(&self, c: char) -> Option<&FontCharacter> {
        let code = u16::try_from(u32::from(c)).ok()?;
        let index = if self.is_remapped {
            let first = self.remap.partition_point(|&(from, _)| from < code);
            self.remap.get(first).filter(|&&(from, _)| from == code).map(|&(_, to)| to)?
        } else {
            code
        };
        self.characters.get(usize::from(index)).filter(|g| g.u_size > 0 && g.v_size > 0)
    }
}

pub fn read_font(pkg: &Package, file: &[u8], export: &Export) -> Result<Font, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let characters = array(&mut r, |r| {
        Ok(FontCharacter { start_u: r.i32()?, start_v: r.i32()?, u_size: r.i32()?, v_size: r.i32()?, texture: r.u8()? })
    })?;
    let textures = array(&mut r, |r| Ok(r.compact()?))?;
    let kerning = r.i32()?;
    let mut remap: Vec<(u16, u16)> = array(&mut r, |r| Ok((r.u16()?, r.u16()?)))?;
    remap.sort_by_key(|&(from, _)| from);
    let is_remapped = r.i32()? != 0;
    if !r.at_end() {
        return Err(invalid("font end", r.pos()));
    }
    Ok(Font { characters, textures, kerning, remap, is_remapped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_lookup() {
        let glyph = |start_u| FontCharacter { start_u, start_v: 0, u_size: 1, v_size: 1, texture: 0 };
        let font = Font { characters: vec![glyph(0), glyph(1)], textures: Vec::new(), kerning: 0, remap: vec![(65, 1), (65, 0), (67, 0)], is_remapped: true };
        assert_eq!(font.glyph('A').map(|g| g.start_u), Some(1));
        assert_eq!(font.glyph('C').map(|g| g.start_u), Some(0));
        assert!(font.glyph('B').is_none());
    }
}
