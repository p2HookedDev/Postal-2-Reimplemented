//! Package reader: layouts follow POSTAL 2 (package version 118, licensee 2).

mod ini;
mod object;
mod reader;
mod resolve;

pub use ini::Ini;
pub use object::{ObjectData, Property, PropertyTag, TagValue, tag};
pub use reader::Reader;
pub use memmap2::Mmap;
pub use resolve::{LoadedPackage, Loader, PackageId, ResolveError, Resolved, import_path, map_file, search_paths};

pub const PACKAGE_TAG: u32 = 0x9e2a_83c1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unexpected end of data at offset {offset:#x} (needed {needed} bytes)")]
    Eof { offset: usize, needed: usize },
    #[error("bad package tag {0:#010x}")]
    BadTag(u32),
    #[error("unsupported package version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid {what} at offset {offset:#x}")]
    Invalid { what: &'static str, offset: usize },
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub version: u16,
    pub licensee: u16,
    pub package_flags: u32,
    pub name_count: usize,
    pub name_offset: usize,
    pub export_count: usize,
    pub export_offset: usize,
    pub import_count: usize,
    pub import_offset: usize,
    pub guid: [u8; 16],
    pub generations: Vec<Generation>,
}

#[derive(Debug, Clone, Copy)]
pub struct Generation {
    pub export_count: i32,
    pub name_count: i32,
}

#[derive(Debug, Clone)]
pub struct Name {
    pub text: String,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub class_package: usize,
    pub class_name: usize,
    /// Outer object: 0 = none, > 0 = export `n - 1`, < 0 = import `-n - 1`.
    pub package_index: i32,
    pub object_name: usize,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub class_index: i32,
    pub super_index: i32,
    pub package_index: i32,
    pub object_name: usize,
    pub object_flags: u32,
    pub serial_size: usize,
    pub serial_offset: usize,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub summary: Summary,
    pub names: Vec<Name>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
}

impl Package {
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(data);
        let summary = Summary::read(&mut r)?;

        r.seek(summary.name_offset)?;
        let names = (0..summary.name_count)
            .map(|_| Name::read(&mut r))
            .collect::<Result<Vec<_>, _>>()?;

        r.seek(summary.import_offset)?;
        let imports = (0..summary.import_count)
            .map(|_| Import::read(&mut r, names.len()))
            .collect::<Result<Vec<_>, _>>()?;

        r.seek(summary.export_offset)?;
        let exports = (0..summary.export_count)
            .map(|_| Export::read(&mut r, names.len()))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { summary, names, imports, exports })
    }

    pub fn name(&self, index: usize) -> &str {
        &self.names[index].text
    }

    pub fn object_name(&self, index: i32) -> Option<&str> {
        let name = match index {
            0 => return None,
            1.. => self.exports.get(index as usize - 1)?.object_name,
            _ => self.imports.get(index.unsigned_abs() as usize - 1)?.object_name,
        };
        Some(self.name(name))
    }

    pub fn export_in(&self, index: usize, outers: &[&str]) -> bool {
        let Some(export) = self.exports.get(index) else { return false };
        let mut outer = export.package_index;
        for expected in outers.iter().rev() {
            let parent = usize::try_from(outer).ok().and_then(|outer| outer.checked_sub(1)).and_then(|index| self.exports.get(index));
            match parent {
                Some(export) if self.name(export.object_name).eq_ignore_ascii_case(expected) => {
                    outer = export.package_index;
                }
                _ => return false,
            }
        }
        outer == 0
    }

    pub fn export_by_path(&self, path: &str) -> Option<usize> {
        let parts: Vec<&str> = path.split('.').collect();
        let (name, outers) = parts.split_last()?;
        let named = |i: &usize| self.name(self.exports[*i].object_name).eq_ignore_ascii_case(name);
        self.export_among((0..self.exports.len()).filter(named), outers)
    }

    pub(crate) fn export_among(&self, candidates: impl Iterator<Item = usize> + Clone, outers: &[&str]) -> Option<usize> {
        let exact = candidates.clone().find(|&index| self.export_in(index, outers));
        exact.or_else(|| if outers.is_empty() { candidates.clone().next() } else { None })
    }
}

impl Summary {
    fn read(r: &mut Reader) -> Result<Self, Error> {
        let tag = r.u32()?;
        if tag != PACKAGE_TAG {
            return Err(Error::BadTag(tag));
        }
        let version = r.u16()?;
        let licensee = r.u16()?;
        // Version < 68 stores a heritage GUID table instead of GUID + generations.
        // Not supported: every POSTAL 2 package is version 118.
        if version < 68 {
            return Err(Error::UnsupportedVersion(version));
        }
        let package_flags = r.u32()?;
        let name_count = non_negative(r, "name count")?;
        let name_offset = non_negative(r, "name offset")?;
        let export_count = non_negative(r, "export count")?;
        let export_offset = non_negative(r, "export offset")?;
        let import_count = non_negative(r, "import count")?;
        let import_offset = non_negative(r, "import offset")?;
        let guid = r.array()?;
        let generation_count = non_negative(r, "generation count")?;
        let generations = (0..generation_count)
            .map(|_| Ok(Generation { export_count: r.i32()?, name_count: r.i32()? }))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            version,
            licensee,
            package_flags,
            name_count,
            name_offset,
            export_count,
            export_offset,
            import_count,
            import_offset,
            guid,
            generations,
        })
    }
}

impl Name {
    fn read(r: &mut Reader) -> Result<Self, Error> {
        Ok(Self { text: r.fstring()?, flags: r.u32()? })
    }
}

impl Import {
    fn read(r: &mut Reader, name_count: usize) -> Result<Self, Error> {
        Ok(Self {
            class_package: name_ref(r, name_count)?,
            class_name: name_ref(r, name_count)?,
            package_index: r.i32()?,
            object_name: name_ref(r, name_count)?,
        })
    }
}

impl Export {
    fn read(r: &mut Reader, name_count: usize) -> Result<Self, Error> {
        let class_index = r.compact()?;
        let super_index = r.compact()?;
        let package_index = r.i32()?;
        let object_name = name_ref(r, name_count)?;
        let object_flags = r.u32()?;
        let serial_size = non_negative_compact(r, "serial size")?;
        let serial_offset = if serial_size > 0 { non_negative_compact(r, "serial offset")? } else { 0 };
        Ok(Self {
            class_index,
            super_index,
            package_index,
            object_name,
            object_flags,
            serial_size,
            serial_offset,
        })
    }
}

fn non_negative(r: &mut Reader, what: &'static str) -> Result<usize, Error> {
    let offset = r.pos();
    usize::try_from(r.i32()?).map_err(|_| Error::Invalid { what, offset })
}

fn non_negative_compact(r: &mut Reader, what: &'static str) -> Result<usize, Error> {
    let offset = r.pos();
    usize::try_from(r.compact()?).map_err(|_| Error::Invalid { what, offset })
}

pub(crate) fn name_ref(r: &mut Reader, name_count: usize) -> Result<usize, Error> {
    let offset = r.pos();
    usize::try_from(r.compact()?)
        .ok()
        .filter(|&index| index < name_count)
        .ok_or(Error::Invalid { what: "name index", offset })
}
