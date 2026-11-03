use p2_script::{Objects, Resolve, Source, StoredValue};

const DEFAULT_WAVE_AMP: u8 = 0x80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaterDrop {
    /// `WDrop`, plus the internal kinds 0x40 and 0x41 (whirlies with a negative speed).
    pub kind: u8,
    pub depth: u8,
    pub x: u8,
    pub y: u8,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WetParams {
    pub wave_amp: u8,
    pub drops: Vec<WaterDrop>,
}

pub fn read_wet_params<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<WetParams, crate::Error> {
    let object = objects.read(src, export)?;
    let wave_amp = match object.get("WaveAmp", 0) {
        Some((_, &StoredValue::Byte(value))) => value,
        _ => DEFAULT_WAVE_AMP,
    };
    let count = match object.get("NumDrops", 0) {
        Some((_, &StoredValue::Int(value))) => u32::try_from(value).unwrap_or(0).min(256),
        _ => 0,
    };
    let drops = (0..count)
        .map(|index| {
            let byte = |member| match object.member("Drops", index, member) {
                Some((_, &StoredValue::Byte(value))) => value,
                _ => 0,
            };
            WaterDrop {
                kind: byte("Type"),
                depth: byte("Depth"),
                x: byte("X"),
                y: byte("Y"),
                a: byte("ByteA"),
                b: byte("ByteB"),
                c: byte("ByteC"),
                d: byte("ByteD"),
            }
        })
        .collect();
    Ok(WetParams { wave_amp, drops })
}
