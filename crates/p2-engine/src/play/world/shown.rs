use std::borrow::Cow;

use p2_assets::pose::Frame;

use super::*;

/// The drawn state of the game. The view, every actor's place, the channel times and the ragdoll bones a frame shows
/// all come from here, at the one shown time of its clock: nothing drawn keeps a glide or a clock of its own.
pub(crate) struct Shown {
    pub(crate) clock: TickClock,
    glide: Glide,
    pub(crate) places: Places,
    view_from: Option<[(Vector, Rotator); 2]>,
    /// Per posed actor and channel: the animation (its address) and the sequence at the tick before the newest, the
    /// time shown when the newest tick came and that tick's time.
    pose_from: FxHashMap<ObjectId, Vec<ChannelFrom>>,
    ragdoll_from: FxHashMap<ObjectId, [Vec<Frame>; 2]>,
}

impl Default for Shown {
    fn default() -> Self {
        Self {
            clock: TickClock::default(),
            glide: Glide { to_mid: 1.0, to_newest: 1.0 },
            places: Places::default(),
            view_from: None,
            pose_from: FxHashMap::default(),
            ragdoll_from: FxHashMap::default(),
        }
    }
}

impl Shown {
    /// A frame `real_delta` seconds after the one before: the shown time moves on.
    pub(in crate::play) fn frame(&mut self, real_delta: f32) {
        self.glide = self.clock.frame(real_delta);
    }

    /// The tick `new` came after `old`: what each thing shows now is where its way to `old` starts.
    pub(super) fn tick(&mut self, old: &VmFrame, new: &VmFrame, actors: &FxHashMap<ObjectId, ActorFrame>) {
        let glide = self.glide;
        self.places.tick(glide, actors);
        let shown = old.view.map(|view| (self.view((view.location, view.rotation)), (view.location, view.rotation)));
        self.view_from = shown.zip(new.view).filter(|((_, mid), to)| (to.location - mid.0).size() <= GLIDE_REACH).map(|((from, mid), _)| [from, mid]);
        let before = std::mem::take(&mut self.pose_from);
        self.pose_from = old
            .poses
            .iter()
            .filter_map(|(&actor, pose)| {
                let pose = pose.as_ref()?;
                new.poses.get(&actor)?.as_ref()?;
                let from = before.get(&actor);
                let shown = channels(pose).enumerate().map(|(index, channel)| ChannelFrom::new(from.and_then(|from| from.get(index)), ChannelTime::of(channel), glide));
                Some((actor, shown.collect()))
            })
            .collect();
        let before = std::mem::take(&mut self.ragdoll_from);
        self.ragdoll_from = old
            .ragdolls
            .iter()
            .filter(|(actor, bones)| new.ragdolls.get(*actor).is_some_and(|now| now.len() == bones.len()))
            .map(|(&actor, bones)| {
                let shown = before.get(&actor).map_or_else(|| bones.clone(), |[from, mid]| glide.pick(&from[..], &mid[..], &bones[..], glided_bones));
                (actor, [shown, bones.clone()])
            })
            .collect();
        self.clock.tick(new.step);
        self.glide = self.clock.glide();
    }

    /// The view on its way to `to`, the newest tick's.
    pub(in crate::play) fn view(&self, to: (Vector, Rotator)) -> (Vector, Rotator) {
        self.view_from.map_or(to, |[from, mid]| self.glide.pick(from, mid, to, glided))
    }

    pub(super) fn actor_place(&self, actor: ObjectId, now: &ActorFrame) -> Option<(Vector, Rotator)> {
        self.places.shown(actor, now, self.glide)
    }

    /// The time to show channel `index` of the actor's pose at: on its way from the last tick's while both play the
    /// same sequence forward, across a loop's wrap too.
    pub fn channel_time(&self, actor: ObjectId, index: usize, channel: &ChannelPose) -> f32 {
        glided_time(self.pose_from.get(&actor).and_then(|from| from.get(index)), ChannelTime::of(channel), self.glide)
    }

    pub(super) fn ragdoll<'a>(&'a self, actor: ObjectId, to: &'a [Frame]) -> Cow<'a, [Frame]> {
        let from = self.ragdoll_from.get(&actor).filter(|_| self.glide.to_newest < 1.0);
        from.map_or(to.into(), |[from, mid]| self.glide.pick(&from[..], &mid[..], to, glided_bones).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_and_an_actor_stand_at_the_same_shown_time() {
        let view = |x: f32| p2_vm::PlayerView { actor: None, location: [x, 0.0, 0.0].into(), rotation: [0; 3].into(), behind: false, interpolating: false };
        let (old, new) = (VmFrame { view: Some(view(0.0)), ..VmFrame::default() }, VmFrame { view: Some(view(10.0)), ..VmFrame::default() });
        let mover = ObjectId::from_index(1);
        let (before, now) = (ActorFrame::kept_at([0.0; 3], None), ActorFrame::kept_at([10.0, 0.0, 0.0], None));
        let mut shown = Shown::default();
        shown.tick(&old, &new, &FxHashMap::default());
        shown.places.changed(mover, Some(&before), &now);
        for (glide, at) in [(Glide { to_mid: 1.0, to_newest: 0.25 }, 2.5), (Glide { to_mid: 1.0, to_newest: 0.5 }, 5.0), (Glide { to_mid: 1.0, to_newest: 1.0 }, 10.0)] {
            shown.glide = glide;
            let seen = shown.view((new.view.unwrap().location, [0; 3].into())).0;
            let stands = shown.actor_place(mover, &now).unwrap().0;
            assert_eq!((seen, stands), (Vector::from([at, 0.0, 0.0]), Vector::from([at, 0.0, 0.0])));
        }
    }
}
