use crate::actors::ProjectorBlending;
use super::*;
use crate::library::{LoadError, TextureDecode, TextureSource};
use bevy::asset::{AssetHandleProvider, AssetId};
use p2_assets::FinalBlend;
use p2_math::{Rotator, Vector};
use p2_bevy_assets::{MeshBatch, mirrors};

const CELL: f32 = 2048.0;

fn cell(position: Vec3) -> [i32; 2] {
    [(position.x / CELL).floor() as i32, (position.z / CELL).floor() as i32]
}

pub trait SceneAssets {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh>;
    fn add_image(&mut self, image: Image) -> Handle<Image>;
    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes>;
    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2>;
}

impl SceneAssets for AssetStores<'_> {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        self.meshes.add(mesh)
    }

    fn add_image(&mut self, image: Image) -> Handle<Image> {
        self.images.add(image)
    }

    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes> {
        self.bindposes.add(bindposes)
    }

    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2> {
        self.images.get(image).map(Image::size)
    }
}

pub struct PendingAssets {
    mesh_ids: AssetHandleProvider,
    image_ids: AssetHandleProvider,
    bindpose_ids: AssetHandleProvider,
    meshes: Vec<(Handle<Mesh>, Mesh)>,
    images: Vec<(Handle<Image>, Image)>,
    image_sizes: HashMap<AssetId<Image>, UVec2>,
    bindposes: Vec<(Handle<SkinnedMeshInverseBindposes>, SkinnedMeshInverseBindposes)>,
    pub wet: Vec<(AssetId<Image>, crate::library::WetSource)>,
}

impl PendingAssets {
    pub fn new(t: &SceneTargets) -> Self {
        Self {
            mesh_ids: t.assets.meshes.get_handle_provider(),
            image_ids: t.assets.images.get_handle_provider(),
            bindpose_ids: t.assets.bindposes.get_handle_provider(),
            meshes: Vec::new(),
            images: Vec::new(),
            image_sizes: HashMap::new(),
            bindposes: Vec::new(),
            wet: Vec::new(),
        }
    }

    pub(super) fn add_image_as(&mut self, handle: Handle<Image>, image: Image) -> Handle<Image> {
        self.image_sizes.insert(handle.id(), image.size());
        self.images.push((handle.clone(), image));
        handle
    }

    pub fn insert(self, t: &mut SceneTargets) {
        for (image, source) in self.wet {
            t.assets.wet.add(image, source);
        }
        let failed = self.meshes.into_iter().map(|(handle, mesh)| t.assets.meshes.insert(handle.id(), mesh).is_err())
            .chain(self.images.into_iter().map(|(handle, image)| t.assets.images.insert(handle.id(), image).is_err()))
            .chain(self.bindposes.into_iter().map(|(handle, bindposes)| t.assets.bindposes.insert(handle.id(), bindposes).is_err()))
            .filter(|&failed| failed)
            .count();
        if failed > 0 {
            bevy::log::warn!("{failed} scene assets lost their handles");
        }
    }
}

impl SceneAssets for PendingAssets {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        let handle = self.mesh_ids.reserve_handle().typed::<Mesh>();
        self.meshes.push((handle.clone(), mesh));
        handle
    }

    fn add_image(&mut self, image: Image) -> Handle<Image> {
        let handle = self.image_ids.reserve_handle().typed::<Image>();
        self.add_image_as(handle, image)
    }

    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes> {
        let handle = self.bindpose_ids.reserve_handle().typed::<SkinnedMeshInverseBindposes>();
        self.bindposes.push((handle.clone(), bindposes));
        handle
    }

    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2> {
        self.image_sizes.get(&image.id()).copied()
    }
}

#[derive(Clone)]
pub(super) struct ActorTexture {
    pub(super) image: Handle<Image>,
    pub(super) texels: f32,
    pub(super) blending: Option<Blending>,
}

impl ActorTexture {
    pub(super) fn read(game: &mut Game, path: &str, add: impl FnOnce(Image) -> Handle<Image>) -> Option<Self> {
        let (package, export) = game.export_by_path(path)?;
        let texture = game.material_texture(package, export_reference(export))?;
        let (texels, blending) = (texture.image.size().x as f32, texture.state.blending);
        Some(Self { image: add(texture.image), texels, blending })
    }
}

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
    actor_textures: HashMap<String, Option<ActorTexture>>,
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

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BspKey {
    in_sky: bool,
    material: i32,
    atlas: Option<usize>,
    flags: u32,
    zones: Option<Vec<u8>>,
    cell: [i32; 2],
}

/// Invisible surfaces and portals are left out; fake backdrop surfaces too where a sky zone shows
/// through them (unverified). Opaque and masked surfaces are not split by zone or place, to keep draws few; the depth test
/// hides what is behind. Translucent ones are, as they sort by their mesh's centre: a node by its front zone, two-sided
/// ones the eye's side.
pub(super) fn bsp_groups<'a>(
    model: &Model,
    polygons: &'a [ModelPolygon],
    sky_zone: Option<u8>,
) -> BTreeMap<BspKey, Vec<(&'a ModelPolygon, Option<Vec<[f32; 2]>>)>> {
    let backdrop = if sky_zone.is_some() { poly_flags::FAKE_BACKDROP } else { 0 };
    let hidden = poly_flags::INVISIBLE | poly_flags::PORTAL | backdrop;
    let drawing = poly_flags::MASKED | poly_flags::TRANSLUCENT | poly_flags::TWO_SIDED;
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for polygon in polygons.iter().filter(|p| p.poly_flags & hidden == 0) {
        let node = model.nodes.get(polygon.node);
        let in_sky = sky_zone.is_some_and(|zone| node.is_some_and(|node| node.zones[1] == zone));
        let translucent = polygon.poly_flags & poly_flags::TRANSLUCENT != 0;
        let zones = node.filter(|_| translucent && !in_sky).map(|node| {
            let mut zones = if polygon.poly_flags & poly_flags::TWO_SIDED == 0 { vec![node.zones[1]] } else { node.zones.to_vec() };
            zones.sort_unstable();
            zones.dedup();
            zones
        });
        let at = if zones.is_some() { polygon.positions.first().map_or([0, 0], |&p| cell(Vec3::from(to_bevy(p)))) } else { [0, 0] };
        let (atlas, uvs) = model.light_map_uvs(polygon).unzip();
        groups.entry(BspKey { in_sky, material: polygon.material, atlas, flags: polygon.poly_flags & drawing, zones, cell: at }).or_default().push((polygon, uvs));
    }
    groups
}

/// `LevelInfo.SkyZone` (`ZoneInfo.LinkToSkybox` picks it as play begins; every zone links the same one), else the first
/// `SkyZoneInfo`. Its location and rotation.
pub(super) fn sky_zone_info(vm: &Vm<impl p2_vm::LoaderRef>, map: &str) -> Option<(Vector, Rotator)> {
    let prefix = format!("{}.", map.to_ascii_lowercase());
    let live = || vm.actors().filter(|&o| !vm.is_deleted(o) && vm.object(o).path.to_ascii_lowercase().starts_with(&prefix));
    let linked = live().filter(|&o| vm.is_a(o, Kind::LevelInfo)).find_map(|info| match vm.get(info, "SkyZone") {
        Some(p2_vm::Value::Object(Some(sky))) => Some(sky),
        _ => None,
    });
    let sky = linked.or_else(|| live().find(|&o| vm.is_a(o, "SkyZoneInfo")))?;
    Some(location_rotation(vm, sky))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SectionChunks {
    Whole,
    Chunked,
}

pub fn sections_of_static_mesh<S: SceneAssets>(mesh: &StaticMesh, t: &mut S, chunks: SectionChunks, mut texture: impl FnMut(&mut S, usize) -> Option<MaterialImages>) -> Vec<SectionView> {
    static_mesh_to_meshes(mesh)
        .into_iter()
        .enumerate()
        .map(|(i, mut section)| {
            let found = texture(t, i);
            use_uv_stream(&mut section, mesh, found.as_ref());
            let chunks = if chunks == SectionChunks::Chunked { crate::render::world_pass::chunks_of(&mut section) } else { None };
            SectionView { chunks, ..SectionView::textured(t.add_mesh(section), found) }
        })
        .collect()
}

fn use_uv_stream(section: &mut Mesh, mesh: &StaticMesh, found: Option<&MaterialImages>) {
    let stream = found.and_then(|found| found.state.uv_stream).filter(|&stream| stream > 0);
    if let Some(uvs) = stream.and_then(|stream| mesh.uvs.get(usize::from(stream))) {
        section.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
    }
}

#[derive(Clone, Default)]
pub(crate) struct VmAnimations(Option<(Arc<Loader>, p2_vm::MeshAnimations)>);

impl VmAnimations {
    fn of(vm: &Vm<Arc<Loader>>, loader: &Arc<Loader>) -> Self {
        Self(Some((Arc::clone(loader), vm.mesh_animations())))
    }

    pub(crate) fn package(&self, name: &str, pkg: &Package, data: &[u8]) -> Arc<[Arc<MeshAnimation>]> {
        let vm = self.0.as_ref().and_then(|(loader, read)| Some((loader.loaded_id(name)?, read)));
        pkg.exports
            .iter()
            .enumerate()
            .filter(|(_, e)| pkg.object_name(e.class_index) == Some("MeshAnimation"))
            .filter_map(|(export, e)| match vm.and_then(|(id, read)| read.get(id, export)) {
                Some(read) => read,
                None => read_mesh_animation(pkg, data, e).ok().map(Arc::new),
            })
            .collect()
    }
}

/// Heads: their scripts play an animation of another package, so no sequence of its own, posed only by its actor's VM
/// channels.
fn still_mesh(pkg: &Package, handles: Vec<Handle<Mesh>>, mesh: &Arc<SkeletalMesh>) -> AnimatedMesh {
    let none = Arc::new(MeshAnimation { version: 0, bones: Vec::new(), sequences: Vec::new() });
    AnimatedMesh::new(handles, Arc::clone(mesh), none, 0, |i| pkg.name(mesh.bones[i].name).to_owned(), |_| String::new())
        .with_aliases(|n| pkg.name(n).to_owned())
}

pub fn animate_mesh_with(
    pkg: &Package,
    animations: &[Arc<MeshAnimation>],
    handles: Vec<Handle<Mesh>>,
    mesh: &Arc<SkeletalMesh>,
    prefer: &[&str],
) -> Option<(AnimatedMesh, String)> {
    let bone_names: HashSet<String> = mesh.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
    let animation = animations
        .iter()
        .max_by_key(|a| a.bones.iter().filter(|b| bone_names.contains(&pkg.name(b.name).to_ascii_lowercase())).count())?;
    let named = |part: &str| animation.sequences.iter().position(|s| pkg.name(s.name).to_ascii_lowercase().contains(part));
    let sequence = prefer.iter().find_map(|part| named(part)).unwrap_or(0);
    let name = animation.sequences.get(sequence).map(|s| pkg.name(s.name).to_owned())?;
    let item = AnimatedMesh::new(
        handles,
        Arc::clone(mesh),
        Arc::clone(animation),
        sequence,
        |i| pkg.name(mesh.bones[i].name).to_owned(),
        |i| pkg.name(animation.bones[i].name).to_owned(),
    )
    .with_aliases(|n| pkg.name(n).to_owned());
    Some((item, name))
}

pub fn skeletal_sections<S: SceneAssets>(
    game: &mut Game,
    t: &mut S,
    texture: &mut impl FnMut(&mut Game, &mut S, PackageId, i32) -> Option<MaterialImages>,
    path: &str,
    skins: &[Option<String>],
) -> Option<(SkeletalMesh, PackageId, Vec<SectionView>, Vec<Handle<Mesh>>)> {
    let (mesh_id, mesh_export) = game.export_by_path_of_class(path, "SkeletalMesh")?;
    let mesh = {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        (pkg.object_name(export.class_index) == Some("SkeletalMesh")).then(|| read_skeletal_mesh(pkg, file, export).ok()).flatten()?
    };
    let (sections, handles) = skeletal_mesh_sections(game, t, texture, &mesh, mesh_id, skins, true);
    Some((mesh, mesh_id, sections, handles))
}

const MAX_JOINTS: usize = 256;

pub fn skeletal_mesh_sections<S: SceneAssets>(
    game: &mut Game,
    t: &mut S,
    texture: &mut impl FnMut(&mut Game, &mut S, PackageId, i32) -> Option<MaterialImages>,
    mesh: &SkeletalMesh,
    mesh_id: PackageId,
    skins: &[Option<String>],
    gpu_skin: bool,
) -> (Vec<SectionView>, Vec<Handle<Mesh>>) {
    let mut handles = Vec::new();
    let gpu_skin = (gpu_skin && mesh.bones.len() <= MAX_JOINTS).then(|| Skin {
        inverse_bindposes: t.add_bindposes(SkinnedMeshInverseBindposes::from(skin_inverse_bindposes(mesh))),
        bind: skin_bind_transforms(mesh).into(),
    });
    let sections = skeletal_mesh_to_meshes(mesh, gpu_skin.is_some())
        .into_iter()
        .map(|(section, material)| {
            let handle = t.add_mesh(section);
            handles.push(handle.clone());
            let skin = skins.get(usize::from(material)).cloned().flatten();
            let found = match skin.and_then(|skin| game.export_by_path(&skin)) {
                Some((package, export)) => texture(game, t, package, export_reference(export)),
                None => mesh
                    .materials
                    .get(usize::from(material))
                    .or(mesh.materials.first())
                    .and_then(|&material| texture(game, t, mesh_id, material)),
            };
            SectionView { no_scene_lights: true, skin: gpu_skin.clone(), ..SectionView::textured(handle, found) }
        })
        .collect();
    (sections, handles)
}

pub(super) fn projector_decal(game: &mut Game, t: &mut impl SceneAssets, projector: &ProjectorActor) -> Option<(Handle<Image>, Transform, u32)> {
    let (image, size, final_blend) = projector_image(game, t, &projector.texture)?;
    Some((image, projector_transform(projector, size), projector_tag(projector, final_blend)))
}

const PROJECTOR_TAG: u32 = 1 << 31;

const ALPHA_TEST_TAG: u32 = 1 << 21;
const TAGGED_TAG: u32 = 1 << 30;

const FB_MODULATE: u8 = 1;
const FB_ALPHA_BLEND: u8 = 2;
const FB_TRANSLUCENT: u8 = 4;

/// [`PROJECTOR_TAG`], targets in bits 16..21, `MaterialBlendingOp` in bits 8..16, frame buffer blend
/// in bits 0..8, and the alpha test. With `MaterialBlendingOp` none the material's `FinalBlend` wins.
pub(super) fn projector_tag(projector: &ProjectorActor, final_blend: Option<FinalBlend>) -> u32 {
    let (blending, alpha_ref) = match final_blend {
        Some(inner) if projector.material_blending == ProjectorBlending::None => (inner.blending.byte(), inner.alpha_ref),
        _ => (match projector.blending { ProjectorBlending::AlphaBlend => FB_ALPHA_BLEND, ProjectorBlending::Add => FB_TRANSLUCENT, _ => FB_MODULATE }, None),
    };
    let alpha_test = alpha_ref.map_or(0, |alpha_ref| ALPHA_TEST_TAG | u32::from(alpha_ref) << 22);
    let tagged = if projector.tagged { TAGGED_TAG } else { 0 };
    PROJECTOR_TAG | tagged | alpha_test | u32::from(projector.targets.0) << 16 | u32::from(projector.material_blending as u8) << 8 | u32::from(blending)
}

pub(super) fn projector_image(game: &mut Game, t: &mut impl SceneAssets, texture: &str) -> Option<(Handle<Image>, Vec2, Option<FinalBlend>)> {
    let (package, export) = game.export_by_path(texture)?;
    let texture = game.material_texture(package, export_reference(export))?;
    let size = texture.image.size().as_vec2();
    Some((t.add_image(texture.image), size, texture.state.final_blend))
}

/// The box holds retail's frustum. Orthographic: the near square (side ½·√2·√(w² + h²)) and
/// `MaxTraceDistance`. Perspective: the far end of a frustum with apex ½·w / tan(½·FOV) behind it.
pub(super) fn projector_transform(projector: &ProjectorActor, texels: Vec2) -> Transform {
    let frustum = p2_vm::projector::frustum(&p2_vm::projector::ProjectorShape {
        location: projector.location,
        rotation: projector.rotation,
        texels: texels.to_array(),
        draw_scale: projector.draw_scale,
        fov: projector.fov,
        max_trace_distance: projector.max_trace as i32,
        rotating: false,
    });
    let [x, y, z] = projector.rotation.axes();
    let offsets = frustum.vertices.map(|v| v - projector.location);
    let side = offsets.iter().map(|o| o.dot(y).abs().max(o.dot(z).abs())).fold(0.0, f32::max) * 2.0;
    let depth = offsets.iter().map(|o| o.dot(x)).fold(0.0, f32::max);
    projector_box(projector.location, projector.rotation, side, depth)
}

const DECAL_BACK_REACH: f32 = 2.0;

/// In Bevy space. Splats are spawned at the trace's hit: the surface lies in the plane of the projector. A box starting
/// there would have it on its face, where Bevy's inside test (`clustered_decal_iterator_next`, |p| ≤ 0.5) is decided by
/// float rounding pixel by pixel: grainy splats that come and go as the camera turns.
pub fn projector_box(location: Vector, rotation: Rotator, side: f32, trace: f32) -> Transform {
    let axes = rotator_matrix(rotation);
    let (x, y, z) = (axes.x_axis.truncate(), axes.y_axis.truncate(), axes.z_axis.truncate());
    let depth = trace + DECAL_BACK_REACH;
    let center = Vec3::from_array(location.into()) + x * (trace - DECAL_BACK_REACH) / 2.0;
    // The decal's local axes are Bevy's (it projects along local −Z), so the Unreal → Bevy axis swap applies on the
    // world side only. Local X along the texture's U (Y), local Y along Z, local −Z along the projection (X); this
    // basis is mirrored in Unreal terms, which the swap undoes.
    let matrix = Mat4::from_cols((y * side).extend(0.0), (z * side).extend(0.0), (-x * depth).extend(0.0), center.extend(1.0));
    let swap = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);
    Transform::from_matrix(swap * matrix)
}

pub(super) fn scene_preparer(game: &Game, id: PackageId, package: &str, t: &SceneTargets) -> impl FnOnce(StartedVm) -> Result<PreparedScene, LoadError> + Send + 'static {
    let (game, package, assets) = (game.clone(), package.to_owned(), PendingAssets::new(t));
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
        assets.insert(t);
        for model in emitter_models.values() {
            spawned::keep_static_meshes(t, model.as_ref());
        }
        self.spawn_models.extend(emitter_models);
        self.hud_assets = hud;
        let (game, _) = self.library.open(&self.root, &package)?;
        game.adopt(&built);
        self.switch_images.clear();
        for ((id, export), drawn) in switches {
            if let Some(id) = game.same_package(&built, id) {
                self.switch_images.entry((id, export)).or_default().push(drawn);
            }
        }
        self.animations = animations.into_iter().filter_map(|(id, animation)| Some((game.same_package(&built, id)?, animation))).collect();
        self.vm_animations = vm_animations;
        self.actor_textures = actor_textures;
        self.map_system = Some(Library::system_of(&self.root, &package));
        self.map = Some(package);
        t.animated.0.extend(animated);
        let center = scene.center;
        let (vm, loader, player, zone_actors) = played;
        t.play.set(Some(PlayWorld::new(vm, loader, center, player, zone_actors, self.root.join("Music"))));
        self.add_particles(emitters, -center, t);
        self.add_sprites(sprites, -center, t);
        self.add_coronas(coronas, -center, t);
        self.add_fluids(fluids, -center, t);
        t.view.show(scene);
        Ok(status)
    }

    pub(super) fn update_material_switches(&mut self, t: &mut SceneTargets) {
        let Some(world) = t.play.world_mut() else { return };
        let changes = world.take_material_switches();
        if changes.is_empty() || self.switch_images.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        let (mut switched, mut looks) = (HashSet::new(), HashMap::new());
        for (switch, material) in changes {
            let Some(key) = game.export_by_path(&switch) else { continue };
            let Some(surfaces) = self.switch_images.get_mut(&key) else { continue };
            let Some((package, export)) = material.and_then(|path| game.export_by_path(&path)) else { continue };
            let Some((source, state)) = game.material_source(package, export_reference(export)) else { continue };
            let Some(mut image) = game.source_image(source, TextureDecode::Keep) else { continue };
            // Uploaded as a new texture with all its mips: Bevy rewrites a texture of the same descriptor with
            // `COPY_DST` in place, mip 0 only, so the smaller mips kept the old picture past some distance from the
            // camera. Without `COPY_DST` the descriptor differs; wgpu adds it for the upload itself.
            image.texture_descriptor.usage.remove(bevy::render::render_resource::TextureUsages::COPY_DST);
            let image_size = |image: &Image| image.size().as_vec2().to_array();
            let (size, specular) = (image_size(&image), game.specular_layer(source));
            let specular_size = specular.as_ref().map_or([1.0; 2], |(specular, _)| image_size(specular));
            let now = MaterialImages {
                image: Handle::default(),
                glow: game.glow_image(source).map(|glow| t.assets.images.add(glow)),
                detail: game.detail_image(source).map(|detail| t.assets.images.add(detail)),
                specular: specular.map(|(specular, params)| (t.assets.images.add(specular), params)),
                state,
            };
            for surface in surfaces {
                let Some(mut drawn) = t.assets.images.get_mut(surface.image) else { continue };
                *drawn = image.clone();
                switched.insert(surface.image);
                if surface.whole {
                    let was = MaterialImages { state: std::mem::replace(&mut surface.shown, now.state.clone()), ..now.clone() };
                    let (was, now) = (SectionView::textured(Handle::default(), Some(was)), SectionView::textured(Handle::default(), Some(now.clone())));
                    looks.insert(surface.image, crate::mesh_view::SwitchedLook { was, now, size, specular_size });
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
        let mut texture = |game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32| {
            cache
                .entry((package, material))
                .or_insert_with(|| {
                    let (source, state) = game.material_source(package, material)?;
                    let image = images.entry(source).or_insert_with(|| {
                        let handle = t.add_image(game.source_image(source, TextureDecode::Keep)?);
                        let glow = game.glow_image(source).map(|glow| t.add_image(glow));
                        let detail = game.detail_image(source).map(|detail| t.add_image(detail));
                        let specular = game.specular_layer(source).map(|(cube, params)| (t.add_image(cube), params));
                        if source.opacity.is_none()
                            && let Some(wet) = game.wet_source(source) {
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
        } = read_map_actors(started, &model, sky_zone);
        let emitter_textures = emitters.iter().flat_map(|(_, emitter)| &emitter.emitters).filter(|(p, _)| p.kind != p2_particles::EmitterKind::Mesh).filter_map(|(_, texture)| texture.as_deref());
        let mut actor_textures = HashMap::new();
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
        let level = StaticLevel { pkg, file, model: &model, environment: &environment };
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
        let sky = sky_info.map(|(location, rotation)| Sky {
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
            collision: Some(StaticCollision { model, terrains: static_terrains, meshes: collisions }),
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

fn switched_images(
    game: &mut Game,
    images: &HashMap<TextureSource, Option<MaterialImages>>,
    cache: &HashMap<(PackageId, i32), Option<MaterialImages>>,
) -> Vec<((PackageId, usize), SwitchImage)> {
    let mut switched = Vec::new();
    for (source, found) in images.iter().filter(|(source, _)| source.wet.is_none()) {
        let (Some(switch), Some(found)) = (source.switch, found) else { continue };
        let image = found.image.id();
        let users: Vec<_> = cache
            .iter()
            .filter_map(|(&(package, material), used)| Some((package, material, used.as_ref().filter(|used| used.image.id() == image)?)))
            .collect();
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


struct TerrainSurface<'a> {
    terrain: &'a p2_assets::Terrain,
    normals: &'a [[f32; 3]],
    alpha_uvs: &'a [[f32; 2]],
    lighting: Option<&'a [[u8; 4]]>,
}

struct DrawnLayer {
    index: usize,
    image: Handle<Image>,
    uvs: Vec<[f32; 2]>,
    alpha_map: Handle<Image>,
    alpha: Vec<f32>,
    glow: Option<Handle<Image>>,
    shader_layers: crate::render::shader_layers::Layers,
    matrix: [[f32; 4]; 4],
}

struct DecorationLayer {
    mesh: i32,
    placements: Vec<p2_assets::Decoration>,
    in_sky: bool,
    zone: u8,
}

/// The engine uses layers while both their texture and alpha map are set: the first one missing either
/// ends the list, layer 0 included.
fn terrain_layers(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    surface: &TerrainSurface,
) -> Vec<DrawnLayer> {
    let mut layers = Vec::new();
    for (index, layer) in surface.terrain.layers.iter().enumerate() {
        if layer.texture == 0 || layer.alpha_map == 0 {
            break;
        }
        let map = game.texture(id, layer.alpha_map);
        let Some((width, height, alpha)) = map.and_then(|(texture, palette)| texture_alpha(&texture, palette.as_deref())) else {
            continue;
        };
        let Some(MaterialImages { image, glow, detail, specular, state }) = texture(game, t, id, layer.texture) else { continue };
        let premultiplied = state.opacity.is_some() && (state.glow.is_some() || state.specular.is_some());
        let scale = state.detail.map_or(0.0, |detail| detail.scale);
        let shader_layers = crate::render::shader_layers::Layers::new(detail, scale, specular, state.specular_uv, premultiplied);
        let (w, h) = (width as usize, height as usize);
        let per_vertex = surface
            .alpha_uvs
            .iter()
            .map(|&[u, v]| {
                let x = ((u * w as f32) as usize).min(w - 1);
                let y = ((v * h as f32) as usize).min(h - 1);
                f32::from(alpha[y * w + x]) / 255.0
            })
            .collect::<Vec<_>>();
        let alpha_map = t.add_image(alpha_map_image(width, height, &alpha));
        let uvs = surface.terrain.layer_uvs(layer);
        layers.push(DrawnLayer { index, image, uvs, alpha_map, alpha: per_vertex, glow, shader_layers, matrix: layer.matrix });
    }
    layers
}

fn sector_sections(
    t: &mut PendingAssets,
    surface: &TerrainSurface,
    triangles: &[[usize; 3]],
    drawn: &[&DrawnLayer],
    in_sky: bool,
) -> (Vec<SectionView>, Vec<Handle<Mesh>>) {
    let TerrainSurface { terrain, normals, alpha_uvs, lighting } = *surface;
    let shader_layers = drawn.iter().any(|layer| {
        let shader = &layer.shader_layers;
        layer.glow.is_some() || shader.detail.is_some() || shader.specular.is_some() || shader.flat_specular.is_some()
    });
    let mut handles = Vec::new();
    let sector_vertices = p2_bevy_assets::SectorVertices::new(triangles, terrain.vertices.len());
    let sections: Vec<SectionView> = if crate::render::terrain_material::one_pass(drawn.len(), shader_layers) {
        let mut mesh = terrain_layer_mesh(&terrain.vertices, normals, &sector_vertices, &[], alpha_uvs, lighting, None);
        if !in_sky {
            keep_for_relighting(&mut mesh);
        }
        let chunks = crate::render::world_pass::chunks_of(&mut mesh);
        let mesh = t.add_mesh(mesh);
        handles.push(mesh.clone());
        let terrain_layers = drawn
            .iter()
            .map(|layer| crate::render::terrain_material::TerrainPassLayer {
                texture: layer.image.clone(),
                alpha_map: layer.alpha_map.clone(),
                matrix: layer.matrix,
            })
            .collect();
        vec![SectionView {
            mesh,
            texture: Some(drawn[0].image.clone()),
            alpha_mode: AlphaMode::Opaque,
            no_scene_lights: true,
            fullbright: false,
            depth_bias: 0.0,
            glow: None,
            layers: Default::default(),
            lightmap: None,
            two_sided: false,
            uv: Vec::new(),
            skin: None,
            tint: [1.0; 4],
            alpha_map: None,
            terrain: terrain_layers,
            chunks,
        }]
    } else {
        let mut first = true;
        drawn
            .iter()
            .map(|layer| {
                let opaque = std::mem::take(&mut first);
                let mut mesh =
                    terrain_layer_mesh(&terrain.vertices, normals, &sector_vertices, &layer.uvs, alpha_uvs, lighting, Some(layer.alpha.as_slice()));
                if !in_sky {
                    keep_for_relighting(&mut mesh);
                }
                let chunks = if opaque { crate::render::world_pass::chunks_of(&mut mesh) } else { None };
                let mesh = t.add_mesh(mesh);
                handles.push(mesh.clone());
                SectionView {
                    mesh,
                    texture: Some(layer.image.clone()),
                    alpha_mode: if opaque { AlphaMode::Opaque } else { AlphaMode::Blend },
                    no_scene_lights: true,
                    fullbright: false,
                    depth_bias: if opaque { 0.0 } else { crate::render::blended::TERRAIN_LAYER_SORT + layer.index as f32 },
                    glow: layer.glow.clone(),
                    layers: layer.shader_layers.clone(),
                    lightmap: None,
                    two_sided: false,
                    uv: Vec::new(),
                    skin: None,
                    tint: [1.0; 4],
                    alpha_map: Some(layer.alpha_map.clone()),
                    terrain: Vec::new(),
                    chunks,
                }
            })
            .collect()
    };
    (sections, handles)
}

/// Each decoration's colour (terrain light × colour map) is drawn on its material with `Modulate2X`:
/// one tinted, unlit copy of the sections per colour, rounded to steps of 8.
fn decoration_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    layer: DecorationLayer,
) -> Option<Vec<ModelView>> {
    let DecorationLayer { mesh: mesh_reference, placements, in_sky, zone } = layer;
    let (mesh_id, mesh_export) = game.export_of(id, mesh_reference)?;
    let (mesh, materials) = {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        (read_static_mesh(pkg, file, export).ok()?, read_static_mesh_materials(pkg, file, export).unwrap_or_default())
    };
    let sections = sections_of_static_mesh(&mesh, t, SectionChunks::Chunked, |t, i| texture(game, t, mesh_id, *materials.get(i)?));
    let radius = mesh.positions.iter().map(|&p| Vector::from(p).size_squared()).fold(0.0_f32, f32::max).sqrt();
    let mut by_color: HashMap<([u8; 3], [i32; 2]), (Vec<Transform>, [f32; 3], [f32; 3])> = HashMap::new();
    for d in &placements {
        let color = [0, 1, 2].map(|i| ((u16::from(d.color[i]) + 4) / 8 * 8).min(255) as u8);
        let (location, scale) = (<[f32; 3]>::from(d.location), <[f32; 3]>::from(d.scale));
        let transform = unreal_transform(location.into(), d.rotation, scale.into(), [0.0; 3].into());
        let reach = radius * scale.iter().fold(0.0_f32, |most, s| most.max(s.abs()));
        let (low, high) = (location.map(|c| c - reach), location.map(|c| c + reach));
        let (instances, min, max) = by_color.entry((color, cell(transform.translation))).or_insert((Vec::new(), low, high));
        instances.push(transform);
        (*min, *max) = merged_box((*min, *max), (low, high));
    }
    let models = by_color
        .into_iter()
        .map(|((color, _), (instances, min, max))| {
            let tint = |s: &SectionView| std::array::from_fn(|i| s.tint[i] * color.get(i).copied().map_or(1.0, modulate_2x));
            let sections = sections.iter().map(|s| SectionView { no_scene_lights: true, tint: tint(s), ..s.clone() }).collect();
            let zone = (!in_sky).then(|| ZoneCulled { zones: vec![zone], min, max, only_from: None });
            ModelView { sections, instances, zone }
        })
        .collect();
    Some(models)
}

struct MeshPlacing<I> {
    transform: Transform,
    instance: Option<(I, MeshLighting)>,
    placement: Option<Placement>,
    follow: Option<FollowActor>,
    in_sky: bool,
    only_from: Option<Vec<u8>>,
}

struct StaticLevel<'a> {
    pkg: &'a Package,
    file: &'a [u8],
    model: &'a Model,
    environment: &'a LightEnvironment,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    material: String,
    has_uvs: bool,
    has_colors: bool,
}

#[derive(Default)]
struct StaticParts {
    models: Vec<ModelView>,
    sky_models: Vec<ModelView>,
    followers: Vec<MovingModel<FollowActor>>,
    collisions: Vec<MeshCollision>,
    batches: BTreeMap<BatchKey, (SectionView, MeshBatch)>,
    lit_actors: usize,
}

fn static_mesh_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    level: &StaticLevel,
    ((mesh_package, reference), skins): ((PackageId, i32), Vec<(PackageId, i32)>),
    placements: Vec<MeshPlacing<Option<i32>>>,
    parts: &mut StaticParts,
) -> bool {
    let mesh = game.export_of(mesh_package, reference).and_then(|(mesh_id, mesh_export)| {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        let mesh = read_static_mesh(pkg, file, export).ok()?;
        Some((mesh_id, mesh, read_static_mesh_materials(pkg, file, export).unwrap_or_default()))
    });
    let Some((mesh_id, mesh, materials)) = mesh else { return false };
    let mut base = static_mesh_to_meshes(&mesh);
    let textures: Vec<_> = (0..base.len())
        .map(|i| match skins.get(i).filter(|(_, skin)| *skin != 0) {
            Some(&(skin_package, skin)) => texture(game, t, skin_package, skin),
            None => materials.get(i).and_then(|&material| texture(game, t, mesh_id, material)),
        })
        .collect();
    for (section, found) in base.iter_mut().zip(&textures) {
        use_uv_stream(section, &mesh, found.as_ref());
    }
    let views: Vec<SectionView> =
        textures.iter().map(|found| SectionView { no_scene_lights: true, ..SectionView::textured(Handle::default(), found.clone()) }).collect();
    let blended: Vec<bool> = views.iter().map(|view| !matches!(view.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_))).collect();
    let material_keys: Vec<String> = views.iter().map(SectionView::material_key).collect();
    let sections = |t: &mut PendingAssets, lighting: Option<&[[u8; 4]]>, relit: bool, mirrored: bool, keep: &dyn Fn(usize) -> bool| -> Vec<SectionView> {
        base.iter()
            .zip(&views)
            .enumerate()
            .filter(|&(i, _)| keep(i))
            .map(|(_, (section, view))| {
                let mut section = section.clone();
                if let Some(lighting) = lighting.filter(|_| !view.fullbright) {
                    section.insert_attribute(Mesh::ATTRIBUTE_COLOR, lighting_colors(lighting));
                }
                if relit {
                    keep_for_relighting(&mut section);
                }
                if mirrored {
                    section.invert_winding().ok();
                }
                SectionView { mesh: t.add_mesh(section), ..view.clone() }
            })
            .collect()
    };
    let mut unlit: [Vec<(Transform, Option<ZoneCulled>)>; 2] = Default::default();
    let mut shared: Option<Arc<StaticMesh>> = None;
    for MeshPlacing { transform, instance, placement, follow, in_sky, only_from } in placements {
        let collision = placement.map(|Placement { location, rotation, scale, pre_pivot }| MeshCollision::new(&mesh, location, rotation, scale, pre_pivot));
        let bounds = collision.as_ref().map(MeshCollision::bounds);
        parts.collisions.extend(collision);
        let mirrored = mirrors(&transform);
        let lit = instance.map(|(reference, lighting)| {
            let export = reference.and_then(|reference| level.pkg.exports.get(usize::try_from(reference - 1).ok()?));
            let instance = export.and_then(|export| read_static_mesh_instance(level.pkg, level.file, export).ok());
            let fits = instance.as_ref().is_some_and(|instance| instance_fits(&mesh, instance));
            (level.environment.mesh_colors(level.model, &mesh, instance.as_ref(), &lighting), fits, lighting)
        });
        let (lighting, relit): (Option<_>, Option<_>) = lit.map(|(colors, fits, lighting)| (colors, (fits, lighting))).unzip();
        if let Some(follow) = follow {
            // Without baked lighting the engine lights it by hardware lights where it is: relit as it moves
            // (movers with an instance keep it, as the engine does).
            let relight = relit.filter(|&(fits, _)| !fits).map(|(_, lighting)| {
                let mesh = shared.get_or_insert_with(|| Arc::new(mesh.clone())).clone();
                let shading = Shading::Vertices { mesh, scale: lighting.placement.scale, pre_pivot: lighting.placement.pre_pivot };
                ActorLit::new(follow.actor, lighting.glow, MAX_HARDWARE_LIGHTS, 0.0, shading)
                    .lit_at(lighting.placement.location, lighting.placement.rotation)
            });
            parts.followers.push(MovingModel { sections: sections(t, lighting.as_deref(), relight.is_some(), mirrored, &|_| true), transform, follow, lit: relight });
            continue;
        }
        if in_sky {
            parts.sky_models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone: None });
            continue;
        }
        parts.lit_actors += usize::from(lighting.is_some());
        let zone = bounds
            .filter(|_| !level.model.leaves.is_empty())
            .map(|(min, max)| ZoneCulled { zones: p2_collision::box_zones(level.model, min, max), min: min.into(), max: max.into(), only_from: only_from.clone() });
        if only_from.is_some() {
            parts.models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone });
            continue;
        }
        let colors = lighting.as_deref().map(lighting_colors);
        for (i, section) in base.iter().enumerate().filter(|&(i, _)| !blended[i]) {
            let view = &views[i];
            let colors = colors.as_deref().filter(|_| !view.fullbright);
            let key = BatchKey { material: material_keys[i].clone(), has_uvs: section.attribute(Mesh::ATTRIBUTE_UV_0).is_some(), has_colors: colors.is_some() };
            let (_, batch) = parts.batches.entry(key).or_insert_with(|| (view.clone(), MeshBatch::default()));
            batch.push(section, &transform, colors);
        }
        if blended.contains(&true) {
            match lighting {
                Some(lighting) => parts.models.push(ModelView { sections: sections(t, Some(&lighting), false, mirrored, &|i| blended[i]), instances: vec![transform], zone }),
                None => unlit[usize::from(mirrored)].push((transform, zone)),
            }
        }
    }
    for (mirrored, unlit) in unlit.into_iter().enumerate().filter(|(_, unlit)| !unlit.is_empty()) {
        let sections = sections(t, None, false, mirrored == 1, &|i| blended[i]);
        parts.models.extend(unlit.into_iter().map(|(transform, zone)| ModelView { sections: sections.clone(), instances: vec![transform], zone }));
    }
    true
}


#[derive(Default)]
struct PawnParts {
    followers: Vec<MovingModel<FollowActor>>,
    attached: Vec<MovingModel<FollowBone>>,
    animated: Vec<AnimatedMesh>,
    drawn_pawns: usize,
    drawn_attachments: usize,
}

fn pawn_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    animations: &mut HashMap<PackageId, Arc<[Arc<MeshAnimation>]>>,
    vm_animations: &VmAnimations,
    pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>>,
    attachments: &HashMap<ObjectId, Vec<SkeletalActor>>,
) -> PawnParts {
    let mut parts = PawnParts::default();
    let mut attached_instances: HashMap<(String, Vec<Option<String>>), Vec<(FollowBone, Mat4, ActorLighting)>> = HashMap::new();
    for ((mesh_path, skins), actors) in pawns {
        let Some((mesh, mesh_id, sections, handles)) = skeletal_sections(game, t, texture, &mesh_path, &skins)
        else {
            continue;
        };
        let mesh = Arc::new(mesh);
        let animated = {
            let (pkg, data) = game.data(mesh_id);
                let animations = animations.entry(mesh_id).or_insert_with(|| vm_animations.package(game.package_name(mesh_id), pkg, data));
                animate_mesh_with(pkg, animations, handles.clone(), &mesh, &["idle", "stand"])
        };
        let pose = match &animated {
            Some((item, _)) => item.bone_pose(0.0),
            None => {
                let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
                let local: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
                bone_matrices(&parents, &local)
            }
        };
        let attached = actors.iter().any(|a| attachments.contains_key(&a.object));
        let bone_names: Vec<String> = if attached {
            let (pkg, _) = game.data(mesh_id);
            mesh.bones.iter().map(|b| pkg.name(b.name).to_owned()).collect()
        } else {
            Vec::new()
        };
        for a in &actors {
            let own_sections = sections.clone();
            if let Some((item, _)) = &animated {
                let mut item = item.clone();
                item.actor = Some(a.object);
                parts.animated.push(item);
            }
            let body = unreal_mesh_matrix(
                a.location,
                rotator_matrix(a.rotation),
                a.scale,
                a.pre_pivot,
                mesh.scale,
                mesh.origin,
                mesh.rot_origin,
            );
            let follow = FollowActor {
                actor: a.object,
                scale: a.scale,
                pre_pivot: a.pre_pivot,
                skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)),
            };
            // Lit by the lights where it walks (one colour per pawn: the meshes are shared).
            let lit = ActorLit::skeletal(a.object, &mesh, a.scale, &a.lighting);
            parts.followers.push(MovingModel { sections: own_sections, transform: unreal_to_bevy(body), follow, lit });
            for attachment in attachments.get(&a.object).into_iter().flatten() {
                let Some((_, bone)) = &attachment.attached_to else { continue };
                let Some(index) = bone_names.iter().position(|n| n.eq_ignore_ascii_case(bone)) else { continue };
                let Some(&bone) = pose.get(index) else { continue };
                let follow = FollowBone {
                    actor: attachment.object,
                    base: a.object,
                    bone: index,
                    body,
                    base_follow: follow,
                    relative_location: attachment.relative_location,
                    relative_rotation: attachment.relative_rotation,
                    scale: attachment.scale,
                    pre_pivot: attachment.pre_pivot,
                    mesh_scale: [1.0; 3].into(),
                    origin: [0.0; 3].into(),
                    rot_origin: [0; 3].into(),
                };
                attached_instances.entry((attachment.mesh.clone(), attachment.skins.clone())).or_default().push((follow, bone, attachment.lighting));
            }
        }
        parts.drawn_pawns += actors.len();
    }
    for ((mesh_path, skins), followers) in attached_instances {
        let Some((mesh, mesh_id, sections, handles)) = skeletal_sections(game, t, texture, &mesh_path, &skins) else { continue };
        let mesh = Arc::new(mesh);
        let animated = {
            let (pkg, data) = game.data(mesh_id);
            let animations = animations.entry(mesh_id).or_insert_with(|| vm_animations.package(game.package_name(mesh_id), pkg, data));
            animate_mesh_with(pkg, animations, handles.clone(), &mesh, &["idle"]).map(|(item, _)| item).unwrap_or_else(|| still_mesh(pkg, handles.clone(), &mesh))
        };
        for (follow, bone, lighting) in followers {
            let mut item = animated.clone();
            item.actor = Some(follow.actor);
            parts.animated.push(item);
            let follow = FollowBone { mesh_scale: mesh.scale, origin: mesh.origin, rot_origin: mesh.rot_origin, ..follow };
            let lit = ActorLit::skeletal(follow.base, &mesh, follow.scale, &lighting);
            parts.attached.push(MovingModel { sections: sections.clone(), transform: follow.transform(follow.body, bone), follow, lit });
            parts.drawn_attachments += 1;
        }
    }
    parts
}


struct MapActors {
    terrain_infos: Vec<p2_assets::Terrain>,
    start: Option<(Vector, Rotator)>,
    mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>>,
    zone_fogs: Vec<Option<crate::actors::ZoneFog>>,
    terrain_zones: Vec<bool>,
    mesh_lights: HashMap<i32, p2_render_cpu::MeshLight>,
    zone_ambients: Vec<[f32; 3]>,
    level_brightness: f32,
    attachments: HashMap<ObjectId, Vec<SkeletalActor>>,
    script_errors: usize,
    played: (Vm<Arc<Loader>>, Arc<Loader>, Option<(ObjectId, Option<ObjectId>)>, Vec<Option<ObjectId>>),
    pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>>,
    projectors: Vec<ProjectorActor>,
    antiportal_paths: Vec<(String, MeshPlacement)>,
    emitters: Vec<(ObjectId, EmitterActor)>,
    sprites: Vec<SpriteActor>,
    coronas: Vec<CoronaActor>,
    fluids: Vec<fluids::FluidActor>,
}

fn read_map_actors(started: StartedVm, model: &Model, sky_zone: Option<u8>) -> MapActors {
    let mut terrain_infos = Vec::new();
    let mut start = None;
    let mut mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>> = HashMap::new();
    let mut zone_fogs = Vec::new();
    let mut terrain_zones = Vec::new();
    let mut mesh_lights = HashMap::new();
    let mut zone_ambients = Vec::new();
    let mut level_info = None;
    let mut level_brightness = 1.0;
    let mut attachments: HashMap<ObjectId, Vec<SkeletalActor>> = HashMap::new();
    let mut pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>> = HashMap::new();
    let mut projectors = Vec::new();
    let mut antiportal_paths = Vec::new();
    let mut emitters = Vec::new();
    let mut sprites = Vec::new();
    let mut coronas = Vec::new();
    let mut fluids = Vec::new();
    let StartedVm { mut vm, loader, id, script_errors, player } = started;
    let LoadedPackage { package: pkg, data, .. } = loader.package(id);
    for (index, export) in pkg.exports.iter().enumerate() {
        let Ok(object) = vm.load_object(id, index) else { continue };
        if let Some(light) = mesh_light(&vm, object) {
            mesh_lights.insert(export_reference(index), light);
        }
        match pkg.object_name(export.class_index) {
            Some("LevelInfo") => level_info = Some(object),
            Some("TerrainInfo") => {
                let Some(class) = vm.object(object).class else { continue };
                terrain_infos.extend(read_terrain(pkg, data, export, &vm.class(class).layout).ok());
            }
            Some("PlayerStart") if start.is_none() => start = Some(location_rotation(&vm, object)),
            _ => {}
        }
    }
    let zone_actors: Vec<Option<ObjectId>> = model.zones.iter().map(|zone| usize::try_from(zone.actor - 1).ok().and_then(|export| vm.load_object(id, export).ok())).collect();
    let zone_tags: Vec<_> = zone_actors
        .iter()
        .map(|&object| {
            match object.or(level_info).and_then(|object| vm.get(object, "Tag")) {
                Some(p2_vm::Value::Name(tag)) => Some(tag),
                _ => None,
            }
        })
        .collect();
    let project_tags: Vec<_> = vm
        .actors()
        .filter_map(|o| match vm.get(o, "ProjectTag") {
            Some(p2_vm::Value::Name(tag)) if tag != p2_vm::NameId::NONE => Some(tag),
            _ => None,
        })
        .collect();
    for object in vm.actors().filter(|&o| !vm.is_deleted(o)) {
        projectors.extend(projector_actor(&vm, object));
        antiportal_paths.extend(antiportal_actor(&vm, object));
        emitters.extend(emitter_actor(&vm, object).map(|emitter| (object, emitter)));
        sprites.extend(sprite_actor(&vm, object));
        coronas.extend(corona_actor(&vm, object));
        let seed = fluids.len() as u32 + 1;
        fluids.extend(fluids::fluid_actor(&vm, object, seed));
        if let Some(actor) = skeletal_actor(&vm, object) {
            if let Some((base, _)) = actor.attached_to {
                attachments.entry(base).or_default().push(actor);
                continue;
            }
            pawns.entry((actor.mesh.clone(), actor.skins.clone())).or_default().push(actor);
            continue;
        }
        let Some(actor) = mesh_actor(&vm, object) else { continue };
        let transform = unreal_transform(actor.location, actor.rotation, actor.scale, actor.pre_pivot);
        let zone = p2_collision::point_zone(model, actor.location);
        let lighting = MeshLighting {
            placement: MeshPlacement {
                location: actor.location,
                rotation: actor.rotation,
                scale: actor.scale,
                pre_pivot: actor.pre_pivot,
            },
            zone,
            glow: actor.lighting.glow,
        };
        let instance = (!actor.lighting.unlit).then_some((actor.instance, lighting));
        // Movers are not level collision: retail particles trace only the level and its static geometry.
        let placement = (actor.blocks && !actor.movable).then_some(Placement { location: actor.location, rotation: actor.rotation, scale: actor.scale, pre_pivot: actor.pre_pivot });
        let in_sky = sky_zone == Some(zone);
        // Merged sections take every untagged projector and no tagged one: a model of its own carries the actor's own answer
        // (`Session::update_projections`).
        let aimed_at = matches!(vm.get(actor.object, "Tag"), Some(p2_vm::Value::Name(tag)) if project_tags.contains(&tag));
        let own_model = (!actor.accepts_projectors || aimed_at) && !in_sky && actor.forced_zone_tag.is_none();
        let follow =
            (actor.movable || own_model).then_some(FollowActor { actor: actor.object, scale: actor.scale, pre_pivot: actor.pre_pivot, skeletal: None });
        let only_from = actor.forced_zone_tag.as_ref().map(|tag| {
            (0..=u8::MAX).filter(|&zone| zone_tags.get(usize::from(zone)).is_some_and(|zone_tag| zone_tag.as_ref() == Some(tag))).collect::<Vec<_>>()
        });
        mesh_paths.entry((actor.static_mesh, actor.skins)).or_default().push(MeshPlacing { transform, instance, placement, follow, in_sky, only_from });
    }
    for &object in &zone_actors {
        zone_fogs.push(object.and_then(|object| zone_fog(&vm, object)));
        terrain_zones.push(object.or(level_info).is_some_and(|object| vm.get(object, "bTerrainZone") == Some(p2_vm::Value::Bool(true))));
        zone_ambients.push(object.or(level_info).map_or([0.0; 3], |object| zone_ambient(&vm, object)));
    }
    if let Some(p2_vm::Value::Float(brightness)) = level_info.and_then(|info| vm.get(info, "Brightness")) {
        level_brightness = brightness;
    }
    let played = (vm, Arc::clone(&loader), player, zone_actors);
    MapActors { terrain_infos, start, mesh_paths, zone_fogs, terrain_zones, mesh_lights, zone_ambients, level_brightness, attachments, script_errors, played, pawns, projectors, antiportal_paths, emitters, sprites, coronas, fluids }
}

fn resolve_mesh_paths(
    game: &mut Game,
    id: PackageId,
    mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>>,
    placements: &mut HashMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
) -> usize {
    let mut missing = 0;
    for ((mesh, skins), actors) in mesh_paths {
        let mesh = game.export_by_path_of_class(&mesh, "StaticMesh").map(|(package, e)| (package, export_reference(e)));
        let mut export = |path: &str| game.export_by_path(path).map(|(package, e)| (package, export_reference(e)));
        let Some(mesh) = mesh else {
            missing += 1;
            continue;
        };
        let skins = skins.iter().map(|skin| skin.as_deref().and_then(&mut export).unwrap_or((id, 0))).collect();
        let actors = actors
            .into_iter()
            .map(|MeshPlacing { transform, instance, placement, follow, in_sky, only_from }| {
                let instance = instance.map(|(path, lighting)| {
                    let reference = path.and_then(|path| export(&path)).filter(|&(p, _)| p == id).map(|(_, reference)| reference);
                    (reference, lighting)
                });
                MeshPlacing { transform, instance, placement, follow, in_sky, only_from }
            })
            .collect::<Vec<_>>();
        placements.entry((mesh, skins)).or_default().extend(actors);
    }
    missing
}


struct TerrainZones<'a> {
    model: &'a Model,
    sky_zone: Option<u8>,
    terrain_zones: &'a [bool],
}

struct TerrainParts {
    models: Vec<ModelView>,
    sky_models: Vec<ModelView>,
    static_terrains: Vec<p2_assets::Terrain>,
    terrain_sectors: Vec<crate::render::terrain_light::TerrainSector>,
    terrains: usize,
    hidden_terrains: usize,
    drawn_decorations: usize,
}

fn terrain_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    zones: &TerrainZones,
    terrain_infos: Vec<p2_assets::Terrain>,
) -> TerrainParts {
    let (mut models, mut sky_models) = (Vec::new(), Vec::new());
    let mut terrains = 0;
    let mut hidden_terrains = 0;
    let mut static_terrains = Vec::new();
    let mut decorations = Vec::new();
    let mut terrain_sectors = Vec::new();
    for mut terrain in terrain_infos {
        let heights =
            game.texture(id, terrain.heightmap).and_then(|(texture, _)| texture_heights(&texture));
        if let Some((width, height, heights)) = heights
            && (width as usize, height as usize) == (terrain.width, terrain.height)
        {
            terrain.vertices = terrain.vertices_from_heights(&heights);
        }
        if terrain.vertices.is_empty() {
            continue;
        }
        terrains += 1;
        // A terrain belongs to the zone its `TerrainInfo` stands in (`ZoneInfo.Terrains`): in the sky zone it is part
        // of the sky (some `SkyZoneInfo`s are `bTerrainZone`).
        let zone = p2_collision::point_zone(zones.model, terrain.location);
        let in_sky = zones.sky_zone == Some(zone);
        // Retail draws a terrain and its decorations only from a `bTerrainZone` zone.
        if !in_sky && !zones.terrain_zones.get(usize::from(zone)).copied().unwrap_or(true) {
            hidden_terrains += 1;
            static_terrains.push(terrain);
            continue;
        }
        for layer in terrain.deco_layers.iter().filter(|layer| layer.show_on_terrain) {
            let mut map = |reference: i32| (reference != 0).then(|| game.texture(id, reference)).flatten();
            let density = map(layer.density_map).and_then(|(texture, palette)| texture_alpha(&texture, palette.as_deref()));
            let scale = map(layer.scale_map).and_then(|(texture, _)| texture_bgra(&texture));
            let color = map(layer.color_map).and_then(|(texture, _)| texture_bgra(&texture));
            let maps = DecoMaps {
                density: density.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
                scale: scale.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
                color: color.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
            };
            let placements = terrain.decorations(layer, &terrain.vertices, &maps);
            decorations.push(DecorationLayer { mesh: layer.static_mesh, placements, in_sky, zone });
        }
        let normals = terrain.vertex_normals();
        let alpha_uvs: Vec<[f32; 2]> = (0..terrain.vertices.len()).map(|i| terrain.alpha_uv(i)).collect();
        let surface = TerrainSurface {
            terrain: &terrain,
            normals: &normals,
            alpha_uvs: &alpha_uvs,
            lighting: (!terrain.vertex_colors.is_empty()).then_some(terrain.vertex_colors.as_slice()),
        };
        let layers = terrain_layers(game, t, texture, id, &surface);
        let size = terrain.sector_size.max(1);
        let (quads_x, quads_y) = (terrain.width.saturating_sub(1), terrain.height.saturating_sub(1));
        for y0 in (0..quads_y).step_by(size) {
            for x0 in (0..quads_x).step_by(size) {
                let terrain = &terrain;
                let triangles: Vec<[usize; 3]> = (y0..(y0 + size).min(quads_y))
                    .flat_map(|y| (x0..(x0 + size).min(quads_x)).flat_map(move |x| terrain.quad_triangles(x, y)))
                    .collect();
                let Some((min, max)) = bounds(triangles.iter().flatten().map(|&i| terrain.vertices[i])) else { continue };
                let drawn: Vec<&DrawnLayer> = layers.iter().filter(|layer| triangles.iter().flatten().any(|&i| layer.alpha[i] > 0.0)).collect();
                let (sections, handles) = sector_sections(t, &surface, &triangles, &drawn, in_sky);
                if !in_sky && !handles.is_empty() {
                    let order = p2_bevy_assets::terrain_sector_vertices(&triangles, terrain.vertices.len());
                    terrain_sectors.push(crate::render::terrain_light::TerrainSector {
                        meshes: handles,
                        positions: order.iter().map(|&i| terrain.vertices[i]).collect(),
                        normals: order.iter().map(|&i| normals[i]).collect(),
                        baked: order.iter().map(|&i| p2_bevy_assets::terrain_vertex_color(surface.lighting, i)).collect(),
                        min: min.into(),
                        max: max.into(),
                    });
                }
                let zone = (!in_sky).then(|| ZoneCulled { zones: vec![zone], min, max, only_from: None });
                let drawn_by = if in_sky { &mut sky_models } else { &mut models };
                drawn_by.push(ModelView { sections, instances: vec![Transform::IDENTITY], zone });
            }
        }
        static_terrains.push(terrain);
    }
    let mut drawn_decorations = 0;
    for layer in decorations.into_iter().filter(|layer| !layer.placements.is_empty()) {
        let (count, in_sky) = (layer.placements.len(), layer.in_sky);
        let Some(decorated) = decoration_models(game, t, texture, id, layer) else { continue };
        drawn_decorations += count;
        let drawn_by = if in_sky { &mut sky_models } else { &mut models };
        drawn_by.extend(decorated);
    }
    TerrainParts { models, sky_models, static_terrains, terrain_sectors, terrains, hidden_terrains, drawn_decorations }
}


struct BspParts {
    models: Vec<ModelView>,
    sky_models: Vec<ModelView>,
    textured: usize,
    lit: usize,
    min: Vec3,
    max: Vec3,
    atlases: Vec<Option<Handle<Image>>>,
}

fn bsp_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    model: &Model,
    groups: &BTreeMap<BspKey, Vec<(&ModelPolygon, Option<Vec<[f32; 2]>>)>>,
) -> BspParts {
    let atlases: Vec<Option<Handle<Image>>> = model
        .light_map_textures
        .iter()
        .map(|atlas| atlas.texture().and_then(|texture| texture_to_image(&texture, None)))
        .map(|image| image.map(|image| t.add_image(image)))
        .collect();
    let points =
        groups.values().flatten().flat_map(|(p, _)| &p.positions).map(|&p| Vec3::from(to_bevy(p)));
    let (min, max) = points.fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let (mut textured, mut lit) = (0, 0);
    let (sky_bsp, bsp): (Vec<_>, Vec<_>) = groups
        .iter()
        .map(|(BspKey { in_sky, material, atlas, flags, zones, .. }, polygons)| {
            let (in_sky, material, atlas, flags) = (*in_sky, *material, *atlas, *flags);
            let mut found = texture(game, t, id, material);
            if let Some(MaterialImages { state, .. }) = &mut found {
                if flags & poly_flags::TRANSLUCENT != 0 {
                    state.blending = Some(Blending::Additive);
                } else if flags & poly_flags::MASKED != 0 {
                    state.blending = Some(Blending::Masked);
                }
                state.two_sided |= flags & poly_flags::TWO_SIDED != 0;
            }
            textured += polygons.len() * usize::from(found.is_some());
            let lightmap = atlas.and_then(|atlas| atlases.get(atlas).cloned().flatten());
            lit += polygons.len() * usize::from(lightmap.is_some());
            let size = found.as_ref().and_then(|found| t.image_size(&found.image)).unwrap_or(UVec2::splat(128));
            let scale = [1.0 / size.x.max(1) as f32, 1.0 / size.y.max(1) as f32];
            let mut mesh = model_polygons_to_mesh(polygons.iter().map(|(p, uvs)| (*p, uvs.as_deref())), scale);
            let zone = zones.clone().zip(bounds(polygons.iter().flat_map(|(p, _)| p.positions.iter().copied())));
            let zone = zone.map(|(zones, (min, max))| ZoneCulled { zones, min, max, only_from: None });
            let chunks = if zone.is_none() { crate::render::world_pass::chunks_of(&mut mesh) } else { None };
            let section = SectionView::textured(t.add_mesh(mesh), found);
            let section = SectionView { no_scene_lights: true, lightmap: lightmap.filter(|_| !section.fullbright), chunks, ..section };
            (in_sky, ModelView { sections: vec![section], instances: vec![Transform::IDENTITY], zone })
        })
        .partition(|(in_sky, _)| *in_sky);
    let strip = |models: Vec<(bool, ModelView)>| models.into_iter().map(|(_, model)| model).collect::<Vec<_>>();
    BspParts { models: strip(bsp), sky_models: strip(sky_bsp), textured, lit, min, max, atlases }
}


fn antiportal_actors(game: &mut Game, model: &Model, paths: &[(String, MeshPlacement)]) -> Vec<AntiportalActor> {
    paths
        .iter()
        .filter_map(|(path, placement)| {
            let (package, export) = game.export_by_path_of_class(path, "ConvexVolume")?;
            let (pkg, file) = game.data(package);
            let volume = read_convex_volume(pkg, file, &pkg.exports[export]).ok()?;
            AntiportalActor::new(&volume, placement, |min, max| p2_collision::box_zones(model, min, max))
        })
        .collect()
}

fn static_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    level: &StaticLevel,
    placements: HashMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
) -> (StaticParts, usize, usize) {
    let (mut parts, mut missing) = (StaticParts::default(), 0);
    for (mesh, placements) in placements {
        if !static_mesh_models(game, t, texture, level, mesh, placements, &mut parts) {
            missing += 1;
        }
    }
    let batches = std::mem::take(&mut parts.batches);
    let merged = batches.len();
    for (view, batch) in batches.into_values() {
        let mut mesh = batch.into_mesh();
        let chunks = crate::render::world_pass::chunks_of(&mut mesh);
        parts.models.push(ModelView { sections: vec![SectionView { mesh: t.add_mesh(mesh), chunks, ..view }], instances: vec![Transform::IDENTITY], zone: None });
    }
    (parts, missing, merged)
}


fn read_map_model(pkg: &Package, file: &[u8], package: &str) -> Result<(String, Model), LoadError> {
    let export = pkg
        .exports
        .iter()
        .find(|e| pkg.object_name(e.class_index) == Some("Level"))
        .ok_or_else(|| LoadError::NoLevel { package: package.to_owned() })?;
    let name = pkg.name(export.object_name).to_owned();
    let level = read_level(pkg, file, export).map_err(|source| LoadError::Level { name: name.clone(), source })?;
    let model = usize::try_from(level.model - 1).ok().and_then(|i| pkg.exports.get(i));
    let model = model.ok_or_else(|| LoadError::NoModel { name: name.clone() })?;
    let model = read_model(pkg, file, model).map_err(|source| LoadError::Level { name: name.clone(), source })?;
    Ok((name, model))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use p2_assets::{MeshActor, read_actor_location, read_mesh_actor, read_model, read_texture, texture_rgba};
    use p2_script::{Objects, Source};
    use p2_render_cpu::{MeshLight};
    use p2_package::TagValue;

    use super::*;

    fn mesh_placement(loader: &Loader, actor: &MeshActor) -> ((PackageId, i32), MeshPlacement) {
        let mesh = (loader.id_of(actor.mesh_package).unwrap(), actor.static_mesh);
        (mesh, MeshPlacement { location: actor.location, rotation: actor.rotation, scale: actor.scale, pre_pivot: actor.pre_pivot })
    }

    #[test]
    fn only_surfaces_a_switch_restyles_leave_the_world_pass() {
        let mut images = Assets::<Image>::default();
        let [restyled, image_only, plain] = [(); 3].map(|()| images.add(Image::default()));
        let section = |image: &Handle<Image>| SectionView { texture: Some(image.clone()), chunks: Some(Arc::from(Vec::new())), ..SectionView::textured(Handle::default(), None) };
        let mut models = [ModelView { sections: vec![section(&restyled), section(&image_only), section(&plain)], instances: Vec::new(), zone: None }];
        let switch = |image: &Handle<Image>, whole| SwitchImage { image: image.id(), whole, shown: RenderState::default() };
        keep_restyled_out_of_world_pass(&mut models, [switch(&restyled, true), switch(&image_only, false)].iter());
        let in_table: Vec<bool> = models[0].sections.iter().map(|section| section.chunks.is_some()).collect();
        assert_eq!(in_table, [false, true, true]);
    }

    #[test]
    fn splat_surface_lies_inside_the_decal_box() {
        let local = projector_box([100.0, 200.0, 50.0].into(), [-16384, 0, 0].into(), 64.0, 32.0).to_matrix().inverse();
        let surface = local.transform_point3(Vec3::new(100.0, 50.0, 200.0));
        assert!(surface.z.abs() < 0.5 - 1.0 / 34.0 + 1e-4, "surface at local z {}", surface.z);
        assert!((local.transform_point3(Vec3::new(100.0, 50.0 - 32.0, 200.0)).z + 0.5).abs() < 1e-4);
        assert!((local.transform_point3(Vec3::new(132.0, 50.0, 200.0)).y.abs() - 0.5).abs() < 1e-4);
    }

    #[test]
    #[ignore = "survey: prints only"]
    fn splat_textures_survey() {
        let Some(root) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut library = Library::default();
        let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
        let (game, _) = library.open(&root, &map).unwrap();
        for (path, blending) in [
            ("nathans.Skins.bloodsplatblend1", 1),
            ("nathans.Skins.bloodsplatblend5", 1),
            ("nathans.Skins.blooddripblend1", 1),
            ("nathans.skins.pistolhitblend", 2),
            ("nathans.skins.shotgunhitblend1", 2),
        ] {
            let Some((package, export)) = game.export_by_path(path) else {
                eprintln!("{path}: not found");
                continue;
            };
            let texture = game.material_texture_rgba(package, export_reference(export)).expect("texture");
            let data = texture.image.data.clone().unwrap_or_default();
            let size = texture.image.texture_descriptor.size;
            let texels = (size.width * size.height) as usize;
            let mean = |data: &[u8], filter: &dyn Fn(&[u8]) -> bool| {
                let picked: Vec<&[u8]> = data[..texels * 4].chunks(4).filter(|t| filter(t)).collect();
                let n = picked.len().max(1) as f32;
                let m = [0, 1, 2, 3].map(|c| picked.iter().map(|t| f32::from(t[c])).sum::<f32>() / n);
                (picked.len(), m.map(|v| v.round()))
            };
            eprintln!(
                "{path}: {:?} {}x{}, state {:?}; texels alpha < 128 {:?}, alpha >= 128 {:?}; all {:?}; projector blending {blending}",
                texture.image.texture_descriptor.format,
                size.width,
                size.height,
                texture.state.blending,
                mean(&data, &|t| t[3] < 128),
                mean(&data, &|t| t[3] >= 128),
                mean(&data, &|_| true),
            );
        }
    }

    fn saved_light(pkg: &Package, data: &[u8], reference: i32) -> Option<MeshLight> {
        let export = pkg.exports.get(usize::try_from(reference - 1).ok()?)?;
        if pkg.object_name(export.class_index) != Some("Light") {
            return None;
        }
        let object = pkg.read_object(data, export).ok()?;
        let mut light = MeshLight::default();
        for property in &object.properties {
            match (pkg.name(property.name), &property.value) {
                ("Location", TagValue::Struct { data, .. }) => {
                    light.location = [0, 4, 8].map(|at| f32::from_le_bytes(data[at..at + 4].try_into().unwrap())).into();
                }
                ("Rotation", TagValue::Struct { data, .. }) => {
                    light.rotation = [0, 4, 8].map(|at| i32::from_le_bytes(data[at..at + 4].try_into().unwrap())).into();
                }
                ("LightType", &TagValue::Byte(v)) => light.light_type = p2_render_cpu::LightType(v),
                ("LightPeriod", &TagValue::Byte(v)) => light.period = v,
                ("LightPhase", &TagValue::Byte(v)) => light.phase = v,
                ("LightEffect", &TagValue::Byte(v)) => light.effect = p2_render_cpu::LightEffect(v),
                ("LightBrightness", &TagValue::Float(v)) => light.brightness = v,
                ("LightHue", &TagValue::Byte(v)) => light.hue = v,
                ("LightSaturation", &TagValue::Byte(v)) => light.saturation = v,
                ("LightRadius", &TagValue::Float(v)) => light.radius = v,
                ("LightCone", &TagValue::Byte(v)) => light.cone = v,
                ("bDynamicLight", &TagValue::Bool(v)) => light.dynamic = v,
                _ => {}
            }
        }
        Some(light)
    }

    /// `ZoneInfo` defaults: saturation 255, hue and brightness 0.
    fn saved_ambient(pkg: &Package, data: &[u8], export: &p2_package::Export) -> [f32; 3] {
        let object = pkg.read_object(data, export).unwrap();
        let byte = |name, default| match object.property(pkg, name) {
            Some(&TagValue::Byte(v)) => v,
            _ => default,
        };
        p2_render_cpu::hsv_color(byte("AmbientHue", 0), byte("AmbientSaturation", 255), byte("AmbientBrightness", 0))
    }

    #[test]
    fn suburbs3_static_meshes_not_black() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut loader = Loader::open(&dir.join("System")).unwrap();
        loader.load_scripts().unwrap();
        let map = loader.load("Suburbs-3").unwrap();
        let (environment, model) = {
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
            let level = read_level(pkg, data, level).unwrap();
            let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
            let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
            let zone_ambients = model
                .zones
                .iter()
                .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
                .collect();
            let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
            (LightEnvironment::new(lights, zone_ambients, 1.0), model)
        };
        let (mut lit, mut relit, mut black_before, mut black_after, mut dark_zone) = (0, 0, 0, 0, 0);
        let mean = |colors: &[[u8; 4]], scale: f32| {
            colors.iter().map(|c| c[..3].iter().map(|&b| f32::from(b) * scale).sum::<f32>() / 3.0).sum::<f32>() / colors.len().max(1) as f32
        };
        let exports = loader.package(map).package.exports.len();
        for index in 0..exports {
            let (mesh, placement, instance) = {
                let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                let export = &pkg.exports[index];
                if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                    continue;
                }
                let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
                if actor.instance <= 0 || actor.unlit {
                    continue;
                }
                let instance = read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap();
                let (mesh, placement) = mesh_placement(&loader, &actor);
                (mesh, placement, instance)
            };
            let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
            let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
            let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
            let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
            if instance.colors.len() != mesh.positions.len() {
                continue;
            }
            let colors = environment.mesh_colors(&model, &mesh, Some(&instance), &lighting);
            lit += 1;
            let lights: Vec<_> = instance.lights.iter().map(|l| environment.lights.get(&l.actor).copied()).collect();
            relit += usize::from(p2_render_cpu::needs_relight(&instance, &lights));
            black_before += usize::from(mean(&instance.colors, 1.0) < 32.0);
            if mean(&colors, 2.0) < 32.0 {
                black_after += 1;
                let zone = environment.zone_ambients.get(usize::from(lighting.zone)).copied().unwrap_or_default();
                dark_zone += usize::from(zone.iter().all(|&c| c * 255.0 < 1.0));
            }
        }
        eprintln!(
            "{lit} lit actors, {relit} relit; near black: {black_before} before, {black_after} after ({dark_zone} in zones without ambient)"
        );
        assert!(lit > 1000 && relit > 50, "{lit} {relit}");
        assert!(black_after * 5 < black_before, "{black_after} of {black_before}");
        assert_eq!(dark_zone, black_after, "near-black actors in a zone with ambient light");
    }

    #[test]
    fn suburbs3_brightness_balance() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut loader = Loader::open(&dir.join("System")).unwrap();
        loader.load_scripts().unwrap();
        let map = loader.load("Suburbs-3").unwrap();
        let shown = |c: &[u8]| c[..3].iter().map(|&b| (2.0 * f64::from(b)).min(255.0)).sum::<f64>() / 3.0;
        let mean_shown = |colors: &[[u8; 4]]| colors.iter().map(|c| shown(c)).sum::<f64>() / colors.len().max(1) as f64;
        let (model, environment, exports, bsp, terrain) = {
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
            let level = read_level(pkg, data, level).unwrap();
            let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
            let atlases: Vec<_> = model.light_map_textures.iter().map(|a| a.texture().and_then(|t| texture_rgba(&t, None))).collect();
            let mut texels = Vec::new();
            for polygon in model.polygons() {
                let Some((atlas, uvs)) = model.light_map_uvs(&polygon) else { continue };
                let Some(Some((w, h, atlas))) = atlases.get(atlas) else { continue };
                let (w, h) = (*w as usize, *h as usize);
                texels.extend(uvs.iter().map(|&[u, v]| atlas[((v * h as f32) as usize).min(h - 1) * w + ((u * w as f32) as usize).min(w - 1)]));
            }
            let mut layouts = p2_script::Layouts::new(&loader);
            let terrain: Vec<[u8; 4]> = (pkg.exports.iter().enumerate())
                .filter(|(_, export)| pkg.object_name(export.class_index) == Some("TerrainInfo"))
                .flat_map(|(index, export)| {
                    let layout = layouts.class_layout(Source { package: pkg, data }, index).unwrap();
                    read_terrain(pkg, data, export, &layout).unwrap().vertex_colors
                })
                .collect();
            let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
            let zone_ambients = (model.zones.iter())
                .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
                .collect();
            let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
            let environment = LightEnvironment::new(lights, zone_ambients, 1.0);
            (model, environment, pkg.exports.len(), mean_shown(&texels), mean_shown(&terrain))
        };
        let (mut lit, mut lit_sum, mut ambient_only, mut ambient_sum) = (0, 0.0, 0, 0.0);
        for index in 0..exports {
            let (mesh, placement, instance) = {
                let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                let export = &pkg.exports[index];
                if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                    continue;
                }
                let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
                if actor.unlit {
                    continue;
                }
                let instance = (actor.instance > 0)
                    .then(|| read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap());
                let (mesh, placement) = mesh_placement(&loader, &actor);
                (mesh, placement, instance)
            };
            let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
            let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
            let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
            let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
            let drawn = mean_shown(&environment.mesh_colors(&model, &mesh, instance.as_ref(), &lighting));
            if instance.is_some_and(|i| i.colors.len() == mesh.positions.len()) {
                (lit, lit_sum) = (lit + 1, lit_sum + drawn);
            } else {
                (ambient_only, ambient_sum) = (ambient_only + 1, ambient_sum + drawn);
            }
        }
        let (statics, ambient) = (lit_sum / f64::from(lit), ambient_sum / f64::from(ambient_only));
        eprintln!(
            "shown at 2x: BSP {bsp:.1}, terrain {terrain:.1}, {lit} lit static meshes {statics:.1}, {ambient_only} with ambient only {ambient:.1}"
        );
        assert!(lit > 1000 && ambient_only > 100, "{lit} {ambient_only}");
        assert!(bsp < statics && statics < terrain, "BSP {bsp}, static meshes {statics}, terrain {terrain}");
    }

    fn ray_hits<'a>(polygons: &[&'a ModelPolygon], origin: [f32; 3], dir: [f32; 3]) -> Vec<(f32, &'a ModelPolygon)> {
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let mut hits = Vec::new();
        for &p in polygons {
            if dot(dir, p.normal) >= 0.0 && p.poly_flags & poly_flags::TWO_SIDED == 0 {
                continue;
            }
            for i in 1..p.positions.len().saturating_sub(1) {
                let (a, b, c) = (p.positions[0], p.positions[i], p.positions[i + 1]);
                let (e1, e2) = (sub(b, a), sub(c, a));
                let h = cross(dir, e2);
                let det = dot(e1, h);
                if det.abs() < 1e-6 {
                    continue;
                }
                let s = sub(origin, a);
                let u = dot(s, h) / det;
                let q = cross(s, e1);
                let v = dot(dir, q) / det;
                let t = dot(e2, q) / det;
                if (0.0..=1.0).contains(&u) && v >= 0.0 && u + v <= 1.0 && t > 0.1 {
                    hits.push((t, p));
                    break;
                }
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        hits
    }

    #[test]
    fn props_without_baked_light_not_black() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut loader = Loader::open(&dir.join("System")).unwrap();
        loader.load_scripts().unwrap();
        for map_name in ["Suburbs-3", "Church"] {
            let map = loader.load(map_name).unwrap();
            let (environment, model) = {
                let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
                let level = read_level(pkg, data, level).unwrap();
                let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
                let info = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("LevelInfo")).unwrap();
                let zone_ambients = (model.zones.iter())
                    .map(|zone| saved_ambient(pkg, data, pkg.exports.get(usize::try_from(zone.actor - 1).ok().unwrap_or(usize::MAX)).unwrap_or(info)))
                    .collect();
                let lights = (1..=pkg.exports.len() as i32).filter_map(|r| saved_light(pkg, data, r).map(|l| (r, l))).collect();
                (LightEnvironment::new(lights, zone_ambients, 1.0), model)
            };
            let shown = |colors: &[[u8; 4]]| {
                colors.iter().map(|c| c[..3].iter().map(|&b| (2.0 * f32::from(b)).min(255.0)).sum::<f32>() / 3.0).sum::<f32>()
                    / colors.len().max(1) as f32
            };
            let exports = loader.package(map).package.exports.len();
            let (mut statics, mut props) = (Vec::new(), Vec::new());
            for index in 0..exports {
                let (mesh, placement, instance, class) = {
                    let LoadedPackage { package: pkg, data, .. } = loader.package(map);
                    let export = &pkg.exports[index];
                    if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                        continue;
                    }
                    let Ok(Some(actor)) = read_mesh_actor(&mut Objects::new(&loader), Source { package: pkg, data }, index) else { continue };
                    if actor.unlit {
                        continue;
                    }
                    let instance = (actor.instance > 0).then(|| read_static_mesh_instance(pkg, data, &pkg.exports[actor.instance as usize - 1]).unwrap());
                    let (mesh, placement) = mesh_placement(&loader, &actor);
                    (mesh, placement, instance, pkg.object_name(export.class_index).unwrap_or("?").to_owned())
                };
                let Some(resolved) = loader.resolve(mesh.0, mesh.1).ok().flatten() else { continue };
                let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
                let mesh = read_static_mesh(pkg, data, &pkg.exports[resolved.export.unwrap()]).unwrap();
                let lighting = MeshLighting { placement, zone: p2_collision::point_zone(&model, placement.location), glow: 0 };
                let after = shown(&environment.mesh_colors(&model, &mesh, instance.as_ref(), &lighting));
                if instance.as_ref().is_some_and(|instance| instance_fits(&mesh, instance)) {
                    statics.push((placement.location, after));
                    continue;
                }
                let ambient = environment.ambient(lighting.zone, 0).map(|c| (c * 255.0).floor().min(255.0));
                let before = ambient.iter().map(|&c| (2.0 * c).min(255.0)).sum::<f32>() / 3.0;
                props.push((class, placement.location, before, after));
            }
            let near = |at: [f32; 3]| {
                let around: Vec<f32> = statics
                    .iter()
                    .filter(|(p, _)| (0..3).map(|i| (p[i] - at[i]).powi(2)).sum::<f32>() < 1000.0f32.powi(2))
                    .map(|&(_, b)| b)
                    .collect();
                (!around.is_empty()).then(|| around.iter().sum::<f32>() / around.len() as f32)
            };
            let black_before = props.iter().filter(|p| p.2 < 32.0).count();
            let black_after = props.iter().filter(|p| p.3 < 32.0).count();
            let dark: Vec<_> = props.iter().filter(|p| p.3 < 32.0 && near(p.1.into()).is_some_and(|b| b >= 64.0)).collect();
            if !dark.is_empty() {
                eprintln!("  dark among lit: {dark:?}");
            }
            let dark_among_lit = dark.len();
            let (mut prop_sum, mut static_sum, mut paired) = (0.0, 0.0, 0);
            for p in &props {
                if let Some(b) = near(p.1.into()) {
                    (prop_sum, static_sum, paired) = (prop_sum + p.3, static_sum + b, paired + 1);
                }
            }
            let mut classes: HashMap<&str, usize> = HashMap::new();
            for p in &props {
                *classes.entry(p.0.as_str()).or_default() += 1;
            }
            let mean = |f: &dyn Fn(&(String, Vector, f32, f32)) -> f32| props.iter().map(f).sum::<f32>() / props.len().max(1) as f32;
            eprintln!(
                "{map_name}: {} props without baked light ({classes:?}); mean shown {:.1} before, {:.1} after; near black {black_before} before, {black_after} after, {dark_among_lit} among lit static meshes; {paired} with static meshes within 1000: props {:.1}, static meshes {:.1}",
                props.len(),
                mean(&|p| p.2),
                mean(&|p| p.3),
                prop_sum / paired.max(1) as f32,
                static_sum / paired.max(1) as f32,
            );
            assert!(!props.is_empty());
            assert!(black_after <= black_before, "{black_after} > {black_before}");
            assert!(dark_among_lit * 100 <= props.len(), "{dark_among_lit} props near black among lit static meshes");
        }
    }

    #[test]
    fn only_translucent_bsp_splits_by_place() {
        let node = p2_assets::BspNode {
            plane: [0.0; 4],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [1, 1],
            num_vertices: 0,
            leaves: [0; 2],
            section: None,
            first_vertex: 0,
            light_map: None,
        };
        let model = Model { nodes: vec![node], ..Model::default() };
        let polygon = |poly_flags, x| ModelPolygon { node: 0, surface: 0, material: 1, poly_flags, normal: [0.0; 3], positions: vec![[x, 0.0, 0.0]], uvs: Vec::new() };
        let far = 1_000_000.0;
        let polygons = [polygon(0, 0.0), polygon(0, far), polygon(poly_flags::TRANSLUCENT, 0.0), polygon(poly_flags::TRANSLUCENT, far)];
        assert_eq!(bsp_groups(&model, &polygons, None).len(), 3);
    }

    #[test]
    fn suburbs3_sky_zone_drawn_by_the_sky_camera_only() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut loader = Loader::open(&dir.join("System")).unwrap();
        loader.load_scripts().unwrap();
        for map_name in ["Suburbs-3", "Church"] {
            let map = loader.load(map_name).unwrap();
            let LoadedPackage { package: pkg, data, .. } = loader.package(map);
            let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
            let level = read_level(pkg, data, level).unwrap();
            let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
            let sky = pkg.exports.iter().position(|e| pkg.object_name(e.class_index) == Some("SkyZoneInfo")).unwrap();
            let sky_zone = p2_collision::point_zone(&model, read_actor_location(&mut Objects::new(&loader), Source { package: pkg, data }, sky).unwrap());
            assert_eq!(pkg.object_name(pkg.exports[model.zones[usize::from(sky_zone)].actor as usize - 1].class_index), Some("SkyZoneInfo"));
            let polygons = model.polygons();
            let groups = bsp_groups(&model, &polygons, Some(sky_zone));
            let front_zone = |p: &ModelPolygon| model.nodes[p.node].zones[1];
            let (in_sky, in_view): (Vec<_>, Vec<_>) = groups.iter().flat_map(|(k, v)| v.iter().map(move |(p, _)| (k.in_sky, *p))).partition(|(sky, _)| *sky);
            assert!(!in_sky.is_empty() && in_sky.iter().all(|(_, p)| front_zone(p) == sky_zone));
            assert!(in_view.iter().all(|(_, p)| front_zone(p) != sky_zone && p.poly_flags & poly_flags::FAKE_BACKDROP == 0));
            let without = bsp_groups(&model, &polygons, None);
            let backdrop = without.values().flatten().filter(|(p, _)| p.poly_flags & poly_flags::FAKE_BACKDROP != 0).count();
            assert!(backdrop > 300, "{backdrop}");
            let drawn: Vec<&ModelPolygon> = polygons.iter().filter(|p| p.poly_flags & (poly_flags::INVISIBLE | poly_flags::PORTAL) == 0).collect();
            let mut objects = Objects::new(&loader);
            let starts: Vec<[f32; 3]> = (0..pkg.exports.len())
                .filter(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart"))
                .filter_map(|i| read_actor_location(&mut objects, Source { package: pkg, data }, i).ok())
                .map(<[f32; 3]>::from)
                .take(4)
                .collect();
            let (mut sky_rays, mut sky_box_before, mut leaks) = (0, 0, 0);
            for start in &starts {
                let origin = [start[0], start[1], start[2] + 60.0];
                let n = 1000;
                for i in 0..n {
                    let z = 1.0 - (i as f32 + 0.5) / n as f32 * 2.0;
                    let r = (1.0 - z * z).sqrt();
                    let phi = i as f32 * 2.399_963;
                    let hits = ray_hits(&drawn, origin, [r * phi.cos(), r * phi.sin(), z]);
                    if !hits.first().is_some_and(|(_, p)| p.poly_flags & poly_flags::FAKE_BACKDROP != 0) {
                        continue;
                    }
                    sky_rays += 1;
                    let behind = hits.iter().find(|(_, p)| p.poly_flags & poly_flags::FAKE_BACKDROP == 0);
                    sky_box_before += usize::from(behind.is_some_and(|(_, p)| front_zone(p) == sky_zone));
                    leaks += usize::from(behind.is_some_and(|(_, p)| front_zone(p) != sky_zone));
                }
            }
            eprintln!("{map_name}: sky zone {sky_zone}, {} sky polygons; {sky_rays} rays see the sky, behind the backdrop {sky_box_before} the sky box (now not drawn), {leaks} other BSP", in_sky.len());
            assert!(sky_rays > 500);
            assert!(leaks * 100 <= sky_rays, "{leaks} of {sky_rays}");
        }
    }

    #[test]
    fn window_alpha_from_opacity_mask() {
        let Some(root) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
        let mut library = Library::default();
        let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
        let (game, _) = library.open(&root, &map).unwrap();
        let (package, export) = game.export_by_path("P2R_Tex_D.window_house_innertrans").expect("window shader");
        let drawn = game.material_texture(package, export_reference(export)).expect("window texture");
        assert_eq!(drawn.state.blending, Some(Blending::Alpha));
        let decoded = |game: &mut Game, path: &str| {
            let (package, export) = game.export_by_path(path).unwrap();
            let (package, data) = game.data(package);
            let source = Source { package, data };
            texture_rgba(&read_texture(&mut game.objects(), source, export).unwrap(), None).unwrap()
        };
        let (width, height, diffuse) = decoded(game, "STV-3.window-house");
        let (_, _, mask) = decoded(game, "P2R_Tex_D.window_house_innermask");
        assert_eq!(drawn.image.texture_descriptor.size.width, width);
        let data = drawn.image.data.as_ref().unwrap();
        let texels = (width * height) as usize;
        let drawn: Vec<&[u8]> = data[..texels * 4].chunks(4).collect();
        let differs = diffuse.iter().zip(&mask).filter(|(d, m)| d[3] != m[3]).count();
        eprintln!("{differs} of {texels} texels: the diffuse's own alpha differs from the mask's");
        assert!(drawn.iter().zip(&mask).all(|(d, m)| d[3] == m[3]));
        assert!(drawn.iter().zip(&diffuse).all(|(d, c)| d[..3] == c[..3]));
    }
    #[test]
    fn projector_tag_takes_the_innermost_final_blend() {
        let projector = |material_blending| ProjectorActor {
            texture: String::new(),
            location: Vector::default(),
            rotation: Rotator::default(),
            draw_scale: 1.0,
            max_trace: 100.0,
            fov: 0,
            blending: ProjectorBlending::Add,
            material_blending,
            targets: crate::actors::ProjectTargets(0),
            tagged: false,
        };
        let splat = Some(FinalBlend { blending: p2_assets::FrameBufferBlending::AlphaBlend, alpha_ref: Some(10) });
        assert_eq!(projector_tag(&projector(ProjectorBlending::None), splat), PROJECTOR_TAG | ALPHA_TEST_TAG | 10 << 22 | 2);
        assert_eq!(projector_tag(&projector(ProjectorBlending::Modulate), splat), PROJECTOR_TAG | 1 << 8 | 4);
        assert_eq!(projector_tag(&projector(ProjectorBlending::None), None), PROJECTOR_TAG | 4);
        assert_eq!(projector_tag(&ProjectorActor { tagged: true, ..projector(ProjectorBlending::None) }, None), PROJECTOR_TAG | TAGGED_TAG | 4);
    }
}
