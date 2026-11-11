use super::*;

impl Simulation {
    /// Before a pass of reads: writes from here on carry a later stamp than `mark`, so a write that comes after an
    /// actor was read in this pass, at any time, makes the next pass read it again.
    pub(in crate::play) fn begin_reads(&mut self) {
        self.mark = self.vm.store.stamp();
        self.vm.store.next_stamp();
        self.time_dilation = self.vm.time_dilation().to_bits();
    }

    pub(in crate::play) fn pass_actor(&mut self, actor: ObjectId, changes: &mut Vec<(ObjectId, ActorFrame)>, restyled: &mut Vec<Spawned>) {
        let store = &self.vm.store;
        let time_dilation = self.time_dilation;
        let undrawn = |sent: &Sent| {
            sent.steady
                && sent.time_dilation == time_dilation
                && store.object(actor).drawn() <= sent.read_at
                && sent.emitters.iter().all(|&emitter| store.object(emitter).drawn() <= sent.read_at)
        };
        if self.sent.get(&actor).is_some_and(undrawn) {
            self.reads[0] += 1;
            return;
        }
        let always = self.sent.get(&actor).is_some_and(|sent| !sent.steady);
        self.reads[if always { 2 } else { 1 }] += 1;
        if self.vm.diagnostics.tick_profile.is_some()
            && let Some(class) = store.object(actor).class
        {
            self.reads_by_class.entry(class).or_default()[usize::from(always)] += 1;
        }
        let (vm, view, hud_drawn, names) = (&self.vm, self.view.as_ref(), &self.hud_drawn, &mut self.lower_names);
        let (owner_only, now) = vm.store.reading_drawn(|| {
            let owner_only = vm.store.flag_of(actor, Prop::bOwnerNoSee) || vm.store.flag_of(actor, Prop::bOnlyOwnerSee);
            (owner_only, actor_frame(vm, actor, view, hud_drawn, names))
        });
        let (read_at, steady) = (self.mark, now.attachment.is_none() && !owner_only);
        let emitters = || self.vm.store.objects_of(actor, Prop::Emitters).flatten();
        match self.sent.get_mut(&actor) {
            Some(sent) => {
                (sent.read_at, sent.steady, sent.time_dilation) = (read_at, steady, time_dilation);
                sent.emitters.clear();
                sent.emitters.extend(emitters());
                if sent.frame == now {
                    return;
                }
                if sent.frame.look != now.look {
                    let vm = &self.vm;
                    let (mesh, skeletal, sprite) = vm.store.reading_drawn(|| (mesh_actor(vm, actor), skeletal_actor(vm, actor), crate::actors::sprite_actor(vm, actor)));
                    restyled.push(Spawned { actor, mesh, skeletal, emitter: None, sprite, corona: None, restyled: true });
                }
                sent.frame.clone_from(&now);
                changes.push((actor, now));
            }
            None => {
                let emitters = emitters().collect();
                self.sent.insert(actor, Sent { frame: now.clone(), read_at, steady, time_dilation, emitters });
                changes.push((actor, now));
            }
        }
    }

    pub(in crate::play) fn draw_hud(&mut self, frame: &mut VmFrame, canvas: [f32; 2], step: f32) {
        self.hud_age += step;
        // With a menu or the mouse shown the HUD's scripts answer input: they run every tick.
        if self.hud_age >= HUD_PERIOD || frame.shows_mouse || self.hud_canvas != canvas || self.hud_kept.is_empty() {
            self.hud_age = 0.0;
            self.hud_canvas = canvas;
            self.hud_kept = self.hud(canvas).into();
            self.hud_view = self.view;
        }
        frame.hud = Arc::clone(&self.hud_kept);
        frame.hud_view = self.hud_view;
    }

    pub(in crate::play) fn hud(&mut self, [width, height]: [f32; 2]) -> Vec<CanvasItem> {
        let Some(controller) = self.player else { return Vec::new() };
        self.vm.render_hud(controller, width, height).unwrap_or_else(|error| {
            bevy::log::warn!("HUD: {error}");
            Vec::new()
        })
    }

    pub(in crate::play) fn actor_mesh(&self, actor: ObjectId) -> Option<CanvasMesh> {
        let vm = &self.vm;
        let path = object_path(vm, vm.store.get(actor, Prop::Mesh).as_ref())?;
        let skins = skin_paths(vm, actor);
        let scale = actor_scale(vm, actor);
        let follow = FollowActor { actor, scale, pre_pivot: vm.store.vector(actor, Prop::PrePivot), skeletal: None };
        Some(CanvasMesh { path, skins, follow })
    }

    pub(in crate::play) fn screen_overlay(&self) -> Option<Overlay> {
        let overlay = self.vm.screen_overlay(self.vm.player()?);
        let titles = overlay
            .titles
            .iter()
            .filter_map(|t| Some(OverlayTitle { material: self.vm.store.object(t.material?).path.clone(), color: t.color, size: t.size, at: [t.x, t.y], alpha: t.alpha }))
            .collect();
        Some(Overlay { flash_scale: overlay.flash_scale, flash_fog: overlay.flash_fog, titles })
    }

    pub(in crate::play) fn actor_pose(&mut self, actor: ObjectId) -> Option<ActorPose> {
        let base = self.pose_of(actor, 0)?;
        let mut blends = Vec::new();
        for index in 1.. {
            let Some((channel, _)) = self.vm.anim_channel(actor, index) else { break };
            let (alpha, bone) = (p2_assets::pose::channel_weight(channel.blend_alpha, channel.frame, channel.blend_in_time), channel.blend_bone);
            if alpha > 0.0
                && let Some(pose) = self.pose_of(actor, index)
            {
                blends.push((pose, alpha, self.lower_name(bone)));
            }
        }
        #[expect(clippy::needless_collect, reason = "`lower_name` needs `self` mutably while the list borrows the VM")]
        let scales: Vec<_> = self.vm.bone_scales(actor).collect();
        let scales = scales.into_iter().map(|(bone, scale)| (self.lower_name(bone), scale)).collect();
        let turns = self.vm.bone_rotations(actor).to_vec();
        let turns = turns.into_iter().map(|(bone, turn)| (self.lower_name(bone), turn.into())).collect();
        Some(ActorPose { base, blends, scales, turns })
    }

    pub(in crate::play) fn lower_name(&mut self, name: p2_vm::NameId) -> Arc<str> {
        let vm = &self.vm;
        let made = self.lower_names.entry(name).or_insert_with(|| {
            if name == p2_vm::NameId::NONE { Arc::from("") } else { Arc::from(vm.store.name(name).to_ascii_lowercase()) }
        });
        Arc::clone(made)
    }

    pub(in crate::play) fn pose_of(&mut self, actor: ObjectId, index: usize) -> Option<ChannelPose> {
        let (channel, animation) = self.vm.anim_channel(actor, index)?;
        let (sequence, frame) = (channel.sequence, channel.frame.max(0.0));
        let tween = channel.tween().map(|(from, weight)| (Arc::clone(from), weight));
        let object = animation?;
        let vm = &mut self.vm;
        let animation = self
            .animations
            .entry(object)
            .or_insert_with(|| {
                let path = vm.store.object(object).path.clone();
                PlayedAnimation::load(vm, object, &path)
            })
            .clone()?;
        let index = *self.sequences.entry((object, sequence)).or_insert_with(|| animation.sequences.get(&self.vm.store.name(sequence).to_ascii_lowercase()).copied());
        let index = index?;
        let frames = animation.animation.sequences[index].num_frames.max(1) as f32;
        Some(ChannelPose { animation, sequence: index, time: frame * frames, tween })
    }
}

fn actor_frame(vm: &Vm<Arc<Loader>>, actor: ObjectId, view: Option<&p2_vm::PlayerView>, hud_drawn: &[ObjectId], names: &mut FxHashMap<p2_vm::NameId, Arc<str>>) -> ActorFrame {
    let (location, rotation) = location_rotation(vm, actor);
    ActorFrame {
        location,
        rotation,
        scale: actor_scale(vm, actor),
        pre_pivot: vm.store.vector(actor, Prop::PrePivot),
        velocity: vm.velocity(actor),
        time_scale: vm.particle_time_scale(actor),
        gone: vm.store.is_deleted(actor),
        hidden: vm.filtered_from_view(actor, view, hud_drawn),
        attachment: bone_attachment(vm, actor, names),
        emitters_disabled: emitters_disabled(vm, actor),
        look: actor_look(vm, actor),
        glow: vm.store.byte_of(actor, Prop::AmbientGlow),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_reads_of_an_actor_are_all_on_the_drawn_list() {
        let loader = Loader::new(&std::env::temp_dir(), &[]).unwrap();
        let mut vm = Vm::new(Arc::new(loader));
        let actor = vm.stub("Fixture.Thing0", "Actor");
        let mut names = FxHashMap::default();
        let frame = vm.store.reading_drawn(|| actor_frame(&vm, actor, None, &[], &mut names));
        assert!(frame.attachment.is_none() && !frame.gone);
        let restyle = vm.store.reading_drawn(|| (mesh_actor(&vm, actor), skeletal_actor(&vm, actor), crate::actors::sprite_actor(&vm, actor)));
        assert!(restyle.0.is_none() && restyle.1.is_none() && restyle.2.is_none());
    }
}
