use std::sync::Arc;

use thiserror::Error;

const ID_SIGNATURE: u16 = 0x1084;
const ID_INFO: u16 = 0x1001;
const ID_CODEBOOK: u16 = 0x1002;
const ID_VQ: u16 = 0x1011;
const ID_SOUND_MONO: u16 = 0x1020;
const ID_SOUND_STEREO: u16 = 0x1021;

/// The widest and tallest frame accepted: a bad info chunk must not abort on allocating its buffers.
const MAX_FRAME_SIDE: u16 = 4096;

#[derive(Clone, Copy)]
enum Code {
    Skip,
    Motion,
    Vector,
    Split,
}

/// Sample rate of `RoQ` sound chunks; the file does not store it (unverified: the rate id's tools wrote).
pub const SAMPLE_RATE: u32 = 22_050;

type Rgba = [u8; 4];
const BLACK: Rgba = [0, 0, 0, 255];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoqError {
    #[error("RoQ data ends at offset {offset:#x}: {need} bytes needed")]
    Truncated { offset: usize, need: usize },
    #[error("not a RoQ file: chunk id {id:#06x} at offset 0, expected 0x1084")]
    BadSignature { id: u16 },
    #[error("RoQ file has no info chunk (0x1001); scanned to offset {offset:#x}")]
    MissingInfo { offset: usize },
    #[error("RoQ info chunk at offset {offset:#x} gives a bad frame size {width}x{height}")]
    BadSize { offset: usize, width: u16, height: u16 },
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub rgba: Vec<u8>,
    pub audio: Vec<i16>,
    pub channels: u16,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    base: usize,
}

#[derive(Clone, Copy)]
struct Chunk {
    id: u16,
    size: u32,
    arg: u16,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], pos: usize, base: usize) -> Self {
        Self { data, pos, base }
    }

    fn offset(&self) -> usize {
        self.base + self.pos
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, need: usize) -> Result<&'a [u8], RoqError> {
        let bytes = self
            .data
            .get(self.pos..self.pos.saturating_add(need))
            .ok_or(RoqError::Truncated { offset: self.offset(), need })?;
        self.pos += need;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, RoqError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, RoqError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, RoqError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// The 8-byte chunk header: id, payload size, argument.
    fn chunk(&mut self) -> Result<Chunk, RoqError> {
        Ok(Chunk { id: self.u16()?, size: self.u32()?, arg: self.u16()? })
    }

    fn chunk_with_payload(&mut self) -> Result<(Chunk, Self), RoqError> {
        let chunk = self.chunk()?;
        let offset = self.offset();
        let payload = self.take(chunk.size as usize)?;
        Ok((chunk, Reader::new(payload, 0, offset)))
    }
}

fn read_info(mut r: Reader) -> Result<(u16, u16), RoqError> {
    let offset = r.offset();
    let (width, height) = (r.u16()?, r.u16()?);
    if width == 0 || height == 0 || width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE {
        return Err(RoqError::BadSize { offset, width, height });
    }
    Ok((width, height))
}

/// Full-range BT.601 (JPEG) YUV to RGB (unverified: the exact rounding retail's decoder uses).
fn yuv_to_rgba(y: u8, u: u8, v: u8) -> Rgba {
    let y = f32::from(y);
    let u = f32::from(u) - 128.0;
    let v = f32::from(v) - 128.0;
    let c = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    [c(y + 1.402 * v), c(y - 0.344_136 * u - 0.714_136 * v), c(y + 1.772 * u), 255]
}

fn quadrants(half: usize) -> [(usize, usize); 4] {
    [(0, 0), (half, 0), (0, half), (half, half)]
}

/// Next 2-bit block code; a fresh u16 code word is read only when the last one is used up,
/// so code words and block data interleave in the stream.
#[derive(Default)]
struct Codes {
    word: u16,
    left: u32,
}

impl Codes {
    fn next(&mut self, r: &mut Reader) -> Result<Code, RoqError> {
        if self.left == 0 {
            self.word = r.u16()?;
            self.left = 8;
        }
        self.left -= 1;
        Ok(match (self.word >> (self.left * 2)) & 3 {
            0 => Code::Skip,
            1 => Code::Motion,
            2 => Code::Vector,
            _ => Code::Split,
        })
    }
}

#[derive(Clone, Copy)]
struct Mean {
    x: i8,
    y: i8,
}

/// Motion byte to a pixel offset into the previous frame: high nibble x, low nibble y.
fn motion_vector(byte: u8, mean: Mean) -> (isize, isize) {
    (
        8 - isize::from(byte >> 4) - isize::from(mean.x),
        8 - isize::from(byte & 0x0F) - isize::from(mean.y),
    )
}

struct Video {
    width: usize,
    height: usize,
    cur: Vec<Rgba>,
    prev: Vec<Rgba>,
    cells2: [[Rgba; 4]; 256],
    cells4: [[Rgba; 16]; 256],
    frames: u64,
}

impl Video {
    fn new(width: u16, height: u16) -> Self {
        let (width, height) = (usize::from(width), usize::from(height));
        Self {
            width,
            height,
            cur: vec![BLACK; width * height],
            prev: vec![BLACK; width * height],
            cells2: [[BLACK; 4]; 256],
            cells4: [[BLACK; 16]; 256],
            frames: 0,
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        let (w, h) = (usize::from(width), usize::from(height));
        if (w, h) != (self.width, self.height) {
            (self.width, self.height, self.frames) = (w, h, 0);
            self.cur = vec![BLACK; w * h];
            self.prev = vec![BLACK; w * h];
        }
    }

    /// Codebook chunk: argument high byte = 2x2 cell count, low byte = 4x4 cell count.
    /// A zero 2x2 count means 256; a zero 4x4 count means 256 only when the payload holds bytes
    /// past the 2x2 cells (unverified against the install's files). Cells not in the chunk keep
    /// their old contents.
    fn load_codebook(&mut self, mut r: Reader, arg: u16) -> Result<(), RoqError> {
        let count2 = match arg >> 8 {
            0 => 256,
            n => usize::from(n),
        };
        let count4 = match arg & 0xFF {
            0 if count2 * 6 < r.remaining() => 256,
            n => usize::from(n),
        };
        for cell in &mut self.cells2[..count2] {
            let b = r.take(6)?;
            *cell = [0, 1, 2, 3].map(|i| yuv_to_rgba(b[i], b[4], b[5]));
        }
        for i in 0..count4 {
            let indices = r.take(4)?;
            let mut cell = [BLACK; 16];
            for (&index, (qx, qy)) in indices.iter().zip(quadrants(2)) {
                for (p, &pixel) in self.cells2[usize::from(index)].iter().enumerate() {
                    cell[(qy + p / 2) * 4 + qx + p % 2] = pixel;
                }
            }
            self.cells4[i] = cell;
        }
        Ok(())
    }

    fn put(&mut self, x: usize, y: usize, cell: &[Rgba], side: usize, scale: usize) {
        for row in 0..side * scale {
            let py = y + row;
            if py >= self.height {
                break;
            }
            for col in 0..side * scale {
                let px = x + col;
                if px >= self.width {
                    break;
                }
                self.cur[py * self.width + px] = cell[row / scale * side + col / scale];
            }
        }
    }

    /// Copies a `size` block from the previous frame at (x + dx, y + dy); a source outside the
    /// frame leaves the block as it is (unverified: what retail does there).
    fn motion(&mut self, x: usize, y: usize, size: usize, (dx, dy): (isize, isize)) {
        let (Some(sx), Some(sy)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else {
            return;
        };
        if sx.max(x) + size > self.width || sy.max(y) + size > self.height {
            return;
        }
        for row in 0..size {
            let src = (sy + row) * self.width + sx;
            let dst = (y + row) * self.width + x;
            self.cur[dst..dst + size].copy_from_slice(&self.prev[src..src + size]);
        }
    }

    fn block8(&mut self, r: &mut Reader, codes: &mut Codes, (x, y): (usize, usize), mean: Mean) -> Result<(), RoqError> {
        match codes.next(r)? {
            Code::Skip => {}
            Code::Motion => self.motion(x, y, 8, motion_vector(r.u8()?, mean)),
            Code::Vector => {
                let cell = self.cells4[usize::from(r.u8()?)];
                self.put(x, y, &cell, 4, 2);
            }
            Code::Split => {
                for (qx, qy) in quadrants(4) {
                    self.block4(r, codes, (x + qx, y + qy), mean)?;
                }
            }
        }
        Ok(())
    }

    fn block4(&mut self, r: &mut Reader, codes: &mut Codes, (x, y): (usize, usize), mean: Mean) -> Result<(), RoqError> {
        match codes.next(r)? {
            Code::Skip => {}
            Code::Motion => self.motion(x, y, 4, motion_vector(r.u8()?, mean)),
            Code::Vector => {
                let cell = self.cells4[usize::from(r.u8()?)];
                self.put(x, y, &cell, 4, 1);
            }
            Code::Split => {
                for (qx, qy) in quadrants(2) {
                    let cell = self.cells2[usize::from(r.u8()?)];
                    self.put(x + qx, y + qy, &cell, 2, 1);
                }
            }
        }
        Ok(())
    }

    /// VQ chunk: 16x16 macroblocks in raster order, each four 8x8 blocks (TL, TR, BL, BR).
    /// Decoding stops when the frame ends or the payload ends between macroblocks; blocks not reached keep their pixels.
    /// A payload ending inside a macroblock is [`RoqError::Truncated`], and the frame is lost.
    fn decode_vq(&mut self, mut r: Reader, arg: u16) -> Result<(), RoqError> {
        // The second frame starts from a copy of the first, so "skip" there shows frame 0
        // (unverified); later, "skip" keeps the frame from two frames back, as the buffers swap.
        if self.frames == 1 {
            self.cur.copy_from_slice(&self.prev);
        }
        let [low, high] = arg.to_le_bytes();
        let mean = Mean { x: high.cast_signed(), y: low.cast_signed() };
        let mut codes = Codes::default();
        let (mut x, mut y) = (0, 0);
        while r.remaining() > 0 && y < self.height {
            for (bx, by) in quadrants(8) {
                self.block8(&mut r, &mut codes, (x + bx, y + by), mean)?;
            }
            x += 16;
            if x >= self.width {
                (x, y) = (0, y + 16);
            }
        }
        std::mem::swap(&mut self.cur, &mut self.prev);
        self.frames += 1;
        Ok(())
    }
}

/// DPCM sound: each byte is a delta, bit 7 the sign and bits 0-6 squared the size, added to the
/// channel's predictor and clamped to i16. Mono: the argument is the starting predictor.
/// Stereo: argument high byte (as the high byte of an i16) starts left, low byte starts right,
/// and bytes alternate left, right.
fn decode_sound(bytes: &[u8], arg: u16, stereo: bool, out: &mut Vec<i16>) {
    let mut predictor = if stereo {
        [i32::from((arg & 0xFF00).cast_signed()), i32::from((arg << 8).cast_signed())]
    } else {
        [i32::from(arg.cast_signed()), 0]
    };
    let mut channel = 0;
    out.reserve(bytes.len());
    for &byte in bytes {
        let size = i32::from(byte & 0x7F);
        let delta = if byte & 0x80 == 0 { size * size } else { -(size * size) };
        let sample = (predictor[channel] + delta).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        predictor[channel] = sample;
        out.push(sample as i16);
        if stereo {
            channel ^= 1;
        }
    }
}

pub struct RoqDecoder {
    data: Arc<[u8]>,
    pos: usize,
    frame_rate: u16,
    video: Video,
    audio: Vec<i16>,
    channels: u16,
}

impl RoqDecoder {
    pub fn new(bytes: &[u8]) -> Result<Self, RoqError> {
        Self::from_shared(bytes.into())
    }

    pub fn from_shared(bytes: Arc<[u8]>) -> Result<Self, RoqError> {
        let mut r = Reader::new(&bytes, 0, 0);
        // The signature's size field is not a payload size (0xFFFFFFFF in id's files); nothing follows it.
        let signature = r.chunk()?;
        if signature.id != ID_SIGNATURE {
            return Err(RoqError::BadSignature { id: signature.id });
        }
        let start = r.pos;
        let (width, height) = loop {
            if r.remaining() == 0 {
                return Err(RoqError::MissingInfo { offset: r.offset() });
            }
            let (chunk, payload) = r.chunk_with_payload()?;
            if chunk.id == ID_INFO {
                break read_info(payload)?;
            }
        };
        Ok(Self {
            data: bytes,
            pos: start,
            frame_rate: signature.arg,
            video: Video::new(width, height),
            audio: Vec::new(),
            channels: 0,
        })
    }

    pub fn width(&self) -> u32 {
        self.video.width as u32
    }

    pub fn height(&self) -> u32 {
        self.video.height as u32
    }

    pub fn frame_rate(&self) -> u16 {
        self.frame_rate
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, RoqError> {
        let sound = self.next_sound()?;
        Ok(sound.map(|(audio, channels)| Frame { rgba: self.video.prev.as_flattened().to_vec(), audio, channels }))
    }

    pub fn next_sound(&mut self) -> Result<Option<(Vec<i16>, u16)>, RoqError> {
        let mut r = Reader::new(&self.data, self.pos, 0);
        while r.remaining() > 0 {
            let (chunk, payload) = r.chunk_with_payload()?;
            self.pos = r.pos;
            match chunk.id {
                ID_INFO => {
                    let (width, height) = read_info(payload)?;
                    self.video.resize(width, height);
                }
                ID_CODEBOOK => self.video.load_codebook(payload, chunk.arg)?,
                ID_VQ => {
                    self.video.decode_vq(payload, chunk.arg)?;
                    return Ok(Some((std::mem::take(&mut self.audio), std::mem::replace(&mut self.channels, 0))));
                }
                ID_SOUND_MONO | ID_SOUND_STEREO => {
                    let stereo = chunk.id == ID_SOUND_STEREO;
                    decode_sound(payload.data, chunk.arg, stereo, &mut self.audio);
                    self.channels = if stereo { 2 } else { 1 };
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = [254, 0, 0, 255];
    const GREY: Rgba = [128, 128, 128, 255];

    #[test]
    fn a_huge_frame_is_refused() {
        let mut data = vec![0x84, 0x10, 0xFF, 0xFF, 0xFF, 0xFF, 30, 0];
        data.extend(chunk(ID_INFO, 0, &[0xFF, 0xFF, 0xFF, 0xFF, 8, 0, 4, 0]));
        assert_eq!(RoqDecoder::new(&data).err(), Some(RoqError::BadSize { offset: 16, width: 0xFFFF, height: 0xFFFF }));
    }

    fn chunk(id: u16, arg: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&arg.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn movie(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x84, 0x10, 0xFF, 0xFF, 0xFF, 0xFF, 30, 0];
        out.extend(chunk(ID_INFO, 0, &[16, 0, 16, 0, 8, 0, 4, 0]));
        for c in chunks {
            out.extend_from_slice(c);
        }
        out
    }

    fn codebook() -> Vec<u8> {
        let mut p = vec![128, 128, 128, 128, 128, 128];
        p.extend([76, 76, 76, 76, 85, 255]);
        p.extend([0, 0, 0, 0, 1, 1, 1, 1]);
        chunk(ID_CODEBOOK, 0x0202, &p)
    }

    fn pixel(frame: &Frame, x: usize, y: usize) -> Rgba {
        let i = (y * 16 + x) * 4;
        frame.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn solid_colour_frame() {
        let data = movie(&[codebook(), chunk(0x1013, 0, &[1, 2, 3]), chunk(ID_VQ, 0, &[0x00, 0xAA, 1, 1, 1, 1])]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        assert_eq!((dec.width(), dec.height(), dec.frame_rate()), (16, 16, 30));
        let frame = dec.next_frame().unwrap().unwrap();
        assert_eq!(frame.rgba.len(), 16 * 16 * 4);
        assert!(frame.rgba.as_chunks::<4>().0.iter().all(|p| *p == RED));
        assert_eq!((frame.channels, frame.audio.len()), (0, 0));
        assert!(dec.next_frame().unwrap().is_none());
    }

    #[test]
    fn motion_copy_frame() {
        let first = chunk(ID_VQ, 0, &[0x00, 0xAA, 1, 0, 0, 0]);
        let second = chunk(ID_VQ, 0x0800, &[0x00, 0x10, 0x88]);
        let data = movie(&[codebook(), first, second]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        let f0 = dec.next_frame().unwrap().unwrap();
        assert_eq!((pixel(&f0, 0, 0), pixel(&f0, 8, 0)), (RED, GREY));
        let f1 = dec.next_frame().unwrap().unwrap();
        assert_eq!(pixel(&f1, 0, 0), RED, "skip keeps frame 0's pixels");
        assert_eq!((pixel(&f1, 8, 0), pixel(&f1, 15, 7)), (RED, RED));
        assert_eq!((pixel(&f1, 0, 8), pixel(&f1, 8, 8)), (GREY, GREY));
    }

    #[test]
    fn subdivided_blocks() {
        // Codes (one word): 8x8 subdivide, 4x4 subdivide, 4x4 vector, skip, skip, then three
        // skipped 8x8 blocks -> 0b11_11_10_00_00_00_00_00 = 0xF800. Data: four 2x2 cells, one 4x4 cell.
        let data = movie(&[codebook(), chunk(ID_VQ, 0, &[0x00, 0xF8, 1, 0, 0, 1, 1])]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        let f = dec.next_frame().unwrap().unwrap();
        assert_eq!([pixel(&f, 0, 0), pixel(&f, 1, 1), pixel(&f, 2, 0)], [RED, RED, GREY]);
        assert_eq!([pixel(&f, 0, 2), pixel(&f, 2, 2), pixel(&f, 3, 3)], [GREY, RED, RED]);
        assert_eq!([pixel(&f, 4, 0), pixel(&f, 7, 3)], [RED, RED]);
        assert_eq!([pixel(&f, 0, 4), pixel(&f, 8, 0)], [BLACK, BLACK]);
    }

    #[test]
    fn mono_sound() {
        let data = movie(&[
            chunk(ID_SOUND_MONO, 1000, &[0x03, 0x82, 0x7F]),
            chunk(ID_SOUND_MONO, 32000, &[0x7F]),
            chunk(ID_SOUND_MONO, (-100i16).cast_unsigned(), &[0x81]),
            codebook(),
            chunk(ID_VQ, 0, &[0x00, 0x00]),
        ]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        let f = dec.next_frame().unwrap().unwrap();
        assert_eq!(f.channels, 1);
        assert_eq!(f.audio, [1009, 1005, 17134, 32767, -101]);
    }

    #[test]
    fn stereo_sound() {
        let data = movie(&[chunk(ID_SOUND_STEREO, 0x1020, &[0x01, 0x81, 0x02, 0x82]), chunk(ID_VQ, 0, &[0x00, 0x00])]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        let f = dec.next_frame().unwrap().unwrap();
        assert_eq!(f.channels, 2);
        assert_eq!(f.audio, [4097, 8191, 4101, 8187]);
    }

    #[test]
    fn truncated_chunk_reports_offset() {
        let mut data = movie(&[]);
        data.extend_from_slice(&chunk(ID_CODEBOOK, 0x0101, &[0; 10])[..11]);
        let mut dec = RoqDecoder::new(&data).unwrap();
        assert_eq!(dec.next_frame().unwrap_err(), RoqError::Truncated { offset: 32, need: 10 });
    }

    #[test]
    fn bad_headers() {
        assert_eq!(RoqDecoder::new(&[0; 8]).err(), Some(RoqError::BadSignature { id: 0 }));
        let sig = [0x84, 0x10, 0xFF, 0xFF, 0xFF, 0xFF, 30, 0];
        assert_eq!(RoqDecoder::new(&sig).err(), Some(RoqError::MissingInfo { offset: 8 }));
    }
}
