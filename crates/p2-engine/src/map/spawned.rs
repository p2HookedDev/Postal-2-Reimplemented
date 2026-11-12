use super::*;
use crate::mesh_view::ActorMaterialKey;
use super::scene::{PendingAssets, VmAnimations};
use bevy::math::Affine3A;
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

#[derive(Resource, Default)]
pub struct SpawnedActors {
    pub(super) spawn_models: HashMap<ModelKey, Option<SpawnModel>>,
    spawn_materials: FxHashMap<ActorMaterialKey, SectionMaterial>,
    pub(super) spawn_builds: Vec<ModelBuild>,
    pub(super) texture_builds: Vec<TextureBuild>,
    spawn_waiting: Vec<SpawnedActor>,
    pub(super) actor_textures: FxHashMap<String, Option<ActorTexture>>,
}

pub(super) struct TextureBuild {
    path: String,
    image: Handle<Image>,
    task: Task<std::thread::Result<(Game, PendingAssets, Option<ActorTexture>)>>,
}

impl TextureBuild {
    fn start(game: &Game, assets: &AssetStores, path: &str) -> Self {
        let image = assets.images.get_handle_provider().reserve_handle().typed::<Image>();
        let (mut built, mut assets, reserved, read_path) = (game.clone(), PendingAssets::new(assets), image.clone(), path.to_owned());
        let task = AsyncComputeTaskPool::get().spawn(async move {
            catch_unwind(AssertUnwindSafe(move || {
                let texture = ActorTexture::read(&mut built, &read_path, |read| assets.add_image_as(reserved, read));
                (built, assets, texture)
            }))
        });
        Self { path: path.to_owned(), image, task }
    }
}

pub(super) fn actor_image(cache: &FxHashMap<String, Option<ActorTexture>>, builds: &mut Vec<TextureBuild>, game: &Game, assets: &AssetStores, path: &str) -> Option<Handle<Image>> {
    if let Some(found) = cache.get(path) {
        return found.as_ref().map(|texture| texture.image.clone());
    }
    if let Some(build) = builds.iter().find(|build| build.path == path) {
        return Some(build.image.clone());
    }
    let build = TextureBuild::start(game, assets, path);
    let image = build.image.clone();
    builds.push(build);
    Some(image)
}

pub(super) fn static_model(game: &mut Game, assets: &mut AssetStores, path: &str, skins: &[Option<String>]) -> Option<SpawnModel> {
    let model = build_static_model(game, assets, path, skins);
    keep_static_meshes(&mut assets.meshes, model.as_ref());
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

pub(super) fn keep_static_meshes(meshes: &mut Assets<Mesh>, model: Option<&SpawnModel>) {
    let Some(SpawnModel::Static { sections, .. }) = model else { return };
    for section in sections {
        if let Some(mut mesh) = meshes.get_mut(&section.mesh) {
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
/// A static mesh lit as an actor (a mover relit as it moves) still takes static mesh projectors.
const STATIC_MESH_TAG: u32 = 4;
const MOVER_TAG: u32 = 8;
/// In a projector's tag: the decal follows a mover or Karma prop and draws on those only. Frame buffer blends stop at 6, so bit 7 is free.
const MOVER_ONLY_TAG: u32 = 1 << 7;

fn projector_mesh_tag((follow, bone): (Option<&FollowActor>, Option<&FollowBone>), attached: &FxHashSet<ObjectId>, movers: &FxHashSet<ObjectId>) -> u32 {
    let Some(actor) = follow.map(|f| f.actor).or(bone.map(|b| b.actor)).filter(|actor| attached.contains(actor)) else { return NO_PROJECTORS_TAG };
    let static_mesh = if follow.is_some_and(|f| f.skeletal.is_none()) { STATIC_MESH_TAG } else { 0 };
    let mover = if movers.contains(&actor) { MOVER_TAG } else { 0 };
    ATTACHED_TAG | static_mesh | mover
}

/// A decal following the mover it was cast on (our choice: retail is thought to leave it where it was cast, unverified).
pub(super) struct MoverDecal {
    mover: ObjectId,
    on_mover: Affine3A,
}

impl MoverDecal {
    fn new(mover: ObjectId, mover_pose: Transform, decal: Transform) -> Self {
        Self { mover, on_mover: mover_pose.compute_affine().inverse() * decal.compute_affine() }
    }

    fn place(&self, mover_pose: Transform) -> Transform {
        Transform::from_matrix(Mat4::from(mover_pose.compute_affine() * self.on_mover))
    }
}

fn mover_pose(origin: Vec3, location: p2_math::Vector, rotation: p2_math::Rotator) -> Transform {
    Transform::from_translation(origin) * unreal_transform(location, rotation, p2_math::Vector::new(1.0, 1.0, 1.0), p2_math::Vector::ZERO)
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
    pub(super) fn start_static(game: &Game, assets: &AssetStores, key: ModelKey, path: String) -> Self {
        let (mut built, mut assets) = (game.clone(), PendingAssets::new(assets));
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
        assets: &AssetStores,
        vm_animations: &VmAnimations,
        animations: &HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
        spawned: &SpawnedActor,
    ) -> Self {
        let (skeletal, path, skins) = match &spawned.draw {
            SpawnedDraw::Static(a) => (false, a.static_mesh.clone(), a.skins.clone()),
            SpawnedDraw::Skeletal(a) => (true, a.mesh.clone(), a.skins.clone()),
        };
        let (mut built, mut assets, vm_animations, mut animations) = (game.clone(), PendingAssets::new(assets), vm_animations.clone(), animations.clone());
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

fn static_actor_sections(meshes: &mut Assets<Mesh>, mesh: &Arc<StaticMesh>, sections: &[SectionView], a: &MeshActor) -> (Vec<SectionView>, Option<ActorLit>) {
    let mut sections = sections.to_vec();
    let lit = (!a.lighting.unlit).then(|| {
        for section in &mut sections {
            if let Some(copy) = meshes.get(&section.mesh).cloned() {
                section.mesh = meshes.add(copy);
            }
        }
        let shading = Shading::Vertices { mesh: Arc::clone(mesh), sphere: crate::render::actor_light::mesh_sphere(mesh), scale: a.scale, pre_pivot: a.pre_pivot };
        // Not its `MaxLights`: retail sets every listed light and the driver takes the first 8.
        ActorLit::new(a.object, MAX_HARDWARE_LIGHTS, 0.0, shading)
    });
    (sections, lit)
}

fn spawn_actor_model(spawned: SpawnedActor, origin: Vec3, t: &mut SpawnTargets) {
    let SpawnedActors { spawn_models, spawn_materials: shared, .. } = &mut *t.spawned;
    let Some(Some(model)) = spawn_models.get(&spawned.key) else { return };
    let SpawnedActor { actor, draw, .. } = spawned;
    let (sections, follow, lit) = match (model, draw) {
        (SpawnModel::Static { mesh, sections }, SpawnedDraw::Static(a)) => {
            let (mut sections, lit) = static_actor_sections(&mut t.assets.meshes, mesh, sections, &a);
            // As the map's static meshes: vertex lit, so drawn by the shader that applies projectors. An unlit one has
            // no light colours and stays on the plain material.
            for section in sections.iter_mut().filter(|_| lit.is_some()) {
                section.no_scene_lights = true;
            }
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
    let frame = t.play.world().and_then(|world| Some((world.actor(actor).is_some_and(|f| f.gone || f.hidden), world.tick_place(actor)?)));
    let (placed, shown) = match frame {
        Some((hidden, (location, rotation))) => (follow.transform(location, rotation), if hidden { Visibility::Hidden } else { Visibility::Inherited }),
        None => (Transform::default(), Visibility::Inherited),
    };
    let parent = t.commands.spawn((MapScoped, ActorModel(actor), Transform::from_translation(origin), Visibility::default())).id();
    let (materials, mesh_lit) = (&mut t.assets.materials, &mut t.assets.mesh_lit);
    let material = |section: &SectionView| spawned_material(shared, materials, mesh_lit, section);
    spawn_actor_sections(t.commands, parent, sections, placed, (actor, follow), material, lit, shown);
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
    pub(super) fn update_spawned(&mut self, t: &mut SpawnTargets) {
        let Some(world) = t.play.world_mut() else { return };
        let mut spawned = world.take_spawned();
        // Restyled actors: retail draws whatever the actor holds each frame. The old model goes, scene-built or spawned; the new
        // one is built below like a spawned actor's (its pose and ragdoll come from the actor's frames, so they carry over). A
        // HUD-drawn model is made again from its current mesh when next drawn.
        let restyled: FxHashSet<ObjectId> = spawned.iter().filter(|s| s.restyled).map(|s| s.actor).collect();
        if !restyled.is_empty() {
            let roots: bevy::ecs::entity::EntityHashSet = t.actor_models.iter().filter(|(_, model, _)| restyled.contains(&model.0)).map(|(entity, _, _)| entity).collect();
            for &entity in &roots {
                t.commands.entity(entity).despawn();
            }
            for (entity, (follow, bone), parent) in t.followers {
                let under_root = parent.is_some_and(|p| roots.contains(&p.parent()));
                if !under_root && follow.map(|f| f.actor).or(bone.map(|b| b.actor)).is_some_and(|actor| restyled.contains(&actor)) {
                    t.commands.entity(entity).despawn();
                }
            }
            t.hud.canvas_actors.retain(|shown| {
                let keep = !restyled.contains(&shown.actor);
                if !keep {
                    t.commands.entity(shown.entity).despawn();
                }
                keep
            });
            t.animated.0.retain(|item| item.actor.is_none_or(|actor| !restyled.contains(&actor)));
            t.spawned.spawn_waiting.retain(|waiting| !restyled.contains(&waiting.actor));
        }
        if spawned.is_empty() && t.spawned.spawn_builds.is_empty() && t.spawned.texture_builds.is_empty() {
            return;
        }
        let origin = world.origin();
        let emitters = spawned.iter_mut().filter_map(|s| s.emitter.take().map(|emitter| (s.actor, emitter))).collect();
        let started = std::time::Instant::now();
        self.add_particles(emitters, origin, t);
        t.times.add("spawned: emitters added", started);
        let started = std::time::Instant::now();
        t.particles.waiting_sprites.extend(spawned.iter_mut().filter_map(|s| s.sprite.take()));
        t.particles.waiting_coronas.extend(spawned.iter_mut().filter_map(|s| s.corona.take()));
        let found: Vec<SpawnedActor> = spawned.into_iter().filter_map(|s| SpawnedActor::new(s.actor, s.mesh, s.skeletal)).collect();
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        let done: Vec<TextureBuild> = t.spawned.texture_builds.extract_if(.., |build| build.task.is_finished()).collect();
        for TextureBuild { path, task, .. } in done {
            let texture = match block_on(task) {
                Ok((built, assets, texture)) => {
                    assets.insert(t.assets);
                    game.adopt(&built);
                    texture
                }
                Err(_) => {
                    bevy::log::warn!("actor texture {path}: its read failed");
                    None
                }
            };
            t.spawned.actor_textures.insert(path, texture);
        }
        let (particles, textures) = (&mut *t.particles, &mut *t.spawned);
        for texture in particles.waiting_sprites.iter().map(|sprite| &sprite.texture).chain(particles.waiting_coronas.iter().map(|corona| &corona.texture)) {
            actor_image(&textures.actor_textures, &mut textures.texture_builds, game, t.assets, texture);
        }
        let sprites: Vec<_> = particles.waiting_sprites.extract_if(.., |sprite| textures.actor_textures.contains_key(&sprite.texture)).collect();
        let coronas: Vec<_> = particles.waiting_coronas.extract_if(.., |corona| textures.actor_textures.contains_key(&corona.texture)).collect();
        t.times.add("spawned: textures", started);
        let started = std::time::Instant::now();
        // One finished model a frame: each brings its meshes, materials and images into the scene at once, and several
        // in one frame made that frame a hitch. The others wait a frame, as their actors already do.
        let done: Vec<ModelBuild> = t.spawned.spawn_builds.extract_if(.., |build| build.task.is_finished()).take(1).collect();
        for ModelBuild { key, task } in done {
            let model = match block_on(task) {
                Ok(built) => {
                    built.assets.insert(t.assets);
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
            keep_static_meshes(&mut t.assets.meshes, model.as_ref());
            t.particles.attach_late_pools(model.as_ref(), &key, t.assets);
            t.spawned.spawn_models.insert(key, model);
        }
        t.times.add("spawned: a built model taken in", started);
        let started = std::time::Instant::now();
        // A severed limb wears its body's skin (`Limb.SetupLimb` sets `Skins[0]`). An actor whose model is not built yet waits
        // for it (shown a frame or two after it spawned).
        let mut waiting = std::mem::take(&mut t.spawned.spawn_waiting);
        waiting.extend(found);
        for spawned in waiting {
            if t.play.world().is_some_and(|world| world.is_gone(spawned.actor)) {
                continue;
            }
            match t.spawned.spawn_models.get(&spawned.key) {
                Some(Some(_)) => {
                    spawn_actor_model(spawned, origin, t);
                    continue;
                }
                Some(None) => continue,
                None => {}
            }
            if !t.spawned.spawn_builds.iter().any(|build| build.key == spawned.key) {
                let build = ModelBuild::start(game, t.assets, &self.vm_animations, &self.animations, &spawned);
                t.spawned.spawn_builds.push(build);
            }
            t.spawned.spawn_waiting.push(spawned);
        }
        t.times.add("spawned: actors given models, builds started", started);
        self.add_sprites(sprites, origin, t);
        self.add_coronas(coronas, origin, t);
    }
}

pub(super) struct SpawnTargets<'a, 'w, 's> {
    pub(super) assets: &'a mut AssetStores<'w>,
    pub(super) spawned: &'a mut SpawnedActors,
    pub(super) particles: &'a mut Particles,
    pub(super) hud: &'a mut Hud,
    pub(super) animated: &'a mut Animated,
    pub(super) play: &'a mut Play,
    pub(super) actor_models: &'a Query<'w, 's, (Entity, &'static ActorModel, Has<MapModel>)>,
    pub(super) followers: &'a Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
    pub(super) commands: &'a mut Commands<'w, 's>,
    pub(super) times: &'a mut FrameTimes,
}

#[derive(SystemParam)]
pub(super) struct SpawnParams<'w, 's> {
    session: ResMut<'w, Session>,
    assets: AssetStores<'w>,
    spawned: ResMut<'w, SpawnedActors>,
    particles: ResMut<'w, Particles>,
    hud: ResMut<'w, Hud>,
    animated: ResMut<'w, Animated>,
    play: ResMut<'w, Play>,
    actor_models: Query<'w, 's, (Entity, &'static ActorModel, Has<MapModel>)>,
    followers: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_spawned(mut t: SpawnParams) {
    let started = std::time::Instant::now();
    t.session.update_spawned(&mut SpawnTargets {
        assets: &mut t.assets,
        spawned: &mut t.spawned,
        particles: &mut t.particles,
        hud: &mut t.hud,
        animated: &mut t.animated,
        play: &mut t.play,
        actor_models: &t.actor_models,
        followers: &t.followers,
        commands: &mut t.commands,
        times: &mut t.times,
    });
    t.times.add("spawned", started);
}

pub(super) struct ProjectionTargets<'a, 'w, 's> {
    pub(super) library: &'a mut Library,
    pub(super) map_system: Option<&'a Path>,
    pub(super) projectors: &'a mut Projectors,
    pub(super) play: &'a mut Play,
    pub(super) followers: &'a Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
    pub(super) untagged: &'a Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>), (Without<CanvasDrawn>, Without<MeshTag>)>,
    pub(super) images: &'a mut ResMut<'w, Assets<Image>>,
    pub(super) commands: &'a mut Commands<'w, 's>,
}

#[derive(SystemParam)]
pub(super) struct ProjectionParams<'w, 's> {
    session: ResMut<'w, Session>,
    projectors: ResMut<'w, Projectors>,
    play: ResMut<'w, Play>,
    followers: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>, Option<&'static ChildOf>), Without<CanvasDrawn>>,
    untagged: Query<'w, 's, (Entity, AnyOf<(&'static FollowActor, &'static FollowBone)>), (Without<CanvasDrawn>, Without<MeshTag>)>,
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_projections(mut t: ProjectionParams) {
    let started = std::time::Instant::now();
    let session = &mut *t.session;
    update_projections(&mut ProjectionTargets {
        library: &mut session.library,
        map_system: session.map_system.as_deref(),
        projectors: &mut t.projectors,
        play: &mut t.play,
        followers: &t.followers,
        untagged: &t.untagged,
        images: &mut t.images,
        commands: &mut t.commands,
    });
    t.times.add("projections", started);
}

/// Splats abandon their projection for the time they live (`AbandonProjector(UseLife)`, which ends the projection in the
/// engine) and destroy themselves then. Sections following an actor are tagged by whether it is in a projector's list.
pub(super) fn update_projections(t: &mut ProjectionTargets) {
    let Some(world) = t.play.world_mut() else { return };
    let projectors = &mut *t.projectors;
    projectors.dynamic_decals.retain(|&actor, &mut entity| {
        let gone = world.is_gone(actor);
        if gone {
            world.unwatch(actor);
            projectors.mover_decals.remove(&actor);
            t.commands.entity(entity).despawn();
        }
        !gone
    });
    let origin = world.origin();
    projectors.mover_decals.retain(|projector, decal| {
        if world.actor(decal.mover).is_none_or(|frame| frame.gone) {
            if let Some(entity) = projectors.dynamic_decals.remove(projector) {
                t.commands.entity(entity).despawn();
            }
            world.unwatch(*projector);
            return false;
        }
        if (world.changed(decal.mover) || world.shown.places.glides(decal.mover))
            && let Some(&entity) = projectors.dynamic_decals.get(projector)
            && let Some((location, rotation)) = world.place(decal.mover)
        {
            t.commands.entity(entity).try_insert(decal.place(mover_pose(origin, location, rotation)));
        }
        true
    });
    let mut changed: FxHashSet<ObjectId> = FxHashSet::default();
    if let Some(actors) = world.take_projector_actors() {
        changed.extend(actors.symmetric_difference(&projectors.projector_actors));
        projectors.projector_actors = actors;
    }
    let movers: FxHashSet<ObjectId> = projectors.mover_decals.values().map(|decal| decal.mover).collect();
    changed.extend(movers.symmetric_difference(&projectors.decal_movers));
    projectors.decal_movers = movers;
    if !changed.is_empty() {
        for (entity, follow, _) in t.followers {
            if follow.0.map(|f| f.actor).or(follow.1.map(|b| b.actor)).is_some_and(|actor| changed.contains(&actor)) {
                t.commands.entity(entity).try_insert(MeshTag::new(projector_mesh_tag(follow, &projectors.projector_actors, &projectors.decal_movers)));
            }
        }
    }
    for (entity, follow) in t.untagged {
        t.commands.entity(entity).try_insert(MeshTag::new(projector_mesh_tag(follow, &projectors.projector_actors, &projectors.decal_movers)));
    }
    let events = world.take_projections();
    if events.is_empty() {
        return;
    }
    let Some(game) = open_game(t.library, t.map_system) else { return };
    for (event, projector) in events {
        match event {
            ProjectorEvent::Attach(actor) => {
                let Some(projector) = projector else { continue };
                let found = match projectors.decal_images.get(&projector.texture) {
                    Some(found) => found.clone(),
                    None => {
                        let found = projector_image(game, |image| t.images.add(image), &projector.texture);
                        projectors.decal_images.insert(projector.texture.clone(), found.clone());
                        found
                    }
                };
                let Some((image, size, final_blend)) = found else { continue };
                let transform = Transform::from_translation(origin) * projector_transform(&projector, size);
                let mover = projector.follows.and_then(|mover| {
                    let world = t.play.world().filter(|world| !world.is_gone(mover))?;
                    let (location, rotation) = world.tick_place(mover)?;
                    Some(MoverDecal::new(mover, mover_pose(origin, location, rotation), transform))
                });
                let tag = projector_tag(&projector, final_blend) | if mover.is_some() { MOVER_ONLY_TAG } else { 0 };
                bevy::log::debug!(
                    "projector {actor:?} attached: {}, targets {:#x}, blending {:?}/{:?}, tag {tag:x}",
                    projector.texture,
                    projector.targets.0,
                    projector.blending,
                    projector.material_blending
                );
                let decal = ClusteredDecal { base_color_texture: Some(image), tag, ..Default::default() };
                let entity = t.commands.spawn((MapScoped, decal, transform)).id();
                if let Some(old) = projectors.dynamic_decals.insert(actor, entity) {
                    t.commands.entity(old).despawn();
                }
                match mover {
                    Some(mover) => projectors.mover_decals.insert(actor, mover),
                    None => projectors.mover_decals.remove(&actor),
                };
                if let Some(world) = t.play.world_mut() {
                    world.watch(actor);
                }
            }
            ProjectorEvent::Detach(actor) => {
                projectors.mover_decals.remove(&actor);
                if let Some(entity) = projectors.dynamic_decals.remove(&actor) {
                    t.commands.entity(entity).despawn();
                }
                if let Some(world) = t.play.world_mut() {
                    world.unwatch(actor);
                }
            }
        }
    }
}

impl Session {
    pub(super) fn spawn_canvas_static(&mut self, a: &MeshActor, origin: Vec3, call: usize, slot: usize, placed: impl Fn(FollowActor) -> Transform, t: &mut HudTargets) -> Option<Entity> {
        let game = open_game(&mut self.library, self.map_system.as_deref())?;
        let SpawnedActors { spawn_models, spawn_materials, .. } = &mut *t.spawned;
        let model = spawn_models.entry(static_model_key(&a.static_mesh, &a.skins)).or_insert_with(|| static_model(game, t.assets, &a.static_mesh, &a.skins));
        let Some(SpawnModel::Static { mesh, sections }) = model else {
            bevy::log::warn_once!("HUD actor {:?}: static mesh {} not built", a.object, a.static_mesh);
            return None;
        };
        let (sections, lit) = static_actor_sections(&mut t.assets.meshes, mesh, sections, a);
        bevy::log::debug!("HUD actor {:?}: {}, skins {:?}, {} sections, overlay slot {slot}", a.object, a.static_mesh, a.skins, sections.len());
        let follow = FollowActor { actor: a.object, scale: a.scale, pre_pivot: a.pre_pivot, skeletal: None };
        let layer = RenderLayers::layer(OVERLAY_LAYERS[slot]);
        let parent = t.commands.spawn((MapScoped, Transform::from_translation(origin), Visibility::default())).id();
        let (materials, mesh_lit, shared) = (&mut t.assets.materials, &mut t.assets.mesh_lit, spawn_materials);
        let material = |section: &SectionView| spawned_material(shared, materials, mesh_lit, section);
        spawn_actor_sections(t.commands, parent, sections, placed(follow), (a.object, follow), material, lit, (layer, CanvasDrawn(call)));
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
        let follow = |actor, skeletal| FollowActor { actor, scale: p2_math::Vector::new(1.0, 1.0, 1.0), pre_pivot: p2_math::Vector::ZERO, skeletal };
        let skinned = follow(attached, Some((p2_math::Vector::new(1.0, 1.0, 1.0), p2_math::Vector::ZERO, p2_math::Rotator::default())));
        let none = FxHashSet::default();
        assert_eq!(projector_mesh_tag((Some(&skinned), None), &list, &none), ATTACHED_TAG);
        assert_eq!(projector_mesh_tag((Some(&follow(attached, None)), None), &list, &none), ATTACHED_TAG | STATIC_MESH_TAG);
        assert_eq!(projector_mesh_tag((Some(&follow(attached, None)), None), &list, &list), ATTACHED_TAG | STATIC_MESH_TAG | MOVER_TAG);
        assert_eq!(projector_mesh_tag((Some(&follow(other, None)), None), &list, &list), NO_PROJECTORS_TAG);
        assert_eq!(projector_mesh_tag((None, None), &list, &none), NO_PROJECTORS_TAG);
    }

    #[test]
    fn a_decal_moves_and_turns_with_its_mover() {
        let mover = ObjectId::from_index(1);
        let decal = Transform::from_xyz(12.0, 1.0, 0.0).with_scale(Vec3::new(4.0, 4.0, 2.0));
        let on_mover = MoverDecal::new(mover, Transform::from_xyz(10.0, 0.0, 0.0), decal);
        let turned = Transform::from_xyz(10.0, 5.0, 0.0).with_rotation(bevy::math::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let placed = on_mover.place(turned);
        assert!(placed.translation.abs_diff_eq(Vec3::new(10.0, 6.0, -2.0), 1e-4));
        assert!(placed.rotation.abs_diff_eq(turned.rotation, 1e-4));
        assert!(placed.scale.abs_diff_eq(decal.scale, 1e-4));
        assert!(on_mover.place(Transform::from_xyz(10.0, 0.0, 0.0)).translation.abs_diff_eq(decal.translation, 1e-4));
    }
}
