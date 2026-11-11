use crate::actors::ProjectorBlending;
use super::*;
use crate::library::{LoadError, SourceLayers, SpecularMask, SpecularSource, TextureDecode, TextureSource, specular_params};
use bevy::asset::{AssetHandleProvider, AssetId};
use p2_assets::FinalBlend;
use p2_math::{Rotator, Vector};
use p2_bevy_assets::{MeshBatch, mirrors};
mod assets;
mod bsp;
mod mesh_sections;
mod projectors;
mod terrain;
mod static_meshes;
mod pawns;
mod map_actors;
pub use assets::{PendingAssets, SceneAssets};
pub(super) use assets::ActorTexture;
use bsp::{BspParts, bsp_models};
pub(super) use bsp::bsp_groups;
use mesh_sections::{still_mesh, use_uv_stream};
pub use mesh_sections::{
    SectionChunks, animate_mesh_with, sections_of_static_mesh, skeletal_mesh_sections, skeletal_sections,
};
pub(crate) use mesh_sections::VmAnimations;
pub use projectors::projector_box;
pub(super) use projectors::{projector_decal, projector_image, projector_tag, projector_transform};
use terrain::{TerrainParts, TerrainZones, terrain_models};
use static_meshes::{MeshPlacing, StaticLevel, StaticParts, antiportal_actors, cell, resolve_mesh_paths, static_models};
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

#[derive(Clone, Copy)]
struct Placement {
    location: Vector,
    rotation: Rotator,
    scale: Vector,
    pre_pivot: Vector,
}

/// `LevelInfo.SkyZone` (`ZoneInfo.LinkToSkybox` picks it as play begins; every zone links the same one), else the first
/// `SkyZoneInfo`. Its location and rotation.
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

pub(super) fn scene_preparer(game: &Game, id: PackageId, package: &str, t: &SceneTargets) -> impl FnOnce(StartedVm) -> Result<PreparedScene, LoadError> + Send + 'static {
    let (game, package, assets) = (game.clone(), package.to_owned(), PendingAssets::new(&t.assets));
    move |started| PreparedScene::build(game, id, &package, started, assets)
}

impl Session {
    pub(super) fn start_map(&mut self, package: &str, t: &mut SceneTargets) -> Result<String, LoadError> {
        let (game, id) = self.library.open(&self.root, package)?;
        let prepare = scene_preparer(game, id, package, t);
        let map_name = map_name(package);
        let (loader, id) = game.play_loader(map_name).map_err(|source| LoadError::Play { package: package.to_owned(), source })?;
        let started = start_vm(None, None, loader, id, game.configs(), &LoadProgress::default());
        let prepared = prepare(started)?;
        self.show_scene(prepared, t)
    }

    pub(super) fn show_scene(&mut self, prepared: PreparedScene, t: &mut SceneTargets) -> Result<String, LoadError> {
        let PreparedScene { emitter_models, package, game: built, animations, vm_animations, assets, scene, played, animated, emitters, sprites, coronas, actor_textures, fluids, status, hud, switches } = prepared;
        assets.insert(&mut t.assets);
        for model in emitter_models.values() {
            spawned::keep_static_meshes(&mut t.assets.meshes, model.as_ref());
        }
        t.spawned.spawn_models.extend(emitter_models);
        t.hud.hud_assets = hud;
        let (game, _) = self.library.open(&self.root, &package)?;
        game.adopt(&built);
        *t.switches = MaterialSwitches::default();
        for ((id, export), drawn) in switches {
            if let Some(id) = game.same_package(&built, id) {
                t.switches.images.entry((id, export)).or_default().push(drawn);
            }
        }
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
        t.scenes.write(crate::mesh_view::SceneChange::Show(Box::new(scene)));
        Ok(status)
    }

}

pub(super) struct SwitchTargets<'a, 'w, 's> {
    pub(super) library: &'a mut Library,
    pub(super) map_system: Option<&'a Path>,
    pub(super) switches: &'a mut MaterialSwitches,
    pub(super) play: &'a mut Play,
    pub(super) images: &'a mut ResMut<'w, Assets<Image>>,
    pub(super) commands: &'a mut Commands<'w, 's>,
}

#[derive(SystemParam)]
pub(super) struct SwitchParams<'w, 's> {
    session: ResMut<'w, Session>,
    switches: ResMut<'w, MaterialSwitches>,
    play: ResMut<'w, Play>,
    images: ResMut<'w, Assets<Image>>,
    commands: Commands<'w, 's>,
    times: ResMut<'w, FrameTimes>,
}

pub(super) fn present_material_switches(mut t: SwitchParams) {
    let started = std::time::Instant::now();
    let session = &mut *t.session;
    update_material_switches(&mut SwitchTargets {
        library: &mut session.library,
        map_system: session.map_system.as_deref(),
        switches: &mut t.switches,
        play: &mut t.play,
        images: &mut t.images,
        commands: &mut t.commands,
    });
    t.times.add("material switches", started);
}

pub(super) fn update_material_switches(t: &mut SwitchTargets) {
    let Some(world) = t.play.world_mut() else { return };
    let changes = world.take_material_switches();
    if changes.is_empty() || t.switches.images.is_empty() {
        return;
    }
    let Some(game) = open_game(t.library, t.map_system) else { return };
    let switches = &mut *t.switches;
    let (mut switched, mut looks) = (HashSet::new(), HashMap::new());
    for (switch, material) in changes {
        let Some(key) = game.export_by_path(&switch) else { continue };
        let Some(surfaces) = switches.images.get_mut(&key) else { continue };
        let Some((package, export)) = material.and_then(|path| game.export_by_path(&path)) else { continue };
        let target = switches.targets.entry((package, export)).or_insert_with(|| switch_target(game, t.images, package, export));
        let Some(SwitchTarget { image, now, size, specular_size }) = target else { continue };
        for surface in surfaces {
            let Some(mut drawn) = t.images.get_mut(surface.image) else { continue };
            *drawn = image.clone();
            switched.insert(surface.image);
            if surface.whole {
                let was = MaterialImages { state: std::mem::replace(&mut surface.shown, now.state.clone()), ..now.clone() };
                let (was, now) = (SectionView::textured(Handle::default(), Some(was)), SectionView::textured(Handle::default(), Some(now.clone())));
                looks.insert(surface.image, crate::mesh_view::SwitchedLook { was, now, size: *size, specular_size: *specular_size });
            }
        }
    }
    if !switched.is_empty() {
        t.commands.queue(move |world: &mut bevy::ecs::world::World| {
            rebind_materials(world, &switched);
            crate::mesh_view::restyle_switched(world, &looks);
        });
    }
}

#[derive(Resource, Default)]
pub struct MaterialSwitches {
    images: HashMap<(PackageId, usize), Vec<SwitchImage>>,
    targets: FxHashMap<(PackageId, usize), Option<SwitchTarget>>,
}

/// A prepared material holds the view of the texture it was prepared with, so it shows a switched image's new texture
/// only once prepared again.
fn rebind_materials(world: &mut bevy::ecs::world::World, images: &HashSet<AssetId<Image>>) {
    touch_drawing::<StandardMaterial>(world, images, |material| material);
    touch_drawing::<MeshLitMaterial>(world, images, |material| &material.base);
    touch_drawing::<crate::render::lightmapped::LightmappedMaterial>(world, images, |material| &material.base);
    touch_drawing::<crate::render::actor_light::ActorLitMaterial>(world, images, |material| &material.base);
    touch_drawing::<crate::render::blended::BlendedMaterial>(world, images, |material| &material.base);
    touch_drawing::<crate::render::blended::TerrainLayerMaterial>(world, images, |material| &material.base);
}

fn touch_drawing<M: bevy::asset::Asset>(
    world: &mut bevy::ecs::world::World,
    images: &HashSet<AssetId<Image>>,
    base: impl Fn(&M) -> &StandardMaterial,
) {
    let Some(mut store) = world.get_resource_mut::<Assets<M>>() else { return };
    let drawing: Vec<AssetId<M>> = store
        .iter()
        .filter(|(_, material)| base(material).base_color_texture.as_ref().is_some_and(|image| images.contains(&image.id())))
        .map(|(id, _)| id)
        .collect();
    for id in drawing {
        if let Some(material) = store.get_mut(id) {
            material.into_inner();
        }
    }
}

impl PreparedScene {
    #[expect(clippy::too_many_lines, reason = "phase timings; split when the phases build in parallel")]
    fn build(
        mut built: Game,
        id: PackageId,
        package: &str,
        started: StartedVm,
        mut assets: PendingAssets,
    ) -> Result<Self, LoadError> {
        let (game, t) = (&mut built, &mut assets);
        let (clock, mut lap, mut times) = (std::time::Instant::now(), std::time::Instant::now(), Vec::new());
        let mut phase = |name: &str| {
            times.push(format!("{name} {} ms", lap.elapsed().as_millis()));
            lap = std::time::Instant::now();
        };
        game.adopt_loader(&started.loader);
        let map_loader = Arc::clone(&started.loader);
        let loaded = map_loader.package(started.id);
        let (pkg, file) = (&loaded.package, &*loaded.data);
        let (name, model) = read_map_model(pkg, file, package)?;
        phase("map file");
        let mut cache: HashMap<(PackageId, i32), Option<MaterialImages>> = HashMap::new();
        let mut images: HashMap<TextureSource, Option<MaterialImages>> = HashMap::new();
        let mut details: FxHashMap<(PackageId, usize), Option<Handle<Image>>> = FxHashMap::default();
        let mut speculars: FxHashMap<SpecularSource, Option<Handle<Image>>> = FxHashMap::default();
        let mut texture = |game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32| {
            cache
                .entry((package, material))
                .or_insert_with(|| {
                    let (source, state) = game.material_source(package, material)?;
                    let image = images.entry(source).or_insert_with(|| {
                        let SourceLayers { image, glow, wet } = game.source_layers(source, TextureDecode::Keep)?;
                        let handle = t.add_image(image);
                        let glow = glow.map(|glow| t.add_image(glow));
                        let detail = source.detail.and_then(|key| {
                            details.entry(key).or_insert_with(|| game.detail_image(source).map(|detail| t.add_image(detail))).clone()
                        });
                        let specular = source.specular.and_then(|specular| {
                            let key = SpecularSource { mask: SpecularMask::None, env_map: None, ..specular };
                            let cube = speculars.entry(key).or_insert_with(|| game.specular_image(specular).map(|cube| t.add_image(cube))).clone()?;
                            Some((cube, specular_params(source)?))
                        });
                        if let Some(wet) = wet {
                            t.wet.push((handle.id(), wet));
                        }
                        Some(MaterialImages { image: handle, glow, detail, specular, state: RenderState::default() })
                    });
                    Some(MaterialImages { state, ..image.clone()? })
                })
                .clone()
        };

        let map_name = map_name(package);
        let sky_info = sky_zone_info(&started.vm, map_name);
        let hud = HudAssets::read(game, &started.vm, |image| t.add_image(image));
        let sky_zone = sky_info.map(|(location, _)| p2_collision::point_zone(&model, location));
        let polygons = model.polygons();
        let groups = bsp_groups(&model, &polygons, sky_zone);
        let drawn: usize = groups.values().map(Vec::len).sum();
        if drawn == 0 {
            return Err(LoadError::NoPolygons { name });
        }
        let BspParts { mut models, mut sky_models, textured, lit, min, max, atlases } = bsp_models(game, t, &mut texture, id, &model, &groups);
        phase("HUD and BSP");

        let mut placements: HashMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>> = HashMap::new();
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
        let decals: Vec<_> = projectors.iter().filter_map(|projector| projector_decal(game, t, projector)).collect();
        let emitter_count = emitters.len();
        let antiportals = antiportal_actors(game, &model, &antiportal_paths);
        missing += resolve_mesh_paths(game, id, mesh_paths, &mut placements);
        phase("projectors and meshes found");

        let zones = TerrainZones { model: &model, sky_zone, terrain_zones: &terrain_zones };
        let TerrainParts { models: terrain_drawn, sky_models: terrain_sky, static_terrains, terrain_sectors, terrains, hidden_terrains, drawn_decorations } =
            terrain_models(game, t, &mut texture, id, &zones, terrain_infos);
        models.extend(terrain_drawn);
        sky_models.extend(terrain_sky);
        phase("terrain");
        let actors: usize = placements.values().map(Vec::len).sum();
        let meshes = placements.keys().map(|(mesh, _)| mesh).collect::<HashSet<_>>().len();
        let environment = Arc::new(LightEnvironment::new(mesh_lights, zone_ambients, level_brightness).with_terrain(terrain_sectors));
        let level = StaticLevel { pkg, file, model: &model, environment: &environment, sky_zone };
        let (parts, static_missing, static_batches) = static_models(game, t, &mut texture, &level, placements);
        missing += static_missing;
        let StaticParts { models: static_drawn, sky_models: static_sky, mut followers, collisions, lit_actors, .. } = parts;
        models.extend(static_drawn);
        sky_models.extend(static_sky);
        phase("static meshes");

        let vm_animations = VmAnimations::of(&played.0, &played.1);
        let mut animations = HashMap::new();
        let pawn_parts = pawn_models(game, t, &mut texture, &mut animations, &vm_animations, pawns, &attachments);
        followers.extend(pawn_parts.followers);
        let animated_meshes = pawn_parts.animated;
        let (attached, drawn_pawns, drawn_attachments) = (pawn_parts.attached, pawn_parts.drawn_pawns, pawn_parts.drawn_attachments);
        phase("pawns");
        let switches = switched_images(game, &images, &cache);
        keep_restyled_out_of_world_pass(&mut models, switches.iter().map(|(_, switch)| switch));
        let blocking = collisions.len();
        let status = format!(
            "{name}: {drawn} of {} BSP polygons ({textured} textured, {lit} light-mapped); \
             {actors} static mesh actors ({lit_actors} lit, {blocking} blocking, static ones in {static_batches} merged meshes), {meshes} meshes ({missing} not found); {emitter_count} particle emitter actors,{} of {projector_count} projectors; \
             {terrains} terrains ({hidden_terrains} in zones without terrain, {drawn_decorations} decorations), {drawn_pawns} pawns (posed by their scripts while playing), {drawn_attachments} attachments; {script_errors} start-up script errors",
            polygons.len(),
            decals.len(),
        );
        let sky = sky_info.zip(sky_zone).map(|((location, rotation), zone)| Sky {
            zone,
            position: Vec3::from(to_bevy(location)),
            rotation: unreal_to_bevy(rotator_matrix(rotation)).rotation,
            models: sky_models,
        });
        let center = (min + max) / 2.0;
        let scene = Scene {
            models,
            center,
            radius: ((max - min).length() / 2.0).max(1.0),
            sky,
            start: start.map(|(location, rotation)| (Vec3::from(to_bevy(location)), rotation.yaw)),
            collision: Some(StaticCollision { hulls: p2_collision::BspHulls::new(&model), model, terrains: static_terrains, mesh_set: p2_collision::MeshSet::new(&collisions), meshes: collisions }),
            fogs: zone_fogs,
            decals,
            actors: followers,
            attachments: attached,
            lighting: Some(environment),
            antiportals,
            light_maps: atlases,
        };
        phase("rest");
        bevy::log::info!("load: {package} scene built in {} ms ({})", clock.elapsed().as_millis(), times.join(", "));
        Ok(Self { emitter_models, package: package.to_owned(), game: built, animations, vm_animations, assets, scene, played, animated: animated_meshes, emitters, sprites, coronas, actor_textures, fluids, status, hud, switches })
    }
}

pub(super) struct SwitchImage {
    image: AssetId<Image>,
    /// The switch itself ends at this image, so its surfaces take all of a new material (`restyle_switched`). A material
    /// around the switch merged its own state into the walk's: those surfaces only get the new image.
    whole: bool,
    shown: RenderState,
}

pub(super) struct SwitchTarget {
    image: Image,
    now: MaterialImages,
    size: [f32; 2],
    specular_size: [f32; 2],
}

fn switch_target(game: &mut Game, images: &mut Assets<Image>, package: PackageId, export: usize) -> Option<SwitchTarget> {
    let (source, state) = game.material_source(package, export_reference(export))?;
    let SourceLayers { mut image, glow, .. } = game.source_layers(source, TextureDecode::Keep)?;
    // Uploaded as a new texture with all its mips: Bevy rewrites a texture of the same descriptor with
    // `COPY_DST` in place, mip 0 only, so the smaller mips kept the old picture past some distance from the
    // camera. Without `COPY_DST` the descriptor differs; wgpu adds it for the upload itself.
    image.texture_descriptor.usage.remove(bevy::render::render_resource::TextureUsages::COPY_DST);
    let image_size = |image: &Image| image.size().as_vec2().to_array();
    let (size, specular) = (image_size(&image), game.specular_layer(source));
    let specular_size = specular.as_ref().map_or([1.0; 2], |(specular, _)| image_size(specular));
    let now = MaterialImages {
        image: Handle::default(),
        glow: glow.map(|glow| images.add(glow)),
        detail: game.detail_image(source).map(|detail| images.add(detail)),
        specular: specular.map(|(specular, params)| (images.add(specular), params)),
        state,
    };
    Some(SwitchTarget { image, now, size, specular_size })
}

fn switched_images(
    game: &mut Game,
    images: &HashMap<TextureSource, Option<MaterialImages>>,
    cache: &HashMap<(PackageId, i32), Option<MaterialImages>>,
) -> Vec<((PackageId, usize), SwitchImage)> {
    let mut users_of: FxHashMap<AssetId<Image>, Vec<(PackageId, i32, &MaterialImages)>> = FxHashMap::default();
    for (&(package, material), used) in cache {
        if let Some(used) = used {
            users_of.entry(used.image.id()).or_default().push((package, material, used));
        }
    }
    let mut switched = Vec::new();
    for (source, found) in images.iter().filter(|(source, _)| source.wet.is_none()) {
        let (Some(switch), Some(found)) = (source.switch, found) else { continue };
        let image = found.image.id();
        let users = users_of.get(&image).map_or(&[][..], Vec::as_slice);
        let whole = !users.is_empty() && users.iter().all(|&(package, material, _)| game.export_of(package, material) == Some(switch));
        if whole || (source.combine.is_none() && source.opacity.is_none() && source.glow.is_none()) {
            let shown = users.first().map_or_else(RenderState::default, |(_, _, used)| used.state.clone());
            switched.push((switch, SwitchImage { image, whole, shown }));
        }
    }
    switched
}

fn keep_restyled_out_of_world_pass<'a>(models: &mut [ModelView], switches: impl Iterator<Item = &'a SwitchImage>) {
    let restyled: HashSet<AssetId<Image>> = switches.filter(|switch| switch.whole).map(|switch| switch.image).collect();
    for section in models.iter_mut().flat_map(|model| &mut model.sections) {
        if section.texture.as_ref().is_some_and(|texture| restyled.contains(&texture.id())) {
            section.chunks = None;
        }
    }
}


#[cfg(test)]
mod tests;
