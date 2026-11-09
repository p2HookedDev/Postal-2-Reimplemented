use super::*;

impl Simulation {
    pub(in crate::play) fn pass_actor(&mut self, actor: ObjectId, changes: &mut Vec<(ObjectId, ActorFrame)>, restyled: &mut Vec<Spawned>) {
        let now = self.actor_frame(actor);
        match self.sent.get_mut(&actor) {
            Some(sent) if *sent == now => {}
            Some(sent) => {
                if sent.look != now.look {
                    let vm = &self.vm;
                    let (mesh, skeletal) = (mesh_actor(vm, actor), skeletal_actor(vm, actor));
                    restyled.push(Spawned { actor, mesh, skeletal, emitter: None, sprite: crate::actors::sprite_actor(vm, actor), corona: None, restyled: true });
                }
                sent.clone_from(&now);
                changes.push((actor, now));
            }
            None => {
                self.sent.insert(actor, now.clone());
                changes.push((actor, now));
            }
        }
    }

    pub(in crate::play) fn actor_frame(&mut self, actor: ObjectId) -> ActorFrame {
        let vm = &self.vm;
        let (location, rotation) = location_rotation(vm, actor);
        ActorFrame {
            location,
            rotation,
            scale: actor_scale(vm, actor),
            pre_pivot: vm.store.vector(actor, Prop::PrePivot),
            velocity: vm.velocity(actor),
            time_scale: vm.particle_time_scale(actor),
            gone: vm.store.is_deleted(actor),
            hidden: vm.filtered_from_view(actor, self.view.as_ref(), &self.hud_drawn),
            attachment: bone_attachment(vm, actor, &mut self.lower_names),
            emitters_disabled: emitters_disabled(vm, actor),
            look: actor_look(vm, actor),
            glow: vm.store.byte_of(actor, Prop::AmbientGlow),
        }
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
