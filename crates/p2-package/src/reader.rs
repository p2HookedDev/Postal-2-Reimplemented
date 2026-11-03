use crate::Error;

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) -> Result<(), Error> {
        if pos > self.data.len() {
            return Err(Error::Eof { offset: pos, needed: 0 });
        }
        self.pos = pos;
        Ok(())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let eof = Error::Eof { offset: self.pos, needed: n };
        let end = self.pos.checked_add(n).filter(|&end| end <= self.data.len()).ok_or(eof)?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    pub fn sub(&mut self, n: usize) -> Result<Self, Error> {
        let start = self.pos;
        self.bytes(n)?;
        Ok(Reader { data: &self.data[..self.pos], pos: start })
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn at_end(&self) -> bool {
        self.pos == self.data.len()
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    /// First byte holds sign (bit 7), "more" (bit 6) and 6 value bits; up to three more bytes hold 7
    /// value bits plus "more" (bit 7); a fifth byte is taken whole.
    pub fn compact(&mut self) -> Result<i32, Error> {
        let first = self.u8()?;
        let mut value = u32::from(first & 0x3f);
        if first & 0x40 != 0 {
            for shift in [6, 13, 20, 27] {
                let b = self.u8()?;
                if shift == 27 {
                    value |= u32::from(b) << shift;
                    break;
                }
                value |= u32::from(b & 0x7f) << shift;
                if b & 0x80 == 0 {
                    break;
                }
            }
        }
        let value = value.cast_signed();
        Ok(if first & 0x80 != 0 { value.wrapping_neg() } else { value })
    }

    /// `TArray` count: a compact index. Negative is invalid: retail would ask the allocator for a negative
    /// size, so it never reads as empty.
    pub fn count(&mut self) -> Result<usize, Error> {
        let offset = self.pos;
        usize::try_from(self.compact()?).map_err(|_| Error::Invalid { what: "array count", offset })
    }

    /// Compact length including the terminator; a negative length means UTF-16, otherwise one Latin-1
    /// byte per character. An unpaired surrogate becomes U+FFFD.
    pub fn fstring(&mut self) -> Result<String, Error> {
        let len = self.compact()?;
        let count = len.unsigned_abs() as usize;
        let mut text = if len < 0 {
            let units: Vec<u16> = self
                .bytes(count.saturating_mul(2))?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            self.bytes(count)?.iter().copied().map(char::from).collect()
        };
        if text.ends_with('\0') {
            text.pop();
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compact(bytes: &[u8]) -> i32 {
        let mut r = Reader::new(bytes);
        let value = r.compact().unwrap();
        assert_eq!(r.pos(), bytes.len(), "consumed bytes for {bytes:02x?}");
        value
    }

    #[test]
    fn compact_index() {
        assert_eq!(compact(&[0x00]), 0);
        assert_eq!(compact(&[0x05]), 5);
        assert_eq!(compact(&[0x81]), -1);
        assert_eq!(compact(&[0x40, 0x01]), 64);
        assert_eq!(compact(&[0xc0, 0x01]), -64);
        assert_eq!(compact(&[0x7f, 0xff, 0xff, 0xff, 0x0f]), i32::MAX);
    }

    #[test]
    fn unpaired_surrogate_is_replaced() {
        assert_eq!(Reader::new(&[0x82, 0x00, 0xD8, 0x00, 0x00]).fstring().unwrap(), "\u{FFFD}");
    }

    #[test]
    fn negative_count_is_invalid() {
        assert_eq!(Reader::new(&[0x05]).count().ok(), Some(5));
        assert!(Reader::new(&[0x81]).count().is_err());
    }

    #[test]
    fn compact_index_truncated() {
        assert!(matches!(Reader::new(&[0x40]).compact(), Err(Error::Eof { offset: 1, .. })));
    }

    #[test]
    fn fstring() {
        assert_eq!(Reader::new(b"\x05None\0").fstring().unwrap(), "None");
        assert_eq!(Reader::new(b"\x00").fstring().unwrap(), "");
        assert_eq!(Reader::new(b"\x83h\0i\0\0\0").fstring().unwrap(), "hi");
    }
}
