use std::ops::Range;

use crate::{ClassId, NameId, memory};

/// What a write moves: `written` always, `drawn` too when it lands on a property the host draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stamp {
    Written,
    Drawn,
}

/// The longest run of words compared before a write moves `drawn`: a vector, a rotator, a colour.
const COMPARED_WORDS: usize = 4;

/// Where a write lands in an object's memory: `words` words from `word`, `heaps` heap slots from `heap`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    pub(crate) word: u32,
    pub(crate) heap: u32,
    pub(crate) words: u32,
    pub(crate) heaps: u32,
}

impl Span {
    pub(crate) fn place(word: u32, heap: u32, (words, heaps): (u32, u32), dim: u32) -> Self {
        Self { word, heap, words: words * dim, heaps: heaps * dim }
    }

    pub(crate) fn of(field: &memory::Field) -> Self {
        Self::place(field.word, field.heap, (field.words, field.heaps), field.dim)
    }

    fn compared(self) -> Range<usize> {
        if self.heaps == 0 { self.word as usize..(self.word + self.words) as usize } else { 0..0 }
    }
}

/// An object. Objects that script data references but the VM has not loaded or created (textures, classes,
/// sounds, ...) are stubs: no `class`, no properties.
#[derive(Debug, Clone)]
pub struct Object {
    pub name: NameId,
    pub class_name: NameId,
    pub path: String,
    pub class: Option<ClassId>,
    /// Private, so every write passes [`Object::mem_mut`] and leaves its stamp.
    mem: memory::Mem,
    written: u64,
    drawn: u64,
}

impl Object {
    pub(crate) fn new(name: NameId, class_name: NameId, path: String, class: Option<ClassId>, mem: memory::Mem) -> Self {
        Self { name, class_name, path, class, mem, written: 0, drawn: 0 }
    }

    #[inline]
    pub(crate) fn mem(&self) -> &memory::Mem {
        &self.mem
    }

    /// The only way to the object's memory for a write. `stamp` is the store's current one
    /// ([`crate::ObjectStore::stamp`]): whoever follows the object's values reads it again only after such a write.
    #[inline]
    pub(crate) fn mem_mut(&mut self, stamp: u64, kind: Stamp) -> &mut memory::Mem {
        self.written = stamp;
        if kind == Stamp::Drawn {
            self.drawn = stamp;
        }
        &mut self.mem
    }

    /// A write at `span`: one that leaves its words as they were moves only `written`. A span that reaches the heap
    /// or is longer than a few words is not compared.
    pub(crate) fn write(&mut self, stamp: u64, kind: Stamp, span: Span, apply: impl FnOnce(&mut memory::Mem)) {
        let words = span.compared();
        let before = match kind {
            Stamp::Drawn => self.mem.words.get(words.clone()).filter(|w| !w.is_empty() && w.len() <= COMPARED_WORDS).map(|w| {
                let mut copy = [0; COMPARED_WORDS];
                copy[..w.len()].copy_from_slice(w);
                copy
            }),
            Stamp::Written => None,
        };
        apply(self.mem_mut(stamp, Stamp::Written));
        if kind == Stamp::Drawn && before.is_none_or(|before| self.mem.words.get(words.clone()) != Some(&before[..words.len()])) {
            self.drawn = stamp;
        }
    }

    /// For the `LifeSpan` countdown of a resting actor only: it runs every tick and nothing that follows stamps reads
    /// `LifeSpan`, so with a stamp no ageing actor would ever count as unwritten.
    #[inline]
    pub(crate) fn mem_mut_unstamped(&mut self) -> &mut memory::Mem {
        &mut self.mem
    }

    /// The store's stamp at the last write to the object's memory, whether or not the write changed a value.
    #[inline]
    pub fn written(&self) -> u64 {
        self.written
    }

    /// The store's stamp at the last write that changed a property the host draws from (the drawn list).
    #[inline]
    pub fn drawn(&self) -> u64 {
        self.drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object() -> Object {
        let mem = memory::Mem { words: vec![0; 4], ..Default::default() };
        Object::new(NameId::NONE, NameId::NONE, String::new(), None, mem)
    }

    fn write_float(object: &mut Object, stamp: u64, kind: Stamp, value: f32) {
        let span = Span::place(1, 0, (1, 0), 1);
        object.write(stamp, kind, span, |mem| mem.words[1] = value.to_bits());
    }

    #[test]
    fn a_write_leaves_the_stamp_and_the_life_span_countdown_does_not() {
        let mut object = object();
        assert_eq!(object.written(), 0);
        object.mem_mut(7, Stamp::Written);
        assert_eq!((object.written(), object.drawn()), (7, 0));
        object.mem_mut_unstamped();
        assert_eq!(object.written(), 7);
        object.mem_mut(9, Stamp::Drawn);
        assert_eq!((object.written(), object.drawn()), (9, 9));
    }

    #[test]
    fn a_drawn_write_that_leaves_the_words_moves_only_written() {
        let mut object = object();
        write_float(&mut object, 1, Stamp::Drawn, 0.0);
        assert_eq!((object.written(), object.drawn()), (1, 0));
        write_float(&mut object, 2, Stamp::Drawn, -0.0);
        assert_eq!((object.written(), object.drawn()), (2, 2));
        write_float(&mut object, 3, Stamp::Written, 5.0);
        assert_eq!((object.written(), object.drawn()), (3, 2));
        write_float(&mut object, 4, Stamp::Drawn, f32::NAN);
        write_float(&mut object, 5, Stamp::Drawn, f32::NAN);
        assert_eq!((object.written(), object.drawn()), (5, 4));
    }
}
