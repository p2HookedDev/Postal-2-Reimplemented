use p2_package::{Export, Package, Reader};

use crate::Error;
use crate::read::{array, invalid, unsigned};

#[derive(Debug, Clone)]
pub struct Level {
    pub actors: Vec<i32>,
    pub url: Url,
    pub model: i32,
    pub time_seconds: f32,
    pub first_deleted: i32,
    /// Object references to the 16 `TextBuffer`s (unverified).
    pub text_blocks: [i32; 16],
    pub travel_info: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Url {
    pub protocol: String,
    pub host: String,
    pub map: String,
    pub portal: String,
    pub options: Vec<String>,
    pub port: i32,
    pub valid: bool,
}

pub fn read_level(pkg: &Package, file: &[u8], export: &Export) -> Result<Level, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;

    let num = unsigned(r, "actor count")?;
    r.i32()?;
    let actors = (0..num).map(|_| r.compact()).collect::<Result<_, _>>()?;
    let url = Url {
        protocol: r.fstring()?,
        host: r.fstring()?,
        map: r.fstring()?,
        portal: r.fstring()?,
        options: array(r, |r| Ok(r.fstring()?))?,
        port: r.i32()?,
        valid: r.i32()? != 0,
    };

    let model = r.compact()?;
    let time_seconds = r.f32()?;
    let first_deleted = r.compact()?;
    let mut text_blocks = [0; 16];
    for block in &mut text_blocks {
        *block = r.compact()?;
    }
    let travel_info = array(r, |r: &mut Reader| Ok((r.fstring()?, r.fstring()?)))?;

    if !r.at_end() {
        return Err(invalid("level end", r.pos()));
    }
    Ok(Level { actors, url, model, time_seconds, first_deleted, text_blocks, travel_info })
}
