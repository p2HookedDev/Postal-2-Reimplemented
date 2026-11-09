use p2_package::Reader;

use crate::Error;

pub(crate) const BOX_SIZE: usize = 25;

pub(crate) fn count(r: &mut Reader) -> Result<usize, Error> {
    Ok(r.count()?)
}

pub(crate) fn array<T>(r: &mut Reader, mut element: impl FnMut(&mut Reader) -> Result<T, Error>) -> Result<Vec<T>, Error> {
    let count = count(r)?;
    // Reserves no more than the data left could hold, whatever a damaged count says.
    let mut elements = Vec::with_capacity(count.min(r.remaining() / size_of::<T>().max(1)));
    for _ in 0..count {
        elements.push(element(r)?);
    }
    Ok(elements)
}

pub(crate) fn skip_array(r: &mut Reader, stride: usize) -> Result<(), Error> {
    let count = count(r)?;
    r.bytes(count.saturating_mul(stride))?;
    Ok(())
}

pub(crate) fn vec3(r: &mut Reader) -> Result<[f32; 3], Error> {
    Ok([r.f32()?, r.f32()?, r.f32()?])
}

pub(crate) fn vec4(r: &mut Reader) -> Result<[f32; 4], Error> {
    Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?])
}

pub(crate) fn skip_primitive(r: &mut Reader) -> Result<(), Error> {
    r.bytes(BOX_SIZE + 16)?;
    Ok(())
}

/// Lazy array (version >= 62): `INT` file offset just past the array. Jumps there without reading it.
pub(crate) fn skip_lazy_array(r: &mut Reader) -> Result<(), Error> {
    let offset = r.pos();
    let skip = usize::try_from(r.i32()?).map_err(|_| invalid("lazy array skip offset", offset))?;
    seek_forward(r, skip, offset)
}

pub(crate) fn lazy_span(r: &mut Reader) -> Result<std::ops::Range<usize>, Error> {
    let offset = r.pos();
    let skip = usize::try_from(r.i32()?).map_err(|_| invalid("lazy array skip offset", offset))?;
    let count = count(r)?;
    let start = r.pos();
    r.bytes(count)?;
    seek_forward(r, skip, offset)?;
    Ok(start..start + count)
}

/// Lazy byte array (version >= 62): `INT` file offset after the array, then compact count + bytes.
/// The engine seeks to that offset after reading.
pub(crate) fn lazy_bytes<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], Error> {
    let offset = r.pos();
    let skip = usize::try_from(r.i32()?).map_err(|_| invalid("lazy array skip offset", offset))?;
    let count = count(r)?;
    let data = r.bytes(count)?;
    seek_forward(r, skip, offset)?;
    Ok(data)
}

/// Jumps to a lazy array's skip offset (read at `offset`). It lies past the array, so never behind the reader: a
/// backward jump would let a counted loop read the same bytes without end.
fn seek_forward(r: &mut Reader, skip: usize, offset: usize) -> Result<(), Error> {
    if skip < r.pos() {
        return Err(invalid("lazy array skip offset", offset));
    }
    Ok(r.seek(skip)?)
}

pub(crate) fn invalid(what: &'static str, offset: usize) -> Error {
    p2_package::Error::Invalid { what, offset }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_skip_behind_the_reader_is_rejected() {
        let backwards = [0, 0, 0, 0, 0];
        assert!(skip_lazy_array(&mut Reader::new(&backwards)).is_err());
        assert!(lazy_bytes(&mut Reader::new(&backwards)).is_err());
        assert!(lazy_span(&mut Reader::new(&backwards)).is_err());
        let empty = [5, 0, 0, 0, 0];
        assert!(lazy_bytes(&mut Reader::new(&empty)).unwrap().is_empty());
    }
}
