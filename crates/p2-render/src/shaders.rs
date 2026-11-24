use bevy::app::{App, Plugin};
use bevy::asset::{AssetApp, Assets, Handle, uuid_handle};
use bevy::pbr::{Material, MaterialPlugin, MeshPipelineKey};
use bevy::render::RenderApp;
use bevy::render::render_resource::RenderPipelineDescriptor;
use bevy::shader::{Shader, ShaderDefVal};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// `folder` beside the exe; a cargo build that has none reads the one in this crate's own sources.
fn beside_exe(folder: &str, in_crate: &str) -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(folder)));
    beside.filter(|dir| dir.is_dir()).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(in_crate))
}

static SHADER_DIR: LazyLock<PathBuf> = LazyLock::new(|| beside_exe("shaders", "src"));

/// A shader read from the shader folder at start-up; WESL imports reach it as `package::p2_render::<stem>`.
pub(crate) fn shader_file(name: &str) -> Shader {
    let file = SHADER_DIR.join(name);
    let source = std::fs::read_to_string(&file).unwrap_or_else(|error| {
        bevy::log::error!("shader {}: {error}", file.display());
        String::new()
    });
    let path = format!("p2_render/{name}");
    if name.ends_with(".wgsl") { Shader::from_wgsl(source, path) } else { Shader::from_wesl(source, path) }
}

pub(crate) fn add_shaders(app: &mut App, shaders: impl IntoIterator<Item = (Handle<Shader>, Shader)>) {
    let mut assets = app.world_mut().resource_mut::<Assets<Shader>>();
    for (handle, shader) in shaders {
        let path = shader.path.clone();
        if let Err(error) = assets.insert(handle.id(), shader) {
            bevy::log::error!("shader {path}: {error}");
        }
    }
}

const DYNAMIC_LIGHT_SHADER: Handle<Shader> = uuid_handle!("e27a4c90-6f1d-4b58-9a3e-d05c8b71f4a2");
const SHADER_LAYERS_SHADER: Handle<Shader> = uuid_handle!("8f4b2d61-93a7-4c0e-b5d8-1a6e7c39f204");
const LAYER_BINDINGS_SHADER: Handle<Shader> = uuid_handle!("c61e0a3f-7d52-4b9e-8f14-3a90d7e2b5c8");
const MODERN_SHADER: Handle<Shader> = uuid_handle!("4e9b7c21-0a6f-4d3e-b852-91c7f0e3a6d4");
const MODERN_BASE_SHADER: Handle<Shader> = uuid_handle!("a3d7f1e8-5c29-4b6a-9e04-7f2b8c61d935");
const RELIEF_SHADER: Handle<Shader> = uuid_handle!("6b1e94c3-d80f-4a57-8c2d-e59a03f7b148");
const SUN_SHADOW_SHADER: Handle<Shader> = uuid_handle!("d48c2a71-39e6-4f0b-a5d3-81c6f29e7b04");
const LAMP_SHADOW_SHADER: Handle<Shader> = uuid_handle!("1f6a8e3d-c472-4b9e-b07a-5d94e2c8a613");
const PROBES_SHADER: Handle<Shader> = uuid_handle!("e9035b7c-6a1d-4e82-9f4b-c27d81a5f360");
const COLOR_SHADER: Handle<Shader> = uuid_handle!("9d3c6a57-2e1b-4f80-a7c4-5b0e8d91f236");
const PROJECTOR_SHADER: Handle<Shader> = uuid_handle!("b7e2f05c-8a31-4d96-9c4b-2f6d1e8a7c53");
const TONE_CURVE_SHADER: Handle<Shader> = uuid_handle!("2c8f6e14-d93a-4b07-8e51-a6b40f7d2c93");
const SCREEN_PASS_SHADER: Handle<Shader> = uuid_handle!("7a5c3e91-2d48-4f6b-9e07-b1c84d2f6a39");
const SURFACE_SHADER: Handle<Shader> = uuid_handle!("5d2a8f47-b13c-4e69-a0d5-c8e71f39b264");
const MATERIAL_LAYERS_SHADER: Handle<Shader> = uuid_handle!("8e41c7d2-5a9f-4b36-a1c8-6f02d3e9b574");
const LAYER_TYPES_SHADER: Handle<Shader> = uuid_handle!("c4f2a917-6e3b-4d08-9b5a-2e71d8f04c63");
const DEPTH_SURFACE_SHADER: Handle<Shader> = uuid_handle!("f05b9d36-2c7e-4a81-b4e9-7d13a6c0e258");
const SUN_LEVELS_SHADER: Handle<Shader> = uuid_handle!("9b2e6d41-c58a-4f17-a3d0-6e8f1b47c925");
const SKY_VIEW_SHADER: Handle<Shader> = uuid_handle!("3e7d1a96-b24c-4f58-8d0e-c71a5f92b630");
const SKY_VERTEX_SHADER: Handle<Shader> = uuid_handle!("6c1f8e24-a937-4b0d-9e52-d84b07a3f619");
const LIQUID_LOOK_SHADER: Handle<Shader> = uuid_handle!("e5a19c37-48d2-4b6f-9c03-7d1e62b8f4a0");

pub(crate) struct ShaderLibraryPlugin;

impl Plugin for ShaderLibraryPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [
            (DYNAMIC_LIGHT_SHADER, shader_file("dynamic_light.wesl")),
            (SHADER_LAYERS_SHADER, shader_file("shader_layers.wesl")),
            (LAYER_BINDINGS_SHADER, shader_file("layer_bindings.wesl")),
            (MODERN_SHADER, shader_file("modern.wesl")),
            (MODERN_BASE_SHADER, shader_file("modern_base.wesl")),
            (RELIEF_SHADER, shader_file("relief.wesl")),
            (SUN_SHADOW_SHADER, shader_file("sun_shadow.wesl")),
            (LAMP_SHADOW_SHADER, shader_file("lamp_shadow.wesl")),
            (crate::reflections::CLASS_PREPASS_SHADER, shader_file("reflections_prepass.wesl")),
            (PROBES_SHADER, shader_file("probes.wesl")),
            (COLOR_SHADER, shader_file("color.wesl")),
            (PROJECTOR_SHADER, shader_file("projector.wesl")),
            (TONE_CURVE_SHADER, shader_file("tone_curve.wesl")),
            (SCREEN_PASS_SHADER, shader_file("screen_pass.wesl")),
            (SURFACE_SHADER, shader_file("surface.wesl")),
            (MATERIAL_LAYERS_SHADER, shader_file("material_layers.wesl")),
            (LAYER_TYPES_SHADER, shader_file("layer_types.wesl")),
            (DEPTH_SURFACE_SHADER, shader_file("depth_surface.wesl")),
            (SUN_LEVELS_SHADER, shader_file("sun_levels.wesl")),
            (SKY_VIEW_SHADER, shader_file("sky_view.wesl")),
            (SKY_VERTEX_SHADER, shader_file("sky_vertex.wesl")),
            (LIQUID_LOOK_SHADER, shader_file("liquid_look.wesl")),
        ]);
    }
}

pub(crate) fn add_libraries(app: &mut App) {
    if !app.is_plugin_added::<ShaderLibraryPlugin>() {
        app.add_plugins(ShaderLibraryPlugin);
    }
}

pub(crate) fn add_material<M: Material>(app: &mut App, handle: Handle<Shader>, name: &str)
where
    MaterialPlugin<M>: Plugin,
{
    if app.get_sub_app(RenderApp).is_none() {
        app.init_asset::<M>();
        return;
    }
    add_libraries(app);
    add_shaders(app, [(handle, shader_file(name))]);
    app.add_plugins(MaterialPlugin::<M>::default());
}

/// The value of `const name` in a shader's source, for a test that holds a Rust constant to the shader's twin of it.
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

/// Whether a section material draws the sky zone, and whether it hands its pools to the screen reflections.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkyKey {
    pub sky: bool,
    pub mirror: bool,
}

pub(crate) fn mark_sky(descriptor: &mut RenderPipelineDescriptor, key: SkyKey) {
    if key.sky {
        sky_vertex(descriptor);
    }
}

/// A sky material's vertex stage squeezes it behind the level and draws nothing in a prepass.
pub(crate) fn sky_vertex(descriptor: &mut RenderPipelineDescriptor) {
    let prepass = descriptor.vertex.shader_defs.contains(&ShaderDefVal::from("PREPASS_PIPELINE"));
    descriptor.vertex.shader = SKY_VERTEX_SHADER;
    descriptor.vertex.entry_point = Some(if prepass { "prepass_vertex" } else { "vertex" }.into());
    sky_view_defs(descriptor);
}

/// The sky view's shader def; the sky takes no scene fog.
pub(crate) fn sky_view_defs(descriptor: &mut RenderPipelineDescriptor) {
    descriptor.vertex.shader_defs.push("SKY_VIEW".into());
    if let Some(fragment) = descriptor.fragment.as_mut() {
        fragment.shader_defs.retain(|def| !matches!(def, ShaderDefVal::Bool(name, _) if name == "DISTANCE_FOG"));
        fragment.shader_defs.push("SKY_VIEW".into());
    }
}

/// Bevy gives an alpha-blend pipeline no def of its own; the lit surfaces' see-through path is compiled into those only.
pub(crate) fn mark_see_through(descriptor: &mut RenderPipelineDescriptor, key: MeshPipelineKey) {
    if key.intersection(MeshPipelineKey::BLEND_RESERVED_BITS) == MeshPipelineKey::BLEND_ALPHA
        && let Some(fragment) = descriptor.fragment.as_mut()
    {
        fragment.shader_defs.push("P2_SEE_THROUGH".into());
    }
}

/// The pool record rides in the frame's alpha, which any blend (or alpha to coverage) would misread, and the sky's alpha
/// of 0 is the sun-ray mask; so only opaque and masked pipelines of the world write it, and only in views without MSAA,
/// the only ones the reflection pass runs in to put the alpha back.
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

/// Bevy's shadow pass takes the material's cull mode, and a level's faces look into its empty space: from the sun's side
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
    fn only_an_unblended_world_pipeline_with_the_flag_writes_the_pool_record() {
        let marked = |mesh_key: MeshPipelineKey, key: SkyKey| {
            let mut descriptor = RenderPipelineDescriptor { fragment: Some(FragmentState::default()), ..Default::default() };
            mark_pool_mirror(&mut descriptor, mesh_key, key);
            descriptor.fragment.expect("a fragment stage").shader_defs.contains(&ShaderDefVal::from("POOL_MIRROR"))
        };
        let world = SkyKey { sky: false, mirror: true };
        assert!(marked(MeshPipelineKey::BLEND_OPAQUE, world));
        assert!(marked(MeshPipelineKey::BLEND_OPAQUE | MeshPipelineKey::MAY_DISCARD, world));
        assert!(!marked(MeshPipelineKey::BLEND_OPAQUE, SkyKey { sky: false, mirror: false }));
        assert!(!marked(MeshPipelineKey::BLEND_OPAQUE, SkyKey { sky: true, mirror: true }));
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
}
