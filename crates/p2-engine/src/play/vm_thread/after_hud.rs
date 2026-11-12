use super::*;

impl Simulation {
    fn pass_switches(vm: &Vm<Arc<Loader>>, switches: &mut Vec<(ObjectId, Option<Option<ObjectId>>)>, new: &[ObjectId], frame: &mut VmFrame) {
        switches.extend(new.iter().filter(|&&object| is_switch(vm, object)).map(|&object| (object, None)));
        for (switch, last) in switches {
            if vm.store.object(*switch).class.is_none() {
                continue;
            }
            let now = switch_material(vm, *switch);
            if *last != Some(now) {
                *last = Some(now);
                frame.material_switches.push((vm.store.object(*switch).path.clone(), now.map(|material| vm.store.object(material).path.clone())));
            }
        }
    }

    fn animate_dynamic_lights(&mut self, frame: &mut VmFrame) {
        let vm = &self.vm;
        let now = level_time(vm);
        let clock = &mut self.light_clock;
        frame.dynamic_lights.extend(
            self.lights
                .iter()
                .filter(|&&actor| vm.store.flag_of(actor, Prop::bDynamicLight))
                .filter_map(|&actor| crate::actors::mesh_light(vm, actor))
                .map(|light| clock.animate(light, now)),
        );
    }

    pub(in crate::play) fn read_after_hud(&mut self, frame: &mut VmFrame, watched: &[ObjectId], forces: bool) {
        let mut started = Instant::now();
        let mut part = |frame: &mut VmFrame, name: &'static str| frame.times.push((name, std::mem::replace(&mut started, Instant::now()).elapsed()));
        self.begin_reads();
        self.read_hud_actors(frame);
        let new: Vec<ObjectId> = self.vm.object_ids().skip(self.seen).collect();
        self.seen += new.len();
        self.read_spawned(frame, &new);
        part(frame, "after hud: hud actors and spawned");
        for index in 0..self.hud_drawn.len() {
            let actor = self.hud_drawn[index];
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        for &actor in watched.iter().chain(&new) {
            self.pass_actor(actor, &mut frame.actor_changes, &mut frame.spawned);
        }
        part(frame, "after hud: watched actors");
        self.update_tracked_lists(frame, &new);
        self.animate_dynamic_lights(frame);
        part(frame, "after hud: lights, switches, coronas");
        self.recent.swap(0, 1);
        self.recent[0] = new;
        self.take_host_commands(frame);
        part(frame, "after hud: host commands");
        let vm = &self.vm;
        frame.emitter_changes = self.emitter_values.changes(vm, watched, self.mark);
        part(frame, "after hud: emitter changes");
        if forces {
            frame.forces.extend(vm.drag_actors().into_iter().map(|(location, velocity, radius, scale)| ActorForce { location, velocity, radius, scale }));
        }
        frame.fov = self.player.and_then(|player| match self.vm.store.get(player, Prop::FovAngle) {
            Some(p2_vm::Value::Float(fov)) if fov > 0.0 => Some(fov),
            _ => None,
        });
        frame.overlay = self.screen_overlay();
        let vm = &self.vm;
        frame.movies.extend(vm.movie_textures().filter_map(|(texture, movie)| Some((vm.store.object(texture).path.clone(), movie, vm.movie(movie)?.clone()))));
        frame.paused = vm.paused();
        frame.quit = self.quit;
        self.read_audio(frame);
        self.read_settings(frame);
        self.read_zone_fogs(frame);
        frame.volume_fog = self.volume_fog();
    }

    fn volume_fog(&self) -> Option<crate::actors::ZoneFog> {
        let vm = &self.vm;
        let target = self.view?.actor?;
        let volume = vm.store.object_of(target, if vm.store.is_a(target, Kind::Pawn) { Prop::HeadVolume } else { Prop::PhysicsVolume })?;
        crate::actors::zone_fog(vm, volume)
    }

    fn read_hud_actors(&mut self, frame: &mut VmFrame) {
        self.hud_drawn.clear();
        self.hud_drawn.extend(frame.hud.iter().filter_map(|item| if let CanvasItem::Actor { actor, .. } = item { Some(*actor) } else { None }));
        self.view_key.hud = std::hash::BuildHasher::hash_one(&rustc_hash::FxBuildHasher, &self.hud_drawn);
        let drawn = &self.hud_drawn;
        self.hud_looks.retain(|actor, _| drawn.contains(actor));
        for index in 0..self.hud_drawn.len() {
            let actor = self.hud_drawn[index];
            let look = actor_look(&self.vm, actor);
            if self.hud_looks.insert(actor, look) != Some(look) {
                let model = self.canvas_model(actor);
                frame.canvas_models.push((actor, model));
            }
            // Posed as drawn (retail poses a mesh when it renders it): one no camera saw last frame (the foot before a
            // kick) was not posed above.
            if let std::collections::hash_map::Entry::Vacant(entry) = frame.poses.entry(actor) {
                entry.insert(self.actor_pose(actor));
                self.posed.insert(actor);
            }
        }
    }

    fn canvas_model(&mut self, actor: ObjectId) -> CanvasModel {
        if let Some(model) = mesh_actor(&self.vm, actor) {
            return CanvasModel::Static(model);
        }
        if let Some(mesh) = self.actor_mesh(actor) {
            return CanvasModel::Skeletal(mesh);
        }
        if self.unmeshed.insert(actor) {
            let vm = &self.vm;
            let path = |property: Prop| match vm.store.get(actor, property) {
                Some(p2_vm::Value::Object(Some(object))) => vm.store.object(object).path.clone(),
                _ => "None".to_owned(),
            };
            bevy::log::warn!(
                "HUD actor {} ({actor:?}) draws neither a static nor a skeletal mesh: DrawType {:?}, Mesh {}, StaticMesh {}",
                vm.store.object(actor).path,
                vm.store.get(actor, Prop::DrawType),
                path(Prop::Mesh),
                path(Prop::StaticMesh)
            );
        }
        CanvasModel::Nothing
    }

    fn read_spawned(&mut self, frame: &mut VmFrame, new: &[ObjectId]) {
        for &actor in new {
            let vm = &self.vm;
            let Some(class) = vm.store.object(actor).class else { continue };
            // Fluid feeders and trail starters are emitters that declare no particle emitters: logic actors with nothing to draw.
            if vm.store.is_a(actor, Kind::Emitter)
                && !vm.store.is_deleted(actor)
                && crate::actors::emitter_objects(vm, actor).is_empty()
                && matches!(vm.store.default_value(class, Prop::Emitters), Some(p2_vm::Value::Array(items)) if !items.is_empty())
            {
                let class = vm.store.name(vm.store.class(class).name).to_owned();
                if self.emitterless.insert(class.clone()) {
                    bevy::log::warn!("spawned {class} ({actor:?}) has no particle emitter to simulate though its class declares some: Emitters {:?}", vm.store.get(actor, Prop::Emitters));
                }
            }
        }
        frame.spawned.extend(
            new.iter()
                .filter(|&&actor| self.vm.store.is_a(actor, Kind::Actor))
                .map(|&actor| Spawned {
                    actor,
                    mesh: mesh_actor(&self.vm, actor),
                    skeletal: skeletal_actor(&self.vm, actor),
                    emitter: emitter_actor(&self.vm, actor),
                    sprite: crate::actors::sprite_actor(&self.vm, actor),
                    corona: crate::map::corona_actor(&self.vm, actor),
                    restyled: false,
                })
                .filter(|s| s.mesh.is_some() || s.skeletal.is_some() || s.emitter.is_some() || s.sprite.is_some() || s.corona.is_some()),
        );
    }

    fn update_tracked_lists(&mut self, frame: &mut VmFrame, new: &[ObjectId]) {
        let vm = &self.vm;
        let destroyed = |actor: ObjectId| vm.store.is_deleted(actor);
        track(
            &mut self.lights,
            new,
            |actor| vm.store.flag_of(actor, Prop::bDynamicLight) || crate::actors::mesh_light(vm, actor).is_some(),
            destroyed,
        );
        track(&mut self.volumes, new, |actor| vm.store.is_a(actor, Kind::Volume), destroyed);
        track(&mut self.corona_lights, new, |actor| crate::map::is_corona(vm, actor), destroyed);
        Self::pass_switches(vm, &mut self.switches, new, frame);
        fluid_touchers(vm, &self.fluid_surfaces, &mut frame.fluid_touchers);
        // Retail's corona line, traced by the VM's level check so static meshes count as in retail.
        // Outside the view or in an unseen zone it fades as if blocked, with no trace.
        if let Some(view) = self.view {
            let (eye, forward) = (view.location, view.rotation.vector());
            let fov = self.player.and_then(|player| match self.vm.store.get(player, Prop::FovAngle) {
                Some(p2_vm::Value::Float(fov)) if fov > 0.0 => Some(fov),
                _ => None,
            });
            let reach = (fov.unwrap_or(90.0).clamp(1.0, 170.0).to_radians() * 0.5).tan() * CORONA_CONE_WIDTH;
            let seen_zones = self.vm.zones_seen_from(eye);
            for &light in &self.corona_lights {
                let at = self.vm.store.location(light);
                let in_view = view.behind || in_view_cone(at - eye, forward, reach);
                let zone_seen = seen_zones & (1u64 << (self.vm.zone_number(at) & 63)) != 0;
                if !in_view || !zone_seen || self.vm.corona_blocked(eye, at) {
                    frame.corona_blocked.push(light);
                }
            }
        }
    }

    fn take_host_commands(&mut self, frame: &mut VmFrame) {
        let commands: Vec<_> = self
            .vm
            .take_commands(|command| {
                matches!(
                    command,
                    HostCommand::Projector(_)
                        | HostCommand::Particle(_)
                        | HostCommand::Window(_)
                        | HostCommand::Quit
                        | HostCommand::SetMouse(_)
                        | HostCommand::SaveGame { .. }
                        | HostCommand::OpenUrl(_)
                )
            })
            .collect();
        frame.plings.append(&mut self.vm.actor_fx.plings);
        frame.projector_actors = self.vm.projector_actors();
        let vm = &self.vm;
        for command in commands {
            match command {
                HostCommand::Projector(event) => frame.projections.push(match event {
                    ProjectorEvent::Attach(actor) => (event, projector_actor(vm, actor)),
                    ProjectorEvent::Detach(_) => (event, None),
                }),
                HostCommand::Particle(command) => frame.particles.push(command.with_properties(|emitter| emitter_properties(vm, emitter))),
                HostCommand::Window(request) => frame.window = Some(request),
                HostCommand::Quit => self.quit = true,
                HostCommand::SetMouse([x, y]) => frame.set_mouse = Some([x as f32, y as f32]),
                HostCommand::SaveGame { slot, save } => crate::saves::write(slot, save),
                HostCommand::OpenUrl(url) => open_url(&url),
                _ => {}
            }
        }
    }

    fn read_zone_fogs(&mut self, frame: &mut VmFrame) {
        let vm = &self.vm;
        let fog_of = |actor: &Option<ObjectId>| actor.and_then(|actor| crate::actors::zone_fog(vm, actor));
        if !self.zone_actors.iter().map(fog_of).eq(self.zone_fogs_sent.iter().copied()) {
            self.zone_fogs_sent = self.zone_actors.iter().map(fog_of).collect();
            frame.zone_fogs = Some(self.zone_fogs_sent.clone());
        }
    }

    fn read_settings(&mut self, frame: &mut VmFrame) {
        let changed = self.vm.take_changed_settings();
        if let Some(settings) = &changed {
            crate::display::save_settings(settings);
        }
        if changed.is_some() || !std::mem::replace(&mut self.settings_sent, true) {
            let vm = &mut self.vm;
            frame.settings = Some(crate::display::Settings::read(|query| vm.get_command(query).unwrap_or_default()));
        }
    }
}

fn fluid_touchers(vm: &Vm<Arc<Loader>>, surfaces: &[ObjectId], touchers: &mut Vec<(ObjectId, Vec<p2_particles::FluidToucher>)>) {
    for &surface in surfaces {
        let Some(p2_vm::Value::Array(touching)) = vm.store.get(surface, Prop::Touching) else { continue };
        let disturbing: Vec<_> = touching
            .iter()
            .filter_map(|value| if let p2_vm::Value::Object(Some(actor)) = value { Some(*actor) } else { None })
            .filter(|&actor| vm.store.flag_of(actor, Prop::bDisturbFluidSurface))
            .map(|actor| p2_particles::FluidToucher {
                location: vm.store.location(actor),
                velocity: vm.velocity(actor),
                radius: match vm.store.get(actor, Prop::CollisionRadius) {
                    Some(p2_vm::Value::Float(radius)) => radius,
                    _ => 0.0,
                },
            })
            .collect();
        if !disturbing.is_empty() {
            touchers.push((surface, disturbing));
        }
    }
}

fn level_time(vm: &Vm<Arc<Loader>>) -> f32 {
    match vm.level().and_then(|level| vm.store.get(level, Prop::TimeSeconds)) {
        Some(p2_vm::Value::Float(time)) => time,
        _ => 0.0,
    }
}

/// How much wider than the tangent of half the field of view the cone tested for coronas is: it holds the whole frustum
/// at any screen shape up to 21:9, and a corona whose centre just left the screen is still partly on it.
const CORONA_CONE_WIDTH: f32 = 2.0;

fn in_view_cone(offset: Vector, forward: Vector, reach: f32) -> bool {
    let along = offset.dot(forward);
    along > 0.0 && (offset.size_squared() - along * along).max(0.0) <= (along * reach).powi(2)
}

fn track(list: &mut Vec<ObjectId>, new: &[ObjectId], joins: impl Fn(ObjectId) -> bool, gone: impl Fn(ObjectId) -> bool) {
    list.extend(new.iter().copied().filter(|&actor| joins(actor)));
    list.retain(|&actor| !gone(actor));
}

/// Explorer hands a web or `steam://` link to its registered program without blocking.
fn open_url(url: &str) {
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    if let Err(error) = std::process::Command::new(opener).arg(url).spawn() {
        bevy::log::warn!("opening {url}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corona_cone_holds_what_is_in_front_within_its_reach() {
        let forward = Vector::from([1.0, 0.0, 0.0]);
        assert!(in_view_cone([100.0, 150.0, 0.0].into(), forward, 2.0));
        assert!(!in_view_cone([100.0, 250.0, 0.0].into(), forward, 2.0));
        assert!(!in_view_cone([-100.0, 0.0, 0.0].into(), forward, 2.0));
    }

    #[test]
    fn tracked_list_adds_joiners_in_order_and_drops_the_gone() {
        let id = ObjectId::from_index;
        let mut list = vec![id(1), id(4)];
        track(&mut list, &[id(6), id(7), id(8)], |actor| [id(6), id(8)].contains(&actor), |actor| actor == id(4));
        assert_eq!(list, [id(1), id(6), id(8)]);
        track(&mut list, &[], |_| true, |_| false);
        assert_eq!(list, [id(1), id(6), id(8)]);
    }
}
