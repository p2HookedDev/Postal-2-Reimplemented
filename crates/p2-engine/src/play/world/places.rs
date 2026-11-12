use super::*;

/// Where each actor is drawn, and the only way to a drawn place. One hung on a bone has the place its bone gave it and
/// none before that: its own stored location is where the game spawned it, which does not follow the bone.
#[derive(Default)]
pub(crate) struct Places {
    /// Per actor still on its way: the place it showed at when the newest tick came and its place at the tick before.
    /// It is drawn through both to the newest tick's place, so the world moves at the frame rate while ticks are
    /// slower than frames.
    gliding: FxHashMap<ObjectId, [(Vector, Rotator); 2]>,
    /// Kept until the actor leaves its base: handing a place to the game does not take it from here.
    on_bones: FxHashMap<ObjectId, (ObjectId, BoneFrame)>,
    untold: FxHashSet<ObjectId>,
}

impl Places {
    /// A new tick came: what was shown becomes where each glide starts.
    pub(super) fn tick(&mut self, glide: Glide, actors: &FxHashMap<ObjectId, ActorFrame>) {
        self.gliding.retain(|actor, [from, mid]| {
            let on_its_way = actors.get(actor).filter(|_| glide.to_newest < 1.0);
            on_its_way.is_some_and(|to| {
                let to = (to.location, to.rotation);
                *from = glide.pick(*from, *mid, to, glided);
                *mid = to;
                *from != to
            })
        });
    }

    pub(super) fn changed(&mut self, actor: ObjectId, before: Option<&ActorFrame>, now: &ActorFrame) {
        if let Some(before) = before {
            if now.gone || (now.location - before.location).size() > GLIDE_REACH {
                self.gliding.remove(&actor);
            } else if (before.location, before.rotation) != (now.location, now.rotation) {
                self.gliding.entry(actor).or_insert([(before.location, before.rotation); 2]);
            }
        }
        let base = now.attachment.as_ref().filter(|_| !now.gone).map(|attachment| attachment.base);
        if self.on_bones.get(&actor).is_some_and(|&(placed_on, _)| Some(placed_on) != base) {
            self.on_bones.remove(&actor);
            self.untold.remove(&actor);
        }
    }

    pub(crate) fn glides(&self, actor: ObjectId) -> bool {
        self.gliding.contains_key(&actor)
    }

    /// Where the actor is drawn this frame; `None` for one on a bone that has no place yet.
    pub(in crate::play) fn shown(&self, actor: ObjectId, now: &ActorFrame, glide: Glide) -> Option<(Vector, Rotator)> {
        if now.attachment.is_some() {
            return self.on_bone(actor);
        }
        let to = (now.location, now.rotation);
        Some(self.gliding.get(&actor).map_or(to, |&[from, mid]| glide.pick(from, mid, to, glided)))
    }

    /// Its place at the newest tick, for what the game's scripts draw themselves at that tick's view.
    pub(in crate::play) fn newest(&self, actor: ObjectId, now: &ActorFrame) -> Option<(Vector, Rotator)> {
        if now.attachment.is_some() { self.on_bone(actor) } else { Some((now.location, now.rotation)) }
    }

    fn on_bone(&self, actor: ObjectId) -> Option<(Vector, Rotator)> {
        let (_, BoneFrame { location, axes: [x, y, z] }) = *self.on_bones.get(&actor)?;
        Some((location, p2_vm::orthogonal_rotation(x, y, z)))
    }

    pub(crate) fn bone_frame(&self, actor: ObjectId) -> Option<BoneFrame> {
        self.on_bones.get(&actor).map(|&(_, frame)| frame)
    }

    pub(crate) fn put_on_bone(&mut self, actor: ObjectId, base: ObjectId, frame: BoneFrame) {
        if self.on_bones.insert(actor, (base, frame)) != Some((base, frame)) {
            self.untold.insert(actor);
        }
    }

    /// The bone places made since the last call, of bases a camera drew, for the game to learn.
    pub(in crate::play) fn tell(&mut self, visible: &FxHashSet<ObjectId>, told: &mut Vec<(ObjectId, BoneFrame)>) {
        let on_bones = &self.on_bones;
        told.extend(self.untold.drain().filter_map(|actor| on_bones.get(&actor).filter(|(base, _)| visible.contains(base)).map(|&(_, frame)| (actor, frame))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEWEST: Glide = Glide { to_mid: 1.0, to_newest: 1.0 };

    fn id(index: u32) -> ObjectId {
        ObjectId::from_index(index)
    }

    fn bone_frame(location: [f32; 3]) -> BoneFrame {
        BoneFrame { location: location.into(), axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into(), [0.0, 0.0, 1.0].into()] }
    }

    #[test]
    fn a_thing_on_a_bone_is_never_drawn_where_the_game_keeps_it() {
        let (flash, weapon) = (id(1), id(2));
        let spawned_at = [5.0, 0.0, 0.0];
        let now = ActorFrame::kept_at(spawned_at, Some(weapon));
        let mut places = Places::default();
        places.changed(flash, None, &now);
        assert_eq!(places.shown(flash, &now, NEWEST), None);
        assert_eq!(places.newest(flash, &now), None);
        places.put_on_bone(flash, weapon, bone_frame([5.0, 0.0, 98.0]));
        let muzzle = Vector::from([5.0, 0.0, 98.0]);
        assert_eq!(places.shown(flash, &now, NEWEST).map(|(location, _)| location), Some(muzzle));
        assert_eq!(places.newest(flash, &now).map(|(location, _)| location), Some(muzzle));
    }

    #[test]
    fn a_bone_place_stands_after_the_game_was_told_it() {
        let (flash, weapon) = (id(1), id(2));
        let now = ActorFrame::kept_at([5.0, 0.0, 0.0], Some(weapon));
        let mut places = Places::default();
        places.put_on_bone(flash, weapon, bone_frame([5.0, 0.0, 98.0]));
        let (seen, mut told) = (FxHashSet::from_iter([weapon]), Vec::new());
        places.tell(&seen, &mut told);
        assert_eq!(told.len(), 1);
        told.clear();
        places.put_on_bone(flash, weapon, bone_frame([5.0, 0.0, 98.0]));
        places.tell(&seen, &mut told);
        assert!(told.is_empty());
        for _ in 0..3 {
            places.tick(NEWEST, &FxHashMap::default());
            assert!(places.shown(flash, &now, NEWEST).is_some());
        }
    }

    #[test]
    fn the_game_is_told_only_of_bases_a_camera_drew() {
        let mut places = Places::default();
        places.put_on_bone(id(1), id(2), bone_frame([0.0; 3]));
        places.put_on_bone(id(3), id(4), bone_frame([0.0; 3]));
        let mut told = Vec::new();
        places.tell(&FxHashSet::from_iter([id(4)]), &mut told);
        assert_eq!(told.iter().map(|&(actor, _)| actor).collect::<Vec<_>>(), [id(3)]);
    }

    #[test]
    fn off_its_bone_gone_or_on_another_base_the_bone_place_is_dropped() {
        let (hat, head, other) = (id(1), id(2), id(3));
        let worn = ActorFrame::kept_at([0.0; 3], Some(head));
        let mut places = Places::default();
        places.put_on_bone(hat, head, bone_frame([0.0, 0.0, 70.0]));
        let moved = ActorFrame::kept_at([1.0, 0.0, 0.0], Some(head));
        places.changed(hat, Some(&worn), &moved);
        assert!(places.shown(hat, &moved, NEWEST).is_some());
        let elsewhere = ActorFrame::kept_at([1.0, 0.0, 0.0], Some(other));
        places.changed(hat, Some(&moved), &elsewhere);
        assert_eq!(places.shown(hat, &elsewhere, NEWEST), None);
        places.put_on_bone(hat, other, bone_frame([0.0, 0.0, 70.0]));
        let fallen = ActorFrame::kept_at([9.0, 0.0, 0.0], None);
        places.changed(hat, Some(&elsewhere), &fallen);
        assert_eq!(places.shown(hat, &fallen, NEWEST).map(|(location, _)| location), Some(Vector::from([9.0, 0.0, 0.0])));
        assert_eq!(places.bone_frame(hat).map(|frame| frame.location), None);
        places.put_on_bone(hat, head, bone_frame([0.0; 3]));
        let destroyed = ActorFrame { gone: true, ..ActorFrame::kept_at([0.0; 3], Some(head)) };
        places.changed(hat, Some(&worn), &destroyed);
        assert_eq!(places.bone_frame(hat).map(|frame| frame.location), None);
    }

    #[test]
    fn an_actor_on_no_bone_is_drawn_on_its_way_between_the_ticks() {
        let mover = id(1);
        let (before, now) = (ActorFrame::kept_at([0.0; 3], None), ActorFrame::kept_at([10.0, 0.0, 0.0], None));
        let mut places = Places::default();
        places.changed(mover, Some(&before), &now);
        assert!(places.glides(mover));
        let halfway = Glide { to_mid: 1.0, to_newest: 0.5 };
        assert_eq!(places.shown(mover, &now, halfway).map(|(location, _)| location), Some(Vector::from([5.0, 0.0, 0.0])));
        assert_eq!(places.newest(mover, &now).map(|(location, _)| location), Some(Vector::from([10.0, 0.0, 0.0])));
    }
}
