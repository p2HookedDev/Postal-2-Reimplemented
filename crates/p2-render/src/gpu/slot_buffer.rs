use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::render::storage::ShaderBuffer;

pub(crate) struct SlotBuffer<T> {
    handle: Handle<ShaderBuffer>,
    records: Vec<T>,
    free: Vec<u32>,
    used: u32,
    dirty: bool,
}

impl<T: bytemuck::NoUninit + Default + PartialEq> SlotBuffer<T> {
    pub(crate) fn new(handle: Handle<ShaderBuffer>, slots: usize) -> Self {
        Self { handle, records: vec![T::default(); slots], free: Vec::new(), used: 0, dirty: true }
    }

    pub(crate) fn take(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.used += 1;
            if self.used as usize > self.records.len() {
                self.records.resize((self.records.len() * 2).max(self.used as usize), T::default());
                self.dirty = true;
            }
            self.used - 1
        })
    }

    pub(crate) fn free(&mut self, slot: u32) {
        self.free.push(slot);
    }

    pub(crate) fn set(&mut self, slot: u32, record: T) {
        if let Some(held) = self.records.get_mut(slot as usize)
            && *held != record
        {
            *held = record;
            self.dirty = true;
        }
    }

    pub(crate) fn upload(&mut self, buffers: &mut Assets<ShaderBuffer>) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let id = self.handle.id();
        if !buffers.contains(id) {
            if let Err(error) = buffers.insert(id, ShaderBuffer::new(self.records.clone(), RenderAssetUsages::default())) {
                bevy::log::error!("slot buffer: {error}");
            }
        } else if let Some(mut buffer) = buffers.get_mut(id) {
            buffer.clear();
            buffer.extend_from_slice(&self.records);
        }
    }

    #[cfg(test)]
    pub(crate) fn records(&self) -> &[T] {
        &self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::uuid_handle;

    const BUFFER: Handle<ShaderBuffer> = uuid_handle!("3f6b2d90-81c4-4e57-a0d3-5c19e7b8a246");

    fn uploaded(buffers: &Assets<ShaderBuffer>) -> Vec<[f32; 4]> {
        buffers.get(BUFFER.id()).and_then(|buffer| buffer.cast_slice::<[f32; 4]>()).unwrap().to_vec()
    }

    #[test]
    fn a_slot_given_back_is_taken_again_before_a_new_one() {
        let mut slots = SlotBuffer::<[f32; 4]>::new(BUFFER, 4);
        assert_eq!(slots.take(), 0);
        assert_eq!(slots.take(), 1);
        slots.free(0);
        assert_eq!(slots.take(), 0);
        assert_eq!(slots.take(), 2);
        assert_eq!(slots.used, 3);
    }

    #[test]
    fn it_doubles_when_the_slots_run_out() {
        let mut slots = SlotBuffer::<[f32; 4]>::new(BUFFER, 2);
        for _ in 0..3 {
            slots.take();
        }
        assert_eq!(slots.records().len(), 4);
        for _ in 0..2 {
            slots.take();
        }
        assert_eq!(slots.records().len(), 8);
    }

    #[test]
    fn it_uploads_only_what_changed() {
        let mut buffers = Assets::<ShaderBuffer>::default();
        let mut slots = SlotBuffer::<[f32; 4]>::new(BUFFER, 2);
        slots.upload(&mut buffers);
        assert_eq!(uploaded(&buffers), vec![[0.0; 4]; 2], "made at its first size");
        let slot = slots.take();
        slots.set(slot, [1.0; 4]);
        assert!(slots.dirty);
        slots.upload(&mut buffers);
        assert_eq!(uploaded(&buffers), vec![[1.0; 4], [0.0; 4]]);
        slots.set(slot, [1.0; 4]);
        assert!(!slots.dirty, "the same record again is no change");
        slots.set(9, [2.0; 4]);
        assert!(!slots.dirty, "a slot past the end is skipped");
        for _ in 0..2 {
            slots.take();
        }
        slots.upload(&mut buffers);
        assert_eq!(uploaded(&buffers).len(), 4, "a grown buffer is uploaded at its new size");
    }
}
