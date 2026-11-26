use bevy::app::{App, Plugin};
use bevy::asset::uuid::Uuid;
use bevy::asset::{AssetApp, Assets, Handle};
use bevy::mesh::{Mesh, MeshVertexBufferLayoutRef};
use bevy::pbr::{Material, MaterialPlugin, MeshPipelineKey};
use bevy::render::RenderApp;
use bevy::render::render_resource::{BlendComponent, BlendFactor, BlendOperation, RenderPipelineDescriptor};
use bevy::render::renderer::RenderDevice;
use bevy::render::settings::WgpuFeatures;
use bevy::shader::{Shader, ShaderDefVal};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

fn beside_exe(folder: &str, in_repo: &str) -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(folder)));
    beside.filter(|dir| dir.is_dir()).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(in_repo))
}

static SHADER_DIR: LazyLock<PathBuf> = LazyLock::new(|| beside_exe("assets/shaders", "assets/shaders"));

pub(crate) const fn shader_handle(name: &str) -> Handle<Shader> {
    let (bytes, mut hash, mut i) = (name.as_bytes(), 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d_u128, 0);
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u128).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
        i += 1;
    }
    Handle::Uuid(Uuid::from_u128(hash), PhantomData)
}

fn shader_file(name: &str) -> Shader {
    let file = SHADER_DIR.join(name);
    let source = std::fs::read_to_string(&file).unwrap_or_else(|error| {
        bevy::log::error!("shader {}: {error}", file.display());
        String::new()
    });
    let path = format!("p2_render/{name}");
    if name.ends_with(".wgsl") { Shader::from_wgsl(source, path) } else { Shader::from_wesl(source, path) }
}

pub(crate) fn add_shaders<'a>(app: &mut App, names: impl IntoIterator<Item = &'a str>) {
    let mut assets = app.world_mut().resource_mut::<Assets<Shader>>();
    for name in names {
        if let Err(error) = assets.insert(shader_handle(name).id(), shader_file(name)) {
            bevy::log::error!("shader {name}: {error}");
        }
    }
}

const PROJECTOR_SHADER: Handle<Shader> = shader_handle("projector.wesl");
const SKY_VERTEX_SHADER: Handle<Shader> = shader_handle("sky_vertex.wesl");

pub(crate) struct ShaderLibraryPlugin;

impl Plugin for ShaderLibraryPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [
            "dynamic_light.wesl",
            "shader_layers.wesl",
            "layer_bindings.wesl",
            "modern.wesl",
            "modern_base.wesl",
            "relief.wesl",
            "sun_shadow.wesl",
            "lamp_shadow.wesl",
            "reflections_prepass.wesl",
            "probes.wesl",
            "color.wesl",
            "projector.wesl",
            "tone_curve.wesl",
            "screen_pass.wesl",
            "surface.wesl",
            "material_layers.wesl",
            "layer_types.wesl",
            "depth_surface.wesl",
            "sun_levels.wesl",
            "sky_view.wesl",
            "sky_vertex.wesl",
            "liquid_look.wesl",
        ]);
    }

    fn finish(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        let half_floats = app.world().get_resource::<RenderDevice>().is_some_and(|device| device.features().contains(WgpuFeatures::SHADER_F16));
        if let Some(mut projector) = app.world_mut().resource_mut::<Assets<Shader>>().get_mut(PROJECTOR_SHADER.id()) {
            projector.shader_defs.push(ShaderDefVal::Bool("HALF_FLOATS".into(), half_floats));
        }
    }
}

pub(crate) fn add_libraries(app: &mut App) {
    if !app.is_plugin_added::<ShaderLibraryPlugin>() {
        app.add_plugins(ShaderLibraryPlugin);
    }
}

pub(crate) fn add_material<M: Material>(app: &mut App, name: &str)
where
    MaterialPlugin<M>: Plugin,
{
    if app.get_sub_app(RenderApp).is_none() {
        app.init_asset::<M>();
        return;
    }
    add_libraries(app);
    add_shaders(app, [name]);
    app.add_plugins(MaterialPlugin::<M>::default());
}

#[cfg(test)]
pub(crate) fn shader_const(source: &str, name: &str) -> f64 {
    let head = format!("const {name}:");
    let line = source.lines().map(str::trim).find(|line| line.starts_with(&head)).unwrap_or_else(|| panic!("no const {name} in the shader"));
    let value = line.split_once('=').map(|(_, value)| value.trim().trim_end_matches(';').trim()).unwrap_or_else(|| panic!("const {name} has no value"));
    let value = value.strip_suffix(['u', 'i']).unwrap_or(value);
    let parsed = match value.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).map(|bits| bits as f64).ok(),
        None => value.trim_end_matches('f').parse().ok(),
    };
    parsed.unwrap_or_else(|| panic!("const {name} = {value} is not a number"))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkyKey {
    pub sky: bool,
    pub mirror: bool,
    pub light: LightSource,
    pub glass: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LightSource {
    Retail,
    Modern,
}

pub(crate) fn mark_sky(descriptor: &mut RenderPipelineDescriptor, key: SkyKey) {
    if key.sky {
        sky_vertex(descriptor);
    }
}

pub(crate) fn sky_vertex(descriptor: &mut RenderPipelineDescriptor) {
    let prepass = descriptor.vertex.shader_defs.contains(&ShaderDefVal::from("PREPASS_PIPELINE"));
    descriptor.vertex.shader = SKY_VERTEX_SHADER;
    descriptor.vertex.entry_point = Some(if prepass { "prepass_vertex" } else { "vertex" }.into());
    sky_view_defs(descriptor);
}

pub(crate) fn sky_view_defs(descriptor: &mut RenderPipelineDescriptor) {
    descriptor.vertex.shader_defs.push("SKY_VIEW".into());
    if let Some(fragment) = descriptor.fragment.as_mut() {
        fragment.shader_defs.retain(|def| !matches!(def, ShaderDefVal::Bool(name, _) if name == "DISTANCE_FOG"));
        fragment.shader_defs.push("SKY_VIEW".into());
    }
}

/// A lit material's normal map slot holds the lamps' screen shadows, not a normal map; Bevy's def for one
/// would stop a two-sided surface's back face flipping its normal.
pub(crate) fn unmark_normal_map(descriptor: &mut RenderPipelineDescriptor) {
    if let Some(fragment) = descriptor.fragment.as_mut() {
        fragment.shader_defs.retain(|def| !matches!(def, ShaderDefVal::Bool(name, _) if name.starts_with("STANDARD_MATERIAL_NORMAL_MAP")));
    }
}

pub(crate) fn mark_see_through(descriptor: &mut RenderPipelineDescriptor, key: MeshPipelineKey) {
    if key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS) == MeshPipelineKey::BLEND_ALPHA
        && let Some(fragment) = descriptor.fragment.as_mut()
    {
        fragment.shader_defs.push("P2_SEE_THROUGH".into());
    }
}

/// The pool record rides in the frame's alpha, so only opaque and masked world pipelines without MSAA write it.
pub(crate) fn mark_pool_mirror(descriptor: &mut RenderPipelineDescriptor, mesh_key: MeshPipelineKey, key: SkyKey) {
    if key.mirror
        && !key.sky
        && mesh_key.msaa_samples() == 1
        && mesh_key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS) == MeshPipelineKey::BLEND_OPAQUE
        && let Some(fragment) = descriptor.fragment.as_mut()
    {
        fragment.shader_defs.push("POOL_MIRROR".into());
    }
}

/// Glass blends minus the frame's alpha squared; the negative alpha tells the reflections pass to skip the pixel.
pub(crate) fn mark_glass_mirror(descriptor: &mut RenderPipelineDescriptor, mesh_key: MeshPipelineKey, key: SkyKey) {
    let blend = mesh_key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS);
    let premultiplied = blend == MeshPipelineKey::BLEND_PREMULTIPLIED_ALPHA;
    let wanted = key.glass
        && !key.sky
        && mesh_key.msaa_samples() == 1
        && (blend == MeshPipelineKey::BLEND_ALPHA || premultiplied)
        && mesh_key.contains(MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::VIEW_TRANSMISSION_TEXTURE);
    let Some(fragment) = descriptor.fragment.as_mut().filter(|_| wanted) else {
        return;
    };
    fragment.shader_defs.push("GLASS_MIRROR".into());
    if premultiplied {
        fragment.shader_defs.push("P2_SEE_THROUGH".into());
    }
    if let Some(blend) = fragment.targets.first_mut().and_then(|target| target.as_mut()).and_then(|target| target.blend.as_mut()) {
        blend.alpha = BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::DstAlpha, operation: BlendOperation::Subtract };
    }
}

fn over_prepass(descriptor: &RenderPipelineDescriptor, mesh_key: MeshPipelineKey, key: SkyKey) -> bool {
    let main = !descriptor.vertex.shader_defs.contains(&ShaderDefVal::from("PREPASS_PIPELINE"));
    let opaque = mesh_key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS) == MeshPipelineKey::BLEND_OPAQUE;
    main && opaque && !key.sky && mesh_key.contains(MeshPipelineKey::DEPTH_PREPASS)
}

pub(crate) fn test_depth_early(descriptor: &mut RenderPipelineDescriptor, mesh_key: MeshPipelineKey, key: SkyKey) {
    let discards = mesh_key.intersects(MeshPipelineKey::MAY_DISCARD | MeshPipelineKey::VISIBILITY_RANGE_DITHER);
    if discards && over_prepass(descriptor, mesh_key, key)
        && let Some(depth) = descriptor.depth_stencil.as_mut()
    {
        depth.depth_write_enabled = Some(false);
    }
}

pub(crate) fn mark_screen_sun(descriptor: &mut RenderPipelineDescriptor, mesh_key: MeshPipelineKey, key: SkyKey) {
    let screen = mesh_key.contains(MeshPipelineKey::NORMAL_PREPASS) && mesh_key.msaa_samples() == 1;
    if screen
        && over_prepass(descriptor, mesh_key, key)
        && let Some(fragment) = descriptor.fragment.as_mut()
        && fragment.shader_defs.contains(&ShaderDefVal::from("BINDLESS"))
    {
        fragment.shader_defs.push("SCREEN_SUN".into());
    }
}

pub(crate) fn mark_cached_lamps(descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef) {
    let main = !descriptor.vertex.shader_defs.contains(&ShaderDefVal::from("PREPASS_PIPELINE"));
    if main
        && layout.0.contains(Mesh::ATTRIBUTE_COLOR)
        && let Some(fragment) = descriptor.fragment.as_mut()
        && fragment.shader_defs.contains(&ShaderDefVal::from("BINDLESS"))
    {
        fragment.shader_defs.push("CACHED_LAMPS".into());
    }
}

/// Bevy's shadow pass takes the material's cull mode, but a level's faces look inward: from the sun's side
/// a sealed room's roof is a back face. Only directional shadow views have this key.
pub(crate) fn cast_both_sides(descriptor: &mut RenderPipelineDescriptor, key: MeshPipelineKey) {
    if key.contains(MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO) {
        descriptor.primitive.cull_mode = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::render_resource::FragmentState;

    #[test]
    fn a_lit_pipeline_keeps_no_normal_map_def() {
        let defs = ["STANDARD_MATERIAL_NORMAL_MAP", "STANDARD_MATERIAL_NORMAL_MAP_UV_B", "VERTEX_UVS"].map(ShaderDefVal::from).to_vec();
        let mut descriptor = RenderPipelineDescriptor { fragment: Some(FragmentState { shader_defs: defs, ..Default::default() }), ..Default::default() };
        unmark_normal_map(&mut descriptor);
        assert_eq!(descriptor.fragment.expect("a fragment stage").shader_defs, [ShaderDefVal::from("VERTEX_UVS")]);
    }

    #[test]
    fn only_an_unblended_world_pipeline_with_the_flag_writes_the_pool_record() {
        let marked = |mesh_key: MeshPipelineKey, key: SkyKey| {
            let mut descriptor = RenderPipelineDescriptor { fragment: Some(FragmentState::default()), ..Default::default() };
            mark_pool_mirror(&mut descriptor, mesh_key, key);
            descriptor.fragment.expect("a fragment stage").shader_defs.contains(&ShaderDefVal::from("POOL_MIRROR"))
        };
        let world = SkyKey { sky: false, mirror: true, light: LightSource::Retail, glass: false };
        assert!(marked(MeshPipelineKey::BLEND_OPAQUE, world));
        assert!(marked(MeshPipelineKey::BLEND_OPAQUE | MeshPipelineKey::MAY_DISCARD, world));
        assert!(!marked(MeshPipelineKey::BLEND_OPAQUE, SkyKey { sky: false, mirror: false, light: LightSource::Retail, glass: false }));
        assert!(!marked(MeshPipelineKey::BLEND_OPAQUE, SkyKey { sky: true, mirror: true, light: LightSource::Retail, glass: false }));
        for blend in [
            MeshPipelineKey::BLEND_ALPHA,
            MeshPipelineKey::BLEND_PREMULTIPLIED_ALPHA,
            MeshPipelineKey::BLEND_MULTIPLY,
            MeshPipelineKey::BLEND_ALPHA_TO_COVERAGE,
        ] {
            assert!(!marked(blend, world));
        }
        let mut depth_only = RenderPipelineDescriptor::default();
        mark_pool_mirror(&mut depth_only, MeshPipelineKey::BLEND_OPAQUE, world);
        assert!(depth_only.fragment.is_none());
    }

    #[test]
    fn only_see_through_glass_over_the_depth_and_the_copy_mirrors_and_marks_its_alpha() {
        use bevy::render::render_resource::{BlendState, ColorTargetState, ColorWrites, TextureFormat};
        let marked = |mesh_key: MeshPipelineKey, key: SkyKey| {
            let target = ColorTargetState { format: TextureFormat::Rgba16Float, blend: Some(BlendState::ALPHA_BLENDING), write_mask: ColorWrites::ALL };
            let mut descriptor = RenderPipelineDescriptor { fragment: Some(FragmentState { targets: vec![Some(target)], ..Default::default() }), ..Default::default() };
            mark_glass_mirror(&mut descriptor, mesh_key, key);
            let fragment = descriptor.fragment.expect("a fragment stage");
            let alpha = fragment.targets[0].as_ref().and_then(|target| target.blend).map(|blend| blend.alpha);
            let def = fragment.shader_defs.contains(&ShaderDefVal::from("GLASS_MIRROR"));
            assert_eq!(def, alpha != Some(BlendState::ALPHA_BLENDING.alpha));
            def
        };
        let glass = SkyKey { sky: false, mirror: true, light: LightSource::Modern, glass: true };
        let over = MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::VIEW_TRANSMISSION_TEXTURE;
        assert!(marked(over | MeshPipelineKey::BLEND_ALPHA, glass));
        assert!(!marked(over | MeshPipelineKey::BLEND_ALPHA, SkyKey { glass: false, ..glass }));
        assert!(!marked(over | MeshPipelineKey::BLEND_ALPHA, SkyKey { sky: true, ..glass }));
        assert!(marked(over | MeshPipelineKey::BLEND_PREMULTIPLIED_ALPHA, glass));
        assert!(!marked(over | MeshPipelineKey::BLEND_OPAQUE, glass));
        let see_through = |blend: MeshPipelineKey| {
            let mut descriptor = RenderPipelineDescriptor { fragment: Some(FragmentState::default()), ..Default::default() };
            mark_glass_mirror(&mut descriptor, over | blend, glass);
            descriptor.fragment.is_some_and(|fragment| fragment.shader_defs.contains(&ShaderDefVal::from("P2_SEE_THROUGH")))
        };
        assert!(see_through(MeshPipelineKey::BLEND_PREMULTIPLIED_ALPHA) && !see_through(MeshPipelineKey::BLEND_ALPHA));
        assert!(!marked(MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::BLEND_ALPHA, glass));
        assert!(!marked(over | MeshPipelineKey::BLEND_ALPHA | MeshPipelineKey::from_msaa_samples(4), glass));
    }

    #[test]
    fn the_shaders_know_glass_by_its_class_number() {
        let glass = p2_bevy_assets::material_class::MaterialClass::TileOrGlass as u8;
        assert_eq!(shader_const(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/modern.wesl")), "GLASS_CLASS"), f64::from(glass));
    }

    #[test]
    fn only_a_main_pipeline_that_may_discard_over_a_depth_prepass_leaves_the_depth_alone() {
        use bevy::render::render_resource::{CompareFunction, DepthStencilState, TextureFormat, VertexState};
        let writes = |mesh_key: MeshPipelineKey, key: SkyKey, defs: &[&'static str]| {
            let mut descriptor = RenderPipelineDescriptor {
                vertex: VertexState { shader_defs: defs.iter().map(|&def| ShaderDefVal::from(def)).collect(), ..Default::default() },
                depth_stencil: Some(DepthStencilState {
                    format: TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                ..Default::default()
            };
            test_depth_early(&mut descriptor, mesh_key, key);
            descriptor.depth_stencil.and_then(|depth| depth.depth_write_enabled) == Some(true)
        };
        let world = SkyKey { sky: false, mirror: false, light: LightSource::Retail, glass: false };
        let prepass = MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::BLEND_OPAQUE;
        assert!(!writes(prepass | MeshPipelineKey::MAY_DISCARD, world, &[]));
        assert!(!writes(prepass | MeshPipelineKey::VISIBILITY_RANGE_DITHER, world, &[]));
        assert!(writes(prepass, world, &[]));
        assert!(writes(MeshPipelineKey::BLEND_OPAQUE | MeshPipelineKey::MAY_DISCARD, world, &[]));
        assert!(writes(prepass | MeshPipelineKey::MAY_DISCARD, world, &["PREPASS_PIPELINE"]));
        assert!(writes(prepass | MeshPipelineKey::MAY_DISCARD, SkyKey { sky: true, mirror: false, light: LightSource::Retail, glass: false }, &[]));
        assert!(writes(MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::BLEND_ALPHA_TO_COVERAGE | MeshPipelineKey::MAY_DISCARD, world, &[]));
    }

    #[test]
    fn only_the_world_cameras_opaque_and_masked_surfaces_read_the_screen_sun() {
        use bevy::render::render_resource::VertexState;
        let marked = |mesh_key: MeshPipelineKey, key: SkyKey, vertex: &[&'static str], fragment: &[&'static str]| {
            let mut descriptor = RenderPipelineDescriptor {
                vertex: VertexState { shader_defs: vertex.iter().map(|&def| ShaderDefVal::from(def)).collect(), ..Default::default() },
                fragment: Some(FragmentState { shader_defs: fragment.iter().map(|&def| ShaderDefVal::from(def)).collect(), ..Default::default() }),
                ..Default::default()
            };
            mark_screen_sun(&mut descriptor, mesh_key, key);
            descriptor.fragment.expect("a fragment stage").shader_defs.contains(&ShaderDefVal::from("SCREEN_SUN"))
        };
        let world = SkyKey { sky: false, mirror: false, light: LightSource::Retail, glass: false };
        let both = MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::NORMAL_PREPASS | MeshPipelineKey::BLEND_OPAQUE;
        let bindless: &[&str] = &["BINDLESS"];
        assert!(marked(both, world, &[], bindless));
        assert!(marked(both | MeshPipelineKey::MAY_DISCARD, world, &[], bindless));
        assert!(!marked(both, world, &[], &[]));
        assert!(!marked(both, world, &["PREPASS_PIPELINE"], bindless));
        assert!(!marked(both, SkyKey { sky: true, mirror: false, light: LightSource::Retail, glass: false }, &[], bindless));
        assert!(!marked(MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::BLEND_OPAQUE, world, &[], bindless));
        assert!(!marked(both | MeshPipelineKey::from_msaa_samples(4), world, &[], bindless));
        assert!(!marked(MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::NORMAL_PREPASS | MeshPipelineKey::BLEND_ALPHA, world, &[], bindless));
    }

    #[test]
    fn only_a_bindless_main_pipeline_over_coloured_vertices_reads_the_cached_lamps() {
        use bevy::mesh::{MeshAttributeCompressionFlags, MeshVertexAttributeId, MeshVertexBufferLayout, VertexBufferLayout};
        use bevy::render::render_resource::VertexState;
        use std::sync::Arc;
        let marked = |attributes: Vec<MeshVertexAttributeId>, vertex: &[&'static str], fragment: &[&'static str]| {
            let layout = MeshVertexBufferLayoutRef(Arc::new(MeshVertexBufferLayout::new(attributes, VertexBufferLayout::default(), MeshAttributeCompressionFlags::empty())));
            let mut descriptor = RenderPipelineDescriptor {
                vertex: VertexState { shader_defs: vertex.iter().map(|&def| ShaderDefVal::from(def)).collect(), ..Default::default() },
                fragment: Some(FragmentState { shader_defs: fragment.iter().map(|&def| ShaderDefVal::from(def)).collect(), ..Default::default() }),
                ..Default::default()
            };
            mark_cached_lamps(&mut descriptor, &layout);
            descriptor.fragment.expect("a fragment stage").shader_defs.contains(&ShaderDefVal::from("CACHED_LAMPS"))
        };
        let coloured = || vec![Mesh::ATTRIBUTE_POSITION.id, Mesh::ATTRIBUTE_COLOR.id];
        let bindless: &[&str] = &["BINDLESS"];
        assert!(marked(coloured(), &[], bindless));
        assert!(!marked(vec![Mesh::ATTRIBUTE_POSITION.id], &[], bindless));
        assert!(!marked(coloured(), &["PREPASS_PIPELINE"], bindless));
        assert!(!marked(coloured(), &[], &[]));
    }
}
