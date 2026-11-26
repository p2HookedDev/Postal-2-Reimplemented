use crate::actors::ProjectorBlending;
use super::*;
use p2_bevy_assets::library::LoadError;
use p2_render::level::{
    MaterialSwitches, switch_materials,
    BspParts, MaterialCache, MeshPlacing, Placement, StaticLevel, StaticParts, SwitchImage, TerrainParts, TerrainZones, antiportal_actors, bake_lamps, bsp_models,
    bsp_groups, mark_restyled, resolve_mesh_paths, static_collision, static_models, switched_images, terrain_models,
};
use p2_render::lamps::LoadBake;
use p2_render::level_collision::LevelAlbedos;
use p2_assets::FinalBlend;
use p2_math::{Rotator, Vector};
mod mesh_sections;
mod projectors;
mod pawns;
mod map_actors;
use mesh_sections::still_mesh;
pub use mesh_sections::animate_mesh_with;
pub(crate) use mesh_sections::VmAnimations;
pub use projectors::projector_box;
pub(super) use projectors::{plain_projector_blend, projector_decal, projector_tag, projector_transform};
use pawns::pawn_models;
use map_actors::{MapActors, read_map_actors, read_map_model};

pub(crate) struct PreparedScene {
    emitter_models: HashMap<spawned::ModelKey, Option<spawned::SpawnModel>>,
    package: String,
    game: Game,
    animations: HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    vm_animations: VmAnimations,
    assets: PendingAssets,
    scene: Scene,
    played: (Vm<Arc<Loader>>, Arc<Loader>, Option<(ObjectId, Option<ObjectId>)>, Vec<Option<ObjectId>>),
    animated: Vec<AnimatedMesh>,
    emitters: Vec<(ObjectId, EmitterActor)>,
    sprites: Vec<SpriteActor>,
    coronas: Vec<CoronaActor>,
    actor_textures: FxHashMap<String, Option<ActorTexture>>,
    fluids: Vec<fluids::FluidActor>,
    status: String,
    hud: HudAssets,
    switches: Vec<((PackageId, usize), SwitchImage)>,
}

pub(super) fn sky_zone_info(vm: &Vm<impl p2_vm::LoaderRef>, map: &str) -> Option<(Vector, Rotator)> {
    let prefix = format!("{}.", map.to_ascii_lowercase());
    let live = || vm.actors().filter(|&o| !vm.store.is_deleted(o) && vm.store.object(o).path.to_ascii_lowercase().starts_with(&prefix));
    let linked = live().filter(|&o| vm.store.is_a(o, Kind::LevelInfo)).find_map(|info| match vm.store.get(info, p2_vm::Prop::SkyZone) {
        Some(p2_vm::Value::Object(Some(sky))) => Some(sky),
        _ => None,
    });
    let sky = linked.or_else(|| live().find(|&o| vm.store.is_a(o, "SkyZoneInfo")))?;
    Some(location_rotation(vm, sky))
}

pub(super) const SCENE_START: f32 = 0.1;
const BAKE_PHASE: &str = "lamps and probes";
const SCENE_PHASES: [(&str, f32); 9] = [
    ("map file", 7.0),
    ("HUD", 10.0),
    ("actors", 20.0),
    ("projectors and meshes found", 5.0),
    ("static meshes", 320.0),
    ("pawns", 235.0),
    ("terrain, waited for", 150.0),
    ("rest", 1.0),
    (BAKE_PHASE, 2200.0),
];

pub(super) fn scene_preparer(game: &Game, id: PackageId, package: &str, t: &SceneTargets) -> impl FnOnce(StartedVm, &LoadProgress) -> Result<PreparedScene, LoadError> + Send + 'static {
    let (game, package, assets) = (game.clone(), package.to_owned(), PendingAssets::new(&t.assets.meshes, &t.assets.images, &t.assets.bindposes));
    let mut painted = t.painter.painted();
    painted.extend(t.hud.hud_assets.paths().cloned());
    move |started, progress| PreparedScene::build(game, id, &package, started, assets, &painted, progress)
}

impl Session {
    pub(super) fn start_map(&mut self, package: &str, t: &mut SceneTargets) -> Result<String, LoadError> {
        let (game, id) = self.library.open(&self.root, package)?;
        let prepare = scene_preparer(game, id, package, t);
        let map_name = map_name(package);
        let (loader, id) = play_loader(game, map_name).map_err(|source| LoadError::Play { package: package.to_owned(), source })?;
        let progress = LoadProgress::default();
        let started = start_vm(None, None, loader, id, configs(game), &progress);
        let prepared = prepare(started, &progress)?;
        self.show_scene(prepared, t)
    }

    pub(super) fn show_scene(&mut self, prepared: PreparedScene, t: &mut SceneTargets) -> Result<String, LoadError> {
        let PreparedScene { emitter_models, package, game: built, animations, vm_animations, assets, scene, played, animated, emitters, sprites, coronas, actor_textures, fluids, status, hud, switches } = prepared;
        assets.insert(&mut t.assets.meshes, &mut t.assets.images, &mut t.assets.bindposes, &mut t.assets.wet);
        for model in emitter_models.values() {
            spawned::keep_static_meshes(&mut t.assets.meshes, model.as_ref());
        }
        t.spawned.spawn_models.extend(emitter_models);
        t.hud.hud_assets.add(hud);
        let (game, _) = self.library.open(&self.root, &package)?;
        game.adopt(&built);
        *t.switches = MaterialSwitches::default();
        t.switches.extend(switches.into_iter().filter_map(|((id, export), drawn)| Some(((game.same_package(&built, id)?, export), drawn))));
        self.animations = animations.into_iter().filter_map(|(id, animation)| Some((game.same_package(&built, id)?, animation))).collect();
        self.vm_animations = vm_animations;
        t.spawned.actor_textures = actor_textures;
        self.map_system = Some(Library::system_of(&self.root, &package));
        self.map = Some(package);
        t.animated.0.extend(animated);
        let center = scene.center;
        let (vm, loader, player, zone_actors) = played;
        t.play.set(Some(PlayWorld::new(vm, loader, center, player, zone_actors, self.root.join("Music"))));
        let spawn = &mut t.spawn_targets();
        self.add_particles(emitters, -center, spawn);
        self.add_sprites(sprites, -center, spawn);
        self.add_coronas(coronas, -center, spawn);
        self.add_fluids(fluids, -center, t);
        t.scenes.write(p2_render::scene::SceneChange::Show(Box::new(scene)));
        Ok(status)
    }

}

#[derive(SystemParam)]
pub(super) struct SwitchParams<'w, 's> {
    switches: ResMut<'w, MaterialSwitches>,
    play: ResMut<'w, Play>,
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_material_switches(mut session: ResMut<Session>, mut t: SwitchParams) {
    let started = std::time::Instant::now();
    let changes = t.play.world_mut().map(PlayWorld::take_material_switches).unwrap_or_default();
    let session = &mut *session;
    if !changes.is_empty()
        && let Some(game) = open_game(&mut session.library, session.map_system.as_deref())
    {
        switch_materials(game, &mut t.switches, &mut t.images, &mut t.commands, changes);
    }
    t.times.add("material switches", started);
}

struct BspBuilt {
    parts: Option<BspParts>,
    drawn: usize,
    polygons: usize,
}

fn spawn_bsp_and_hud(
    game: &Game,
    t: &PendingAssets,
    materials: &Arc<MaterialCache>,
    model: Model,
    sky_zone: Option<u8>,
    id: PackageId,
    hud: HudWanted,
) -> Result<std::thread::JoinHandle<(BspBuilt, HudAssets, PendingAssets)>, LoadError> {
    let (mut game, mut t, materials) = (game.frozen_clone(), t.fork(), Arc::clone(materials));
    std::thread::Builder::new()
        .name("map loading: BSP and HUD".into())
        .spawn(move || {
            let polygons = model.polygons();
            let groups = bsp_groups(&model, &polygons, sky_zone);
            let drawn = groups.values().map(Vec::len).sum();
            let mut texture = |game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32| materials.images(game, t, package, material);
            let parts = (drawn > 0).then(|| bsp_models(&mut game, &mut t, &mut texture, id, &model, &groups));
            let hud = hud.read(&mut game, |image| t.add_image(image));
            (BspBuilt { parts, drawn, polygons: polygons.len() }, hud, t)
        })
        .map_err(LoadError::Thread)
}

impl PreparedScene {
    #[expect(clippy::too_many_lines, reason = "phase timings")]
    fn build(
        mut built: Game,
        id: PackageId,
        package: &str,
        started: StartedVm,
        mut assets: PendingAssets,
        painted: &HashSet<String>,
        progress: &LoadProgress,
    ) -> Result<Self, LoadError> {
        let (game, t) = (&mut built, &mut assets);
        let (clock, mut lap, mut times) = (std::time::Instant::now(), std::time::Instant::now(), Vec::new());
        let bakes = p2_render::lamps::bakes_at_load();
        let weight = |name: &str| SCENE_PHASES.iter().find(|(phase, _)| *phase == name).map_or(0.0, |&(phase, ms)| if phase == BAKE_PHASE && !bakes { 0.0 } else { ms });
        let total: f32 = SCENE_PHASES.iter().map(|(name, _)| weight(name)).sum();
        let mut done = 0.0;
        let mut phase = |name: &str| {
            times.push(format!("{name} {} ms", lap.elapsed().as_millis()));
            lap = std::time::Instant::now();
            done += weight(name);
            progress.set(SCENE_START + (1.0 - SCENE_START) * done / total);
        };
        game.adopt_loader(&started.loader);
        let map_loader = Arc::clone(&started.loader);
        let loaded = map_loader.package(started.id);
        let (pkg, file) = (&loaded.package, &*loaded.data);
        let (name, model) = read_map_model(pkg, file, package)?;
        phase("map file");
        let materials = Arc::new(MaterialCache::default());
        let mut texture = |game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32| materials.images(game, t, package, material);

        let map_name = map_name(package);
        let sky_info = sky_zone_info(&started.vm, map_name);
        let sky_zone = sky_info.map(|(location, _)| p2_collision::point_zone(&model, location));
        let bsp = spawn_bsp_and_hud(game, t, &materials, model.clone(), sky_zone, id, HudAssets::wanted(&started.vm, painted))?;
        phase("HUD");

        let mut placements: BTreeMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>> = BTreeMap::new();
        let mut missing = 0;
        let MapActors {
            terrain_infos,
            start,
            mesh_paths,
            zone_fogs,
            terrain_zones,
            mesh_lights,
            zone_ambients,
            level_brightness,
            attachments,
            script_errors,
            played,
            pawns,
            projectors,
            antiportal_paths,
            emitters,
            sprites,
            coronas,
            fluids,
        } = read_map_actors(started, &model);
        let emitter_textures = emitters.iter().flat_map(|(_, emitter)| &emitter.emitters).filter(|(p, _)| p.kind != p2_particles::EmitterKind::Mesh).filter_map(|(_, texture)| texture.as_deref());
        let mut actor_textures = FxHashMap::default();
        for path in emitter_textures.chain(sprites.iter().map(|sprite| sprite.texture.as_str())).chain(coronas.iter().map(|corona| corona.texture.as_str())) {
            actor_textures.entry(path.to_owned()).or_insert_with(|| ActorTexture::read(game, path, |image| t.add_image(image)));
        }
        let emitter_meshes = emitters.iter().flat_map(|(_, emitter)| emitter.emitters.iter().zip(&emitter.meshes)).filter(|((p, _), _)| p.kind == p2_particles::EmitterKind::Mesh).filter_map(|(_, mesh)| mesh.as_deref());
        let mut emitter_models = HashMap::new();
        for path in emitter_meshes {
            emitter_models.entry(spawned::static_model_key(path, &[])).or_insert_with(|| spawned::build_static_model(game, t, path, &[]));
        }
        phase("actors");
        let projector_count = projectors.len();
        let mut decal_images = HashMap::new();
        let decals: Vec<_> = projectors.iter().filter_map(|projector| projector_decal(game, t, projector, &mut decal_images)).collect();
        let emitter_count = emitters.len();
        let antiportals = antiportal_actors(game, &model, &antiportal_paths);
        missing += resolve_mesh_paths(game, id, mesh_paths, &mut placements);
        phase("projectors and meshes found");

        let zones = TerrainZones { model: &model, sky_zone, terrain_zones: &terrain_zones };
        let actors: usize = placements.values().map(Vec::len).sum();
        let meshes = placements.keys().map(|(mesh, _)| mesh).collect::<HashSet<_>>().len();
        let lights = LightEnvironment::new(mesh_lights, zone_ambients, level_brightness).suns_of_one_zone(|at| p2_collision::point_zone(&model, at));
        let vm_animations = VmAnimations::of(&played.0, &played.1);
        let mut animations = HashMap::new();
        let (terrain, (parts, static_missing, static_batches), pawn_parts) = std::thread::scope(|scope| {
            let terrain = {
                let (mut game, mut t, materials, zones) = (game.frozen_clone(), t.fork(), &materials, &zones);
                scope.spawn(move || {
                    let mut texture = |game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32| materials.images(game, t, package, material);
                    (terrain_models(&mut game, &mut t, &mut texture, id, zones, terrain_infos), t)
                })
            };
            let level = StaticLevel { pkg, file, model: &model, environment: &lights, sky_zone };
            let statics = static_models(game, t, &mut texture, &level, placements);
            phase("static meshes");
            let pawn_parts = pawn_models(game, t, &mut texture, &mut animations, &vm_animations, pawns, &attachments);
            phase("pawns");
            (terrain.join(), statics, pawn_parts)
        });
        let (terrain, terrain_assets) = terrain.map_err(|_| LoadError::Panicked { package: package.to_owned() })?;
        t.merge(terrain_assets);
        let TerrainParts { models: terrain_drawn, sky_models: terrain_sky, static_terrains, terrain_albedos, terrain_sectors, terrains, hidden_terrains, drawn_decorations } = terrain;
        phase("terrain, waited for");
        let environment = Arc::new(lights.with_terrain(terrain_sectors));
        missing += static_missing;
        let StaticParts { models: static_drawn, sky_models: static_sky, mut followers, collisions, lamp_casters, lamp_caster_albedos, lamp_masked, lit_actors, .. } = parts;
        followers.extend(pawn_parts.followers);
        let animated_meshes = pawn_parts.animated;
        let (attached, drawn_pawns, drawn_attachments) = (pawn_parts.attached, pawn_parts.drawn_pawns, pawn_parts.drawn_attachments);
        let (bsp, hud, bsp_assets) = bsp.join().map_err(|_| LoadError::Panicked { package: package.to_owned() })?;
        let BspBuilt { parts, drawn, polygons } = bsp;
        let BspParts { mut models, mut sky_models, textured, lit, min, max, atlases, masked_casters: mut lamp_masked_all, material_albedos, shadow_surfaces } =
            parts.ok_or_else(|| LoadError::NoPolygons { name: name.clone() })?;
        t.merge(bsp_assets);
        models.extend(terrain_drawn);
        models.extend(static_drawn);
        sky_models.extend(terrain_sky);
        sky_models.extend(static_sky);
        lamp_masked_all.extend(lamp_masked);
        phase("BSP and HUD, waited for");
        let switches = switched_images(game, &materials);
        mark_restyled(&mut models, switches.iter().map(|(_, switch)| switch));
        let blocking = collisions.len();
        let status = format!(
            "{name}: {drawn} of {} BSP polygons ({textured} textured, {lit} light-mapped); \
             {actors} static mesh actors ({lit_actors} lit, {blocking} blocking, static ones in {static_batches} merged meshes), {meshes} meshes ({missing} not found); {emitter_count} particle emitter actors,{} of {projector_count} projectors; \
             {terrains} terrains ({hidden_terrains} in zones without terrain, {drawn_decorations} decorations), {drawn_pawns} pawns (posed by their scripts while playing), {drawn_attachments} attachments; {script_errors} start-up script errors",
            polygons,
            decals.len(),
        );
        let sky = sky_info.zip(sky_zone).map(|((location, rotation), zone)| Sky {
            zone,
            position: Vec3::from(to_bevy(location)),
            rotation: unreal_to_bevy(rotator_matrix(rotation)).rotation,
            models: sky_models,
        });
        let center = (min + max) / 2.0;
        let mut scene = Scene {
            models,
            center,
            radius: ((max - min).length() / 2.0).max(1.0),
            sky,
            start: start.map(|(location, rotation)| (Vec3::from(to_bevy(location)), rotation.yaw)),
            collision: Some(static_collision(
                model,
                static_terrains,
                collisions,
                lamp_casters,
                lamp_masked_all,
                LevelAlbedos { bsp: material_albedos, casters: lamp_caster_albedos, terrains: terrain_albedos },
            )),
            fogs: zone_fogs,
            decals,
            actors: followers,
            attachments: attached,
            lighting: Some(environment),
            light_bakes: None,
            antiportals,
            light_maps: atlases,
            shadow_surfaces,
        };
        phase("rest");
        let baked = bake_lamps(&mut scene);
        phase(BAKE_PHASE);
        match (times.last_mut(), baked) {
            (Some(field), Some(LoadBake::Baked { lamps, probes })) => *field = format!("lamps {} ms, probes {} ms", lamps.as_millis(), probes.as_millis()),
            (Some(field), Some(LoadBake::Cached)) => field.push_str(" from cache"),
            _ => {}
        }
        bevy::log::info!("load: {package} scene built in {} ms ({})", clock.elapsed().as_millis(), times.join(", "));
        Ok(Self { emitter_models, package: package.to_owned(), game: built, animations, vm_animations, assets, scene, played, animated: animated_meshes, emitters, sprites, coronas, actor_textures, fluids, status, hud, switches })
    }
}


#[cfg(test)]
mod tests;
