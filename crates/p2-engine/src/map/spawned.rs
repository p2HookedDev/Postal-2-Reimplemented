use super::*;
use crate::map::particles::attach_late_pools;
use crate::mesh_view::ActorMaterialKey;
use super::scene::{PendingAssets, VmAnimations};
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[expect(clippy::large_enum_variant, reason = "one per mesh in the spawn cache, never moved in bulk")]
pub(super) enum SpawnModel {
    Static { mesh: Arc<StaticMesh>, sections: Vec<SectionView> },
    Skeletal { mesh: Arc<SkeletalMesh>, sections: Vec<SectionView>, animated: Option<AnimatedMesh> },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ModelKey {
    Static { path: String, skins: Vec<Option<String>> },
    Skeletal { path: String, skins: Vec<Option<String>> },
}

pub(super) fn static_model_key(path: &str, skins: &[Option<String>]) -> ModelKey {
    ModelKey::Static { path: path.to_owned(), skins: skins.to_vec() }
}

pub(super) struct TextureBuild {
    path: String,
    image: Handle<Image>,
    task: Task<std::thread::Result<(Game, PendingAssets, Option<ActorTexture>)>>,
}

impl TextureBuild {
    fn start(game: &Game, t: &SceneTargets, path: &str) -> Self {
        let image = t.assets.images.get_handle_provider().reserve_handle().typed::<Image>();
        let (mut built, mut assets, reserved, read_path) = (game.clone(), PendingAssets::new(t), image.clone(), path.to_owned());
        let task = AsyncComputeTaskPool::get().spawn(async move {
            catch_unwind(AssertUnwindSafe(move || {
                let texture = ActorTexture::read(&mut built, &read_path, |read| assets.add_image_as(reserved, read));
                (built, assets, texture)
            }))
        });
        Self { path: path.to_owned(), image, task }
    }
}

pub(super) fn actor_image(cache: &FxHashMap<String, Option<ActorTexture>>, builds: &mut Vec<TextureBuild>, game: &Game, t: &SceneTargets, path: &str) -> Option<Handle<Image>> {
    if let Some(found) = cache.get(path) {
        return found.as_ref().map(|texture| texture.image.clone());
    }
    if let Some(build) = builds.iter().find(|build| build.path == path) {
        return Some(build.image.clone());
    }
    let build = TextureBuild::start(game, t, path);
    let image = build.image.clone();
    builds.push(build);
    Some(image)
}

pub(super) fn static_model(game: &mut Game, t: &mut SceneTargets, path: &str, skins: &[Option<String>]) -> Option<SpawnModel> {
    let model = build_static_model(game, &mut t.assets, path, skins);
    keep_static_meshes(t, model.as_ref());
    model
}

pub(super) fn build_static_model<S: SceneAssets>(game: &mut Game, t: &mut S, path: &str, skins: &[Option<String>]) -> Option<SpawnModel> {
    let (mesh_id, mesh_export) = game.export_by_path_of_class(path, "StaticMesh")?;
    let (mesh, materials) = {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        (read_static_mesh(pkg, file, export).ok()?, read_static_mesh_materials(pkg, file, export).unwrap_or_default())
    };
    let refs: Vec<(PackageId, i32)> = (0..materials.len().max(skins.len()))
        .map(|i| match skins.get(i).cloned().flatten().and_then(|path| game.export_by_path(&path)) {
            Some((package, export)) => (package, export_reference(export)),
            None => (mesh_id, materials.get(i).copied().unwrap_or(0)),
        })
        .collect();
    let sections = sections_of_static_mesh(&mesh, t, SectionChunks::Whole, |t, i| {
        let &(package, material) = refs.get(i)?;
        material_images(game, t, package, material)
    });
    Some(SpawnModel::Static { mesh: Arc::new(mesh), sections })
}

pub(super) fn keep_static_meshes(t: &mut SceneTargets, model: Option<&SpawnModel>) {
    let Some(SpawnModel::Static { sections, .. }) = model else { return };
    for section in sections {
        if let Some(mut mesh) = t.assets.meshes.get_mut(&section.mesh) {
            keep_for_relighting(&mut mesh);
        }
    }
}

fn build_skeletal_model(
    game: &mut Game,
    t: &mut PendingAssets,
    vm_animations: &VmAnimations,
    animations: &mut HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    path: &str,
    skins: &[Option<String>],
) -> Option<SpawnModel> {
    let (mesh, mesh_id, sections, handles) = skeletal_sections(game, t, &mut material_images::<PendingAssets>, path, skins)?;
    let mesh = Arc::new(mesh);
    let animated = {
        let (pkg, data) = game.data(mesh_id);
        let animations = animations.entry(mesh_id).or_insert_with(|| vm_animations.package(game.package_name(mesh_id), pkg, data));
        animate_mesh_with(pkg, animations, handles, &mesh, &["idle"]).map(|(item, _)| item)
    };
    Some(SpawnModel::Skeletal { mesh, sections, animated })
}

const NO_PROJECTORS_TAG: u32 = 1;
const ATTACHED_TAG: u32 = 2;

fn projector_mesh_tag(actor: Option<ObjectId>, attached: &FxHashSet<ObjectId>) -> u32 {
    if actor.is_some_and(|actor| attached.contains(&actor)) { ATTACHED_TAG } else { NO_PROJECTORS_TAG }
}

pub(super) struct SpawnedActor {
    key: ModelKey,
    actor: ObjectId,
    draw: SpawnedDraw,
}

enum SpawnedDraw {
    Static(MeshActor),
    Skeletal(SkeletalActor),
}

impl SpawnedActor {
    fn new(actor: ObjectId, mesh: Option<MeshActor>, skeletal: Option<SkeletalActor>) -> Option<Self> {
        let (key, draw) = match (mesh, skeletal) {
            (Some(a), _) => (static_model_key(&a.static_mesh, &a.skins), SpawnedDraw::Static(a)),
            // GPU-skinned meshes stay in their bind pose: actors of the same mesh and skins share them.
            (None, Some(a)) => (ModelKey::Skeletal { path: a.mesh.clone(), skins: a.skins.clone() }, SpawnedDraw::Skeletal(a)),
            (None, None) => return None,
        };
        Some(Self { key, actor, draw })
    }
}

pub(super) struct ModelBuild {
    pub(super) key: ModelKey,
    task: Task<std::thread::Result<BuiltModel>>,
}

struct BuiltModel {
    game: Game,
    assets: PendingAssets,
    animations: HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    model: Option<SpawnModel>,
}

impl ModelBuild {
    pub(super) fn start_static(game: &Game, t: &SceneTargets, key: ModelKey, path: String) -> Self {
        let (mut built, mut assets) = (game.clone(), PendingAssets::new(t));
        let task = AsyncComputeTaskPool::get().spawn(async move {
            catch_unwind(AssertUnwindSafe(move || {
                let model = build_static_model(&mut built, &mut assets, &path, &[]);
                BuiltModel { game: built, assets, animations: HashMap::new(), model }
            }))
        });
        Self { key, task }
    }

    fn start(
        game: &Game,
        t: &SceneTargets,
        vm_animations: &VmAnimations,
        animations: &HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
        spawned: &SpawnedActor,
    ) -> Self {
        let (skeletal, path, skins) = match &spawned.draw {
            SpawnedDraw::Static(a) => (false, a.static_mesh.clone(), a.skins.clone()),
            SpawnedDraw::Skeletal(a) => (true, a.mesh.clone(), a.skins.clone()),
        };
        let (mut built, mut assets, vm_animations, mut animations) = (game.clone(), PendingAssets::new(t), vm_animations.clone(), animations.clone());
        let task = AsyncComputeTaskPool::get().spawn(async move {
            catch_unwind(AssertUnwindSafe(move || {
                let model = if skeletal {
                    build_skeletal_model(&mut built, &mut assets, &vm_animations, &mut animations, &path, &skins)
                } else {
                    build_static_model(&mut built, &mut assets, &path, &skins)
                };
                BuiltModel { game: built, assets, animations, model }
            }))
        });
        Self { key: spawned.key.clone(), task }
    }
}

fn static_actor_sections(t: &mut SceneTargets, mesh: &Arc<StaticMesh>, sections: &[SectionView], a: &MeshActor) -> (Vec<SectionView>, Option<ActorLit>) {
    let mut sections = sections.to_vec();
    let lit = (!a.lighting.unlit).then(|| {
        for section in &mut sections {
            if let Some(copy) = t.assets.meshes.get(&section.mesh).cloned() {
                section.mesh = t.assets.meshes.add(copy);
            }
        }
        let shading = Shading::Vertices { mesh: Arc::clone(mesh), scale: a.scale, pre_pivot: a.pre_pivot };
        // Not its `MaxLights`: retail sets every listed light and the driver takes the first 8.
        ActorLit::new(a.object, a.lighting.glow, MAX_HARDWARE_LIGHTS, 0.0, shading)
    });
    (sections, lit)
}

fn spawn_actor_model(model: &SpawnModel, spawned: SpawnedActor, origin: Vec3, shared: &mut FxHashMap<ActorMaterialKey, SectionMaterial>, t: &mut SceneTargets) {
    let SpawnedActor { actor, draw, .. } = spawned;
    let (sections, follow, lit) = match (model, draw) {
        (SpawnModel::Static { mesh, sections }, SpawnedDraw::Static(a)) => {
            let (sections, lit) = static_actor_sections(t, mesh, sections, &a);
            (sections, FollowActor { actor, scale: a.scale, pre_pivot: a.pre_pivot, skeletal: None }, lit)
        }
        (SpawnModel::Skeletal { mesh, sections, animated }, SpawnedDraw::Skeletal(a)) => {
            if let Some(item) = animated {
                let mut item = item.clone();
                item.actor = Some(actor);
                t.animated.0.push(item);
            }
            let lit = ActorLit::skeletal(actor, mesh, a.scale, &a.lighting);
            (sections.clone(), FollowActor { actor, scale: a.scale, pre_pivot: a.pre_pivot, skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)) }, lit)
        }
        _ => return,
    };
    let frame = t.play.world().and_then(|world| world.actor(actor)).map(|f| (f.gone || f.hidden, f.location, f.rotation));
    let (placed, shown) = match frame {
        Some((hidden, location, rotation)) => (follow.transform(location, rotation), if hidden { Visibility::Hidden } else { Visibility::Inherited }),
        None => (Transform::default(), Visibility::Inherited),
    };
    let parent = t.commands.spawn((ActorModel(actor), Transform::from_translation(origin), Visibility::default())).id();
    let (materials, mesh_lit) = (&mut t.assets.materials, &mut t.assets.mesh_lit);
    let material = |section: &SectionView| spawned_material(shared, materials, mesh_lit, section);
    spawn_actor_sections(&mut t.commands, parent, sections, placed, (actor, follow), material, lit, shown);
}

fn spawned_material(
    shared: &mut FxHashMap<ActorMaterialKey, SectionMaterial>,
    materials: &mut Assets<StandardMaterial>,
    mesh_lit: &mut Assets<MeshLitMaterial>,
    section: &SectionView,
) -> SectionMaterial {
    crate::mesh_view::cached_actor_material(shared, section, materials, mesh_lit, || crate::mesh_view::actor_standard(section, crate::mesh_view::SceneLights::Ignored))
}

pub fn material_images<S: SceneAssets>(game: &mut Game, t: &mut S, package: PackageId, material: i32) -> Option<MaterialImages> {
    let texture = game.material_texture(package, material)?;
    Some(MaterialImages { image: t.add_image(texture.image), glow: texture.glow.map(|glow| t.add_image(glow)), detail: texture.detail.map(|detail| t.add_image(detail)), specular: texture.specular.map(|(cube, params)| (t.add_image(cube), params)), state: texture.state })
}

impl Session {
    pub(super) fn update_spawned(&mut self, t: &mut SceneTargets) {
        let Some(world) = t.play.world_mut() else { return };
        let mut spawned = world.take_spawned();
        // Restyled actors: retail draws whatever the actor holds each frame. The old model goes, scene-built or spawned; the new
        // one is built below like a spawned actor's (its pose and ragdoll come from the actor's frames, so they carry over). A
        // HUD-drawn model is made again from its current mesh when next drawn.
        let restyled: FxHashSet<ObjectId> = spawned.iter().filter(|s| s.restyled).map(|s| s.actor).collect();
        if !restyled.is_empty() {
            let roots: bevy::ecs::entity::EntityHashSet = t.models.actor_models.iter().filter(|(_, model, _)| restyled.contains(&model.0)).map(|(entity, _, _)| entity).collect();
            for &entity in &roots {
                t.commands.entity(entity).despawn();
            }
            for (entity, (follow, bone), parent) in &t.models.followers {
                let under_root = parent.is_some_and(|p| roots.contains(&p.parent()));
                if !under_root && follow.map(|f| f.actor).or(bone.map(|b| b.actor)).is_some_and(|actor| restyled.contains(&actor)) {
                    t.commands.entity(entity).despawn();
                }
            }
            self.canvas_actors.retain(|shown| {
                let keep = !restyled.contains(&shown.actor);
                if !keep {
                    t.commands.entity(shown.entity).despawn();
                }
                keep
            });
            t.animated.0.retain(|item| item.actor.is_none_or(|actor| !restyled.contains(&actor)));
            self.spawn_waiting.retain(|waiting| !restyled.contains(&waiting.actor));
        }
        if spawned.is_empty() && self.spawn_builds.is_empty() && self.texture_builds.is_empty() {
            return;
        }
        let origin = world.origin();
        let emitters = spawned.iter_mut().filter_map(|s| s.emitter.take().map(|emitter| (s.actor, emitter))).collect();
        self.add_particles(emitters, origin, t);
        self.waiting_sprites.extend(spawned.iter_mut().filter_map(|s| s.sprite.take()));
        self.waiting_coronas.extend(spawned.iter_mut().filter_map(|s| s.corona.take()));
        let found: Vec<SpawnedActor> = spawned.into_iter().filter_map(|s| SpawnedActor::new(s.actor, s.mesh, s.skeletal)).collect();
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        let done: Vec<TextureBuild> = self.texture_builds.extract_if(.., |build| build.task.is_finished()).collect();
        for TextureBuild { path, task, .. } in done {
            let texture = match block_on(task) {
                Ok((built, assets, texture)) => {
                    assets.insert(t);
                    game.adopt(&built);
                    texture
                }
                Err(_) => {
                    bevy::log::warn!("actor texture {path}: its read failed");
                    None
                }
            };
            self.actor_textures.insert(path, texture);
        }
        for texture in self.waiting_sprites.iter().map(|sprite| &sprite.texture).chain(self.waiting_coronas.iter().map(|corona| &corona.texture)) {
            actor_image(&self.actor_textures, &mut self.texture_builds, game, t, texture);
        }
        let sprites: Vec<_> = self.waiting_sprites.extract_if(.., |sprite| self.actor_textures.contains_key(&sprite.texture)).collect();
        let coronas: Vec<_> = self.waiting_coronas.extract_if(.., |corona| self.actor_textures.contains_key(&corona.texture)).collect();
        let done: Vec<ModelBuild> = self.spawn_builds.extract_if(.., |build| build.task.is_finished()).collect();
        for ModelBuild { key, task } in done {
            let model = match block_on(task) {
                Ok(built) => {
                    built.assets.insert(t);
                    game.adopt(&built.game);
                    for (id, animations) in built.animations {
                        if let Some(id) = game.same_package(&built.game, id) {
                            self.animations.entry(id).or_insert(animations);
                        }
                    }
                    built.model
                }
                Err(_) => {
                    bevy::log::warn!("spawned model {key:?}: its build failed");
                    None
                }
            };
            keep_static_meshes(t, model.as_ref());
            attach_late_pools(&mut self.particles, model.as_ref(), &key, &mut self.pool_materials, t);
            self.spawn_models.insert(key, model);
        }
        // A severed limb wears its body's skin (`Limb.SetupLimb` sets `Skins[0]`). An actor whose model is not built yet waits
        // for it (shown a frame or two after it spawned).
        let mut waiting = std::mem::take(&mut self.spawn_waiting);
        waiting.extend(found);
        for spawned in waiting {
            if t.play.world().is_some_and(|world| world.is_gone(spawned.actor)) {
                continue;
            }
            match self.spawn_models.get(&spawned.key) {
                Some(Some(model)) => {
                    spawn_actor_model(model, spawned, origin, &mut self.spawn_materials, t);
                    continue;
                }
                Some(None) => continue,
                None => {}
            }
            if !self.spawn_builds.iter().any(|build| build.key == spawned.key) {
                self.spawn_builds.push(ModelBuild::start(game, t, &self.vm_animations, &self.animations, &spawned));
            }
            self.spawn_waiting.push(spawned);
        }
        self.add_sprites(sprites, origin, t);
        self.add_coronas(coronas, origin, t);
    }

    /// Splats abandon their projection for the time they live (`AbandonProjector(UseLife)`, which ends the projection in the
    /// engine) and destroy themselves then. Sections following an actor are tagged by whether it is in a projector's list.
    pub(super) fn update_projections(&mut self, t: &mut SceneTargets) {
        let Some(world) = t.play.world_mut() else { return };
        self.dynamic_decals.retain(|&actor, &mut entity| {
            let gone = world.is_gone(actor);
            if gone {
                world.unwatch(actor);
                t.commands.entity(entity).despawn();
            }
            !gone
        });
        if let Some(actors) = world.take_projector_actors() {
            let changed: FxHashSet<ObjectId> = actors.symmetric_difference(&self.projector_actors).copied().collect();
            self.projector_actors = actors;
            for (entity, (follow, bone), _) in &t.models.followers {
                let actor = follow.map(|f| f.actor).or(bone.map(|b| b.actor));
                if actor.is_some_and(|actor| changed.contains(&actor)) {
                    t.commands.entity(entity).try_insert(MeshTag::new(projector_mesh_tag(actor, &self.projector_actors)));
                }
            }
        }
        for (entity, (follow, bone)) in &t.models.untagged {
            let actor = follow.map(|f| f.actor).or(bone.map(|b| b.actor));
            t.commands.entity(entity).try_insert(MeshTag::new(projector_mesh_tag(actor, &self.projector_actors)));
        }
        let events = world.take_projections();
        if events.is_empty() {
            return;
        }
        let origin = world.origin();
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for (event, projector) in events {
            match event {
                ProjectorEvent::Attach(actor) => {
                    let Some(projector) = projector else { continue };
                    let found = match self.decal_images.get(&projector.texture) {
                        Some(found) => found.clone(),
                        None => {
                            let found = projector_image(game, &mut t.assets, &projector.texture);
                            self.decal_images.insert(projector.texture.clone(), found.clone());
                            found
                        }
                    };
                    let Some((image, size, final_blend)) = found else { continue };
                    let tag = projector_tag(&projector, final_blend);
                    bevy::log::debug!(
                        "projector {actor:?} attached: {}, targets {:#x}, blending {:?}/{:?}, tag {tag:x}",
                        projector.texture,
                        projector.targets.0,
                        projector.blending,
                        projector.material_blending
                    );
                    let decal = ClusteredDecal { base_color_texture: Some(image), tag, ..Default::default() };
                    let transform = Transform::from_translation(origin) * projector_transform(&projector, size);
                    let entity = t.commands.spawn((decal, transform)).id();
                    if let Some(old) = self.dynamic_decals.insert(actor, entity) {
                        t.commands.entity(old).despawn();
                    }
                    if let Some(world) = t.play.world_mut() {
                        world.watch(actor);
                    }
                }
                ProjectorEvent::Detach(actor) => {
                    if let Some(entity) = self.dynamic_decals.remove(&actor) {
                        t.commands.entity(entity).despawn();
                    }
                    if let Some(world) = t.play.world_mut() {
                        world.unwatch(actor);
                    }
                }
            }
        }
    }

    /// The models are placed where the HUD's scripts put their actors at the call (`RenderOverlays` moves the weapon to the
    /// view, `MapScreen` moves its trail marker from point to point), not where they were before the HUD.
    pub(super) fn show_canvas_actors(&mut self, drawn: &[(&CanvasItem, usize)], t: &mut SceneTargets) {
        let Some(world) = t.play.world() else { return };
        self.canvas_actors.retain(|shown| {
            let keep = !world.is_gone(shown.actor);
            if !keep {
                t.animated.0.retain(|item| item.actor != Some(shown.actor));
                t.commands.entity(shown.entity).despawn();
            }
            keep
        });
        let calls: Vec<(ObjectId, usize, usize, &CanvasItem)> = drawn
            .iter()
            .enumerate()
            .filter_map(|(at, &(item, slot))| match *item {
                CanvasItem::Actor { actor, .. } => {
                    let before = drawn[..at].iter().filter(|(other, _)| matches!(other, CanvasItem::Actor { actor: a, .. } if *a == actor)).count();
                    Some((actor, before, slot, item))
                }
                _ => None,
            })
            .collect();
        let call_of = |actor: ObjectId, call: usize| calls.iter().find(|&&(a, n, ..)| a == actor && n == call);
        for &(actor, call, slot, item) in &calls {
            if !self.canvas_actors.iter().any(|shown| shown.actor == actor && shown.call == call)
                && let Some(entity) = self.spawn_canvas_actor(actor, call, slot, item, t)
            {
                self.canvas_actors.push(CanvasActor { actor, call, entity, slot: Some(slot) });
            }
        }
        for CanvasActor { actor, call, entity, slot } in &mut self.canvas_actors {
            let now = call_of(*actor, *call).map(|&(_, _, slot, _)| slot);
            if *slot == now {
                continue;
            }
            match now {
                Some(now) => {
                    t.commands.entity(*entity).insert(Visibility::Inherited);
                    if slot.is_none_or(|before| before != now) {
                        let layer = RenderLayers::layer(OVERLAY_LAYERS[now]);
                        for (section, _) in t.canvas.canvas_sections.iter().filter(|(_, parent)| parent.parent() == *entity) {
                            t.commands.entity(section).insert(layer.clone());
                        }
                    }
                }
                None => {
                    t.commands.entity(*entity).insert(Visibility::Hidden);
                }
            }
            *slot = now;
        }
        for (follow, drawn, mut transform) in &mut t.canvas.canvas_models {
            let Some(&(_, _, _, &CanvasItem::Actor { location, rotation, scale, .. })) = call_of(follow.actor, drawn.0) else { continue };
            let placed = FollowActor { scale, ..*follow }.transform(location, rotation);
            if *transform != placed {
                *transform = placed;
            }
        }
        // Retail draws a skeletal actor's attachments in its own scene node, so they go on the overlay layer
        // of its first call. With the base no longer HUD-drawn (dropped, detached), they return to the world.
        let Some(world) = t.play.world() else { return };
        let moves: FxHashMap<ObjectId, Mat4> = t.canvas.canvas_models.iter()
            .filter(|(_, drawn, _)| drawn.0 == 0)
            .filter_map(|(follow, _, _)| {
                let Some(&(_, _, _, &CanvasItem::Actor { location, rotation, scale, .. })) = call_of(follow.actor, 0) else { return None };
                let base = world.actor(follow.actor)?;
                let left = follow.mesh_matrix(base.location, base.rotation)?;
                Some((follow.actor, FollowActor { scale, ..*follow }.mesh_matrix(location, rotation)? * left.inverse()))
            })
            .collect();
        let places = t.particle_view.places.as_deref();
        let overlays = RenderLayers::from_layers(&OVERLAY_LAYERS);
        for (entity, follow, mut transform, mut shown, layers) in &mut t.canvas.attached {
            let on_overlay = layers.is_some_and(|layers| layers.intersects(&overlays));
            if !on_overlay && !world.attached(follow.actor) {
                continue;
            }
            let Some(actor) = world.actor(follow.actor) else { continue };
            let base = actor.attachment.as_ref().and_then(|attachment| self.canvas_actors.iter().find(|drawn| drawn.actor == attachment.base && drawn.call == 0));
            let Some(base) = base else {
                if on_overlay {
                    t.commands.entity(entity).try_insert(RenderLayers::default());
                }
                continue;
            };
            let visibility = if base.slot.is_none() || actor.hidden { Visibility::Hidden } else { Visibility::Inherited };
            if *shown != visibility {
                *shown = visibility;
            }
            let Some(slot) = base.slot else { continue };
            let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
            if layers != Some(&layer) {
                t.commands.entity(entity).try_insert(layer);
            }
            if let (Some(&moved), Some(frame)) = (moves.get(&base.actor), places.and_then(|places| places.frame(follow.actor))) {
                let placed = crate::animation::attached_transform(follow, moved * frame);
                if *transform != placed {
                    *transform = placed;
                }
            }
        }
    }

    pub(super) fn spawn_canvas_actor(&mut self, actor: ObjectId, call: usize, slot: usize, item: &CanvasItem, t: &mut SceneTargets) -> Option<Entity> {
        let placed = |follow: FollowActor| match *item {
            CanvasItem::Actor { location, rotation, scale, .. } => FollowActor { scale, ..follow }.transform(location, rotation),
            _ => Transform::default(),
        };
        if let Some((a, origin)) = t.play.world().and_then(|world| world.canvas_static(actor).map(|a| (a.clone(), world.origin()))) {
            return self.spawn_canvas_static(&a, origin, call, slot, placed, t);
        }
        let Some((path, skins, follow, origin)) =
            t.play.world().and_then(|world| world.canvas_mesh(actor).map(|mesh| (mesh.path.clone(), mesh.skins.clone(), mesh.follow, world.origin())))
        else {
            bevy::log::warn_once!("HUD actor {actor:?}: no mesh read for it");
            return None;
        };
        if self.failed_canvas_meshes.contains(&path) {
            return None;
        }
        let game = open_game(&mut self.library, self.map_system.as_deref())?;
        let Some((mesh, mesh_id, sections, handles)) = skeletal_sections(game, &mut t.assets, &mut material_images::<AssetStores>, &path, &skins) else {
            bevy::log::warn!("HUD actor {actor:?}: skeletal mesh {path} not built");
            self.failed_canvas_meshes.insert(path);
            return None;
        };
        let mesh = Arc::new(mesh);
        bevy::log::info!("HUD actor {actor:?}: {path}, {} sections, overlay slot {slot}", sections.len());
        let follow = FollowActor { skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)), ..follow };
        let posed = {
            let (pkg, data) = game.data(mesh_id);
            let animations = self.animations.entry(mesh_id).or_insert_with(|| self.vm_animations.package(game.package_name(mesh_id), pkg, data));
            animate_mesh_with(pkg, animations, handles, &mesh, &["idle"])
        };
        if let Some((mut item, _)) = posed {
            item.actor = Some(actor);
            t.animated.0.push(item);
        }
        let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
        let parent = t.commands.spawn((Transform::from_translation(origin), Visibility::default())).id();
        let materials = &mut t.assets.materials;
        let material = |section: &SectionView| {
            SectionMaterial::Standard(materials.add(crate::mesh_view::actor_standard(section, crate::mesh_view::SceneLights::Ignored)))
        };
        let lighting = ActorLighting { unlit: false, glow: 0, max_lights: DEFAULT_MAX_LIGHTS };
        let lit = ActorLit::skeletal(actor, &mesh, follow.scale, &lighting);
        spawn_actor_sections(&mut t.commands, parent, sections, placed(follow), (follow.actor, follow), material, lit, (layer, CanvasDrawn(call)));
        Some(parent)
    }

    fn spawn_canvas_static(&mut self, a: &MeshActor, origin: Vec3, call: usize, slot: usize, placed: impl Fn(FollowActor) -> Transform, t: &mut SceneTargets) -> Option<Entity> {
        let game = open_game(&mut self.library, self.map_system.as_deref())?;
        let model = self.spawn_models.entry(static_model_key(&a.static_mesh, &a.skins)).or_insert_with(|| static_model(game, t, &a.static_mesh, &a.skins));
        let Some(SpawnModel::Static { mesh, sections }) = model else {
            bevy::log::warn_once!("HUD actor {:?}: static mesh {} not built", a.object, a.static_mesh);
            return None;
        };
        let (sections, lit) = static_actor_sections(t, mesh, sections, a);
        bevy::log::debug!("HUD actor {:?}: {}, skins {:?}, {} sections, overlay slot {slot}", a.object, a.static_mesh, a.skins, sections.len());
        let follow = FollowActor { actor: a.object, scale: a.scale, pre_pivot: a.pre_pivot, skeletal: None };
        let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
        let parent = t.commands.spawn((Transform::from_translation(origin), Visibility::default())).id();
        let (materials, mesh_lit, shared) = (&mut t.assets.materials, &mut t.assets.mesh_lit, &mut self.spawn_materials);
        let material = |section: &SectionView| spawned_material(shared, materials, mesh_lit, section);
        spawn_actor_sections(&mut t.commands, parent, sections, placed(follow), (a.object, follow), material, lit, (layer, CanvasDrawn(call)));
        Some(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_takes_projectors_only_while_its_actor_is_attached() {
        let (attached, other) = (ObjectId::from_index(1), ObjectId::from_index(2));
        let list: FxHashSet<ObjectId> = [attached].into_iter().collect();
        assert_eq!(projector_mesh_tag(Some(attached), &list), ATTACHED_TAG);
        assert_eq!(projector_mesh_tag(Some(other), &list), NO_PROJECTORS_TAG);
        assert_eq!(projector_mesh_tag(None, &list), NO_PROJECTORS_TAG);
    }
}
