use crate::ObjectId;

/// Per-object engine state indexed by object id, which the store hands out in order and never reuses.
#[derive(Debug)]
pub(crate) struct ObjectTable<T> {
    slots: Vec<Option<T>>,
}

impl<T> Default for ObjectTable<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T> ObjectTable<T> {
    pub(crate) fn get(&self, id: ObjectId) -> Option<&T> {
        let slot = self.slots.get(id.0 as usize)?;
        slot.as_ref()
    }

    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Option<&mut T> {
        let slot = self.slots.get_mut(id.0 as usize)?;
        slot.as_mut()
    }

    fn grown_slot(&mut self, id: ObjectId) -> &mut Option<T> {
        let index = id.0 as usize;
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        &mut self.slots[index]
    }

    pub(crate) fn insert(&mut self, id: ObjectId, value: T) -> Option<T> {
        self.grown_slot(id).replace(value)
    }

    pub(crate) fn get_or_insert_with(&mut self, id: ObjectId, make: impl FnOnce() -> T) -> &mut T {
        let slot = self.grown_slot(id);
        slot.get_or_insert_with(make)
    }

    pub(crate) fn remove(&mut self, id: ObjectId) -> Option<T> {
        self.slots.get_mut(id.0 as usize)?.take()
    }

    /// Walks every slot: not for a per-frame path.
    pub(crate) fn count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(ObjectId) -> bool) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some() && !keep(ObjectId(index as u32)) {
                *slot = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut table = ObjectTable::default();
        assert_eq!(table.insert(ObjectId(2), 'a'), None);
        assert_eq!(table.insert(ObjectId(2), 'b'), Some('a'));
        assert_eq!(table.get(ObjectId(2)), Some(&'b'));
        assert_eq!(table.get(ObjectId(1)), None);
        assert_eq!(table.remove(ObjectId(2)), Some('b'));
        assert_eq!(table.get(ObjectId(2)), None);
    }

    #[test]
    fn grows_past_its_length() {
        let mut table = ObjectTable::default();
        table.insert(ObjectId(1), 1);
        table.insert(ObjectId(40), 40);
        assert_eq!(table.get(ObjectId(1)), Some(&1));
        assert_eq!(table.get(ObjectId(40)), Some(&40));
        assert_eq!(table.get(ObjectId(39)), None);
        assert_eq!(table.get(ObjectId(41)), None);
    }

    #[test]
    fn remove_of_an_id_never_inserted() {
        let mut table = ObjectTable::default();
        assert_eq!(table.remove(ObjectId(5)), None);
        table.insert(ObjectId(9), 9);
        assert_eq!(table.remove(ObjectId(5)), None);
        assert_eq!(table.remove(ObjectId(100)), None);
        assert_eq!(table.get(ObjectId(9)), Some(&9));
    }

    #[test]
    fn get_mut_changes_only_present_entries() {
        let mut table = ObjectTable::default();
        table.insert(ObjectId(3), 1);
        if let Some(value) = table.get_mut(ObjectId(3)) {
            *value = 2;
        }
        assert_eq!(table.get(ObjectId(3)), Some(&2));
        assert_eq!(table.get_mut(ObjectId(2)), None);
        assert_eq!(table.get_mut(ObjectId(50)), None);
    }

    #[test]
    fn get_or_insert_with_keeps_an_existing_entry() {
        let mut table = ObjectTable::default();
        assert_eq!(*table.get_or_insert_with(ObjectId(7), || 'a'), 'a');
        assert_eq!(*table.get_or_insert_with(ObjectId(7), || 'b'), 'a');
        assert_eq!(table.get(ObjectId(7)), Some(&'a'));
        assert_eq!(table.get(ObjectId(6)), None);
    }

    #[test]
    fn count_and_retain() {
        let mut table = ObjectTable::default();
        assert_eq!(table.count(), 0);
        for id in [1, 4, 5, 8] {
            table.insert(ObjectId(id), id);
        }
        table.remove(ObjectId(5));
        assert_eq!(table.count(), 3);
        let mut seen = Vec::new();
        table.retain(|id| {
            seen.push(id);
            id.0 != 4
        });
        assert_eq!(seen, [ObjectId(1), ObjectId(4), ObjectId(8)]);
        assert_eq!(table.count(), 2);
        assert_eq!(table.get(ObjectId(4)), None);
        assert_eq!(table.get(ObjectId(8)), Some(&8));
    }
}
