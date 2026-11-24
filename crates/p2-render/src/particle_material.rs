use p2_assets::Blending;
use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Asset, AssetApp, AssetEvent, AssetEventSystems, AssetId, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::mesh::{Mesh, MeshVertexBufferLayoutRef};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::reflect::TypePath;
use bevy::render::RenderApp;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, CompareFunction, DepthBiasState, RenderPipelineDescriptor,
    ShaderType, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::{Shader, ShaderRef};
use p2_particles::{DirectionUsage, DrawStyle};
use p2_vm::ERenderStyle;

use p2_host::display::{MODERN_PARTICLE_LIGHT, MODERN_SOFT_PARTICLES, Settings};
use crate::shaders::{add_libraries, add_shaders};
use crate::lamps::material_added;
use crate::sun_levels::SunLevelAtlas;

const PARTICLE_SHADER: Handle<Shader> = uuid_handle!("6f0c2d6e-8a51-4c4f-9a43-2b7d0f3c5e91");

/// This frame's drawn sprites of every sprite emitter, one buffer every [`ParticleMaterial`] binds: per
/// emitter a header record (its sprite count) then one `sprite_record` (`map/particles.rs`) per sprite. An emitter's
/// entity has the index of its header as its `MeshTag`.
pub(crate) const PARTICLE_SPRITES: Handle<ShaderBuffer> = uuid_handle!("3b8e5f1a-74c2-4d09-b6e1-9a0c2f7d4e53");

/// One record per corona, at its entity's `MeshTag`: the corona's colour in the record's `center`.
pub(crate) const CORONA_COLORS: Handle<ShaderBuffer> = uuid_handle!("9d41c7e2-5b3a-4f86-a0d9-6e2f18b7c304");

pub(crate) const RECORD_WORDS: usize = 20;

const PARTICLE_LEVELS: &str = "P2_PARTICLE_LEVELS";

pub struct ParticlePlugin;

impl Plugin for ParticlePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            app.init_asset::<ParticleMaterial>();
            return;
        }
        let shader = Shader::from_wesl(include_str!("particle.wesl"), "p2_render/particle.wesl");
        add_libraries(app);
        add_shaders(app, [(PARTICLE_SHADER, shader)]);
        let sprites = ShaderBuffer::new(vec![[0u32; RECORD_WORDS]], RenderAssetUsages::default());
        if let Err(error) = app.world_mut().resource_mut::<Assets<ShaderBuffer>>().insert(PARTICLE_SPRITES.id(), sprites) {
            bevy::log::error!("particle sprite buffer: {error}");
        }
        app.add_plugins(MaterialPlugin::<ParticleMaterial>::default());
        app.init_resource::<Settings>().add_systems(PostUpdate, (follow_modern_levels, follow_sun_levels).before(AssetEventSystems));
    }
}

fn follow_sun_levels(
    atlas: Option<Res<SunLevelAtlas>>,
    events: Option<Res<Messages<AssetEvent<ParticleMaterial>>>>,
    mut cursor: Local<MessageCursor<AssetEvent<ParticleMaterial>>>,
    mut known: Local<usize>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
) {
    let Some(atlas) = atlas else { return };
    if !atlas.is_changed() && materials.len() == *known && !material_added(events.as_deref(), &mut cursor) {
        return;
    }
    *known = materials.len();
    let stale: Vec<_> = materials.iter().filter(|(_, material)| material.sun_levels != atlas.image).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut material) = materials.get_mut(id) {
            material.sun_levels.clone_from(&atlas.image);
        }
    }
}

/// World units of fade per level of the soft particles setting.
const SOFT_UNITS_PER_LEVEL: f32 = 6.0;

/// `lit` for a sprite that takes the light of the surface it lies on rather than a puff's.
const LIT_LYING: u32 = 2;

/// The uniform's `soft` (fade depth in world units, 0 off) and `lit` for a material, from the two setting levels.
fn modern_params(soft_level: u8, light_level: u8, material: &ParticleMaterial) -> (f32, u32) {
    let soft = if material.soft_edge { f32::from(soft_level) * SOFT_UNITS_PER_LEVEL } else { 0.0 };
    let lit = match material.light {
        SpriteLight::Puff if light_level > 0 => 1,
        SpriteLight::Lying if light_level > 0 => LIT_LYING,
        _ => 0,
    };
    (soft, lit)
}

/// Faces the normal it was given, so it lies on the surface it was put on: a puddle, a trail.
fn lies_flat(usage: DirectionUsage) -> bool {
    matches!(usage, DirectionUsage::Normal | DirectionUsage::UpAndNormal | DirectionUsage::RightAndNormal)
}

const PUFF_WORDS: [&str; 4] = ["smoke", "dust", "steam", "blood"];

/// Additive and brighten sprites (fire, sparks, flashes) and the multiplying ones are self-lit; covering ones take light.
/// The game also draws its hit puffs, dust, steam and blood spray with brighten: those are told from flames by the
/// texture's name and take light.
fn takes_light(style: DrawStyle, texture: &str) -> bool {
    let texture = texture.to_ascii_lowercase();
    match style {
        DrawStyle::Regular | DrawStyle::AlphaBlend | DrawStyle::AlphaModulate => true,
        DrawStyle::Brighten => PUFF_WORDS.iter().any(|word| texture.contains(word)),
        _ => false,
    }
}

/// How a sprite takes the modern look's light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteLight {
    SelfLit,
    /// The light of a puff in the air, at its emitter.
    Puff,
    /// The light of the surface it lies on.
    Lying,
}

pub(crate) fn sprite_light(style: DrawStyle, texture: &str, usage: DirectionUsage) -> SpriteLight {
    match (takes_light(style, texture), lies_flat(usage)) {
        (false, _) => SpriteLight::SelfLit,
        (true, false) => SpriteLight::Puff,
        (true, true) => SpriteLight::Lying,
    }
}

fn follow_modern_levels(
    settings: Res<Settings>,
    events: Option<Res<Messages<AssetEvent<ParticleMaterial>>>>,
    mut cursor: Local<MessageCursor<AssetEvent<ParticleMaterial>>>,
    mut known: Local<usize>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
) {
    let added: Vec<AssetId<ParticleMaterial>> = events.as_deref().map_or_else(Vec::new, |events| {
        cursor.read(events).filter_map(|event| match *event { AssetEvent::Added { id } => Some(id), _ => None }).collect()
    });
    let (soft_level, light_level) = (settings.modern_level(MODERN_SOFT_PARTICLES), settings.modern_level(MODERN_PARTICLE_LIGHT));
    let wanted = |material: &ParticleMaterial| modern_params(soft_level, light_level, material);
    let stale = |material: &ParticleMaterial| {
        let (soft, lit) = wanted(material);
        (soft.to_bits(), lit) != (material.params.soft.to_bits(), material.params.lit)
    };
    let ids: Vec<_> = if settings.is_changed() || materials.len() != *known {
        *known = materials.len();
        materials.iter().filter(|(_, material)| stale(material)).map(|(id, _)| id).collect()
    } else {
        added.into_iter().filter(|&id| materials.get(id).is_some_and(stale)).collect()
    };
    for id in ids {
        if let Some(mut material) = materials.get_mut(id) {
            (material.params.soft, material.params.lit) = wanted(&material);
        }
    }
}

#[derive(ShaderType, Debug, Clone, Copy, PartialEq)]
pub struct ParticleParams {
    pub style: u32,
    pub alpha_ref: f32,
    pub alpha_test: u32,
    pub blend_frames: u32,
    pub fogged: u32,
    pub soft: f32,
    pub lit: u32,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
#[bind_group_data(ParticleKey)]
pub struct ParticleMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub texture: Handle<Image>,
    #[uniform(2)]
    pub params: ParticleParams,
    #[storage(4, read_only)]
    pub sprites: Handle<ShaderBuffer>,
    #[texture(5)]
    #[sampler(6)]
    pub sun_levels: Option<Handle<Image>>,
    pub style: DrawStyle,
    pub no_depth: bool,
    /// Fades into the level geometry behind it under modern graphics: a sprite facing the eye, which never lies on a surface.
    pub soft_edge: bool,
    pub light: SpriteLight,
    pub sky: bool,
}

/// What two particle materials must share to draw alike.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SharedParticleKey {
    texture: AssetId<Image>,
    params: [u32; 5],
    no_depth: bool,
    soft_edge: bool,
    light: SpriteLight,
    sky: bool,
}

impl ParticleMaterial {
    pub fn shared_key(&self) -> SharedParticleKey {
        let ParticleParams { style, alpha_ref, alpha_test, blend_frames, fogged, .. } = self.params;
        SharedParticleKey {
            texture: self.texture.id(),
            params: [style, alpha_ref.to_bits(), alpha_test, blend_frames, fogged],
            no_depth: self.no_depth,
            soft_edge: self.soft_edge,
            light: self.light,
            sky: self.sky,
        }
    }

    pub fn new(texture: Handle<Image>, props: &p2_particles::EmitterProperties) -> Self {
        let blend = props.blend_between_subdivisions && !props.use_random_subdivision;
        let params = ParticleParams {
            style: props.draw_style as u32,
            alpha_ref: props.alpha_ref.clamp(0, 255) as f32 / 255.0,
            alpha_test: u32::from(props.alpha_test),
            blend_frames: u32::from(blend),
            fogged: u32::from(!props.disable_fogging),
            soft: 0.0,
            lit: 0,
        };
        let soft_edge = props.use_direction_as == DirectionUsage::None && props.draw_style != DrawStyle::Regular;
        Self { texture, params, sprites: PARTICLE_SPRITES, sun_levels: None, style: props.draw_style, no_depth: false, soft_edge, light: sprite_light(props.draw_style, "", props.use_direction_as), sky: false }
    }

    /// Alpha test 127 for masked, ref 0 for alpha blended (both as retail's `FinalBlend`), fogged like the scene (unverified
    /// for sprites).
    pub(crate) fn sprite(texture: Handle<Image>, blend: SpriteBlend) -> Self {
        let (style, alpha_ref, alpha_test) = match blend {
            SpriteBlend::Opaque => (DrawStyle::Regular, 0.0, 0),
            SpriteBlend::Masked => (DrawStyle::Regular, 127.0 / 255.0, 1),
            SpriteBlend::Alpha => (DrawStyle::AlphaBlend, 0.0, 1),
            SpriteBlend::Translucent => (DrawStyle::Translucent, 0.0, 0),
            SpriteBlend::Modulated => (DrawStyle::Modulated, 0.0, 0),
        };
        let params = ParticleParams { style: style as u32, alpha_ref, alpha_test, blend_frames: 0, fogged: 1, soft: 0.0, lit: 0 };
        Self { texture, params, sprites: PARTICLE_SPRITES, sun_levels: None, style, no_depth: false, soft_edge: false, light: SpriteLight::SelfLit, sky: false }
    }

    pub fn corona(texture: Handle<Image>) -> Self {
        let mut material = Self::sprite(texture, SpriteBlend::Translucent);
        material.params.fogged = 0;
        material.no_depth = true;
        material.sprites = CORONA_COLORS;
        material
    }
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpriteBlend {
    Opaque,
    Masked,
    Alpha,
    Translucent,
    Modulated,
}

/// Retail's order. Any other style keeps the previous sprite's shared `FinalBlend` in retail (depends
/// on draw order); here it keeps the start state, `FB_Overwrite` without alpha test.
pub(crate) fn sprite_blend(blending: Option<Blending>, style: Option<ERenderStyle>) -> SpriteBlend {
    if blending == Some(Blending::Masked) || style == Some(ERenderStyle::Masked) {
        SpriteBlend::Masked
    } else if blending == Some(Blending::Alpha) {
        SpriteBlend::Alpha
    } else if style == Some(ERenderStyle::Translucent) {
        SpriteBlend::Translucent
    } else if style == Some(ERenderStyle::Modulated) {
        SpriteBlend::Modulated
    } else {
        SpriteBlend::Opaque
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParticleKey {
    style: DrawStyle,
    no_depth: bool,
    tagged_color: bool,
    sky: bool,
}

impl From<&ParticleMaterial> for ParticleKey {
    fn from(material: &ParticleMaterial) -> Self {
        Self { style: material.style, no_depth: material.no_depth, tagged_color: material.sprites.id() == CORONA_COLORS.id(), sky: material.sky }
    }
}

impl Material for ParticleMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(PARTICLE_SHADER)
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(PARTICLE_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // `sun_levels.wesl` reads this material's bindings 5 and 6 instead of a standard material's slot.
        descriptor.vertex.shader_defs.push(PARTICLE_LEVELS.into());
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push(PARTICLE_LEVELS.into());
        }
        if !layout.0.contains(Mesh::ATTRIBUTE_COLOR) {
            descriptor.vertex.shader_defs.push("PARTICLE_INSTANCES".into());
        }
        if key.bind_group_data.tagged_color {
            descriptor.vertex.shader_defs.push("TAGGED_COLOR".into());
        }
        if key.bind_group_data.sky {
            crate::shaders::sky_view_defs(descriptor);
        }
        // Sprites lying on surfaces are coplanar with them; with another triangulation they z-fought. Pulled towards the camera
        // (reversed Z: up) by a slope bias, which grows at grazing views where they fought.
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.bias = DepthBiasState { constant: PARTICLE_DEPTH_BIAS.0, slope_scale: PARTICLE_DEPTH_BIAS.1, clamp: 0.0 };
            if key.bind_group_data.no_depth {
                depth.depth_compare = CompareFunction::Always.into();
            }
        }
        if let Some(target) = descriptor.fragment.as_mut().and_then(|fragment| fragment.targets.first_mut()).and_then(Option::as_mut) {
            target.blend = Some(particle_blend(key.bind_group_data.style));
        }
        Ok(())
    }
}

/// Constant, slope scale. Measured on the GPU (floor at about 6°): constant bias alone (up to 512) does nothing on the
/// float depth; slope 0.01 to 0.05 hides 0.1 % of a coplanar sprite, and 0.05 shows sprites up to ~0.3 units under the
/// floor (traces' hits are not exact) while one 5 units under stays hidden.
const PARTICLE_DEPTH_BIAS: (i32, f32) = (16, 0.05);

/// The engine blends in display space, the framebuffer
/// here in linear light: `particle.wesl` makes up for it.
pub(crate) fn particle_blend(style: DrawStyle) -> BlendState {
    let color = |src_factor, dst_factor| BlendComponent { src_factor, dst_factor, operation: BlendOperation::Add };
    let keep = color(BlendFactor::Zero, BlendFactor::One);
    match style {
        DrawStyle::Regular => BlendState::REPLACE,
        DrawStyle::AlphaBlend | DrawStyle::AlphaModulate => BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        DrawStyle::Modulated => BlendState { color: color(BlendFactor::Dst, BlendFactor::Src), alpha: keep },
        DrawStyle::Translucent => BlendState { color: color(BlendFactor::One, BlendFactor::One), alpha: keep },
        DrawStyle::Darken => BlendState { color: color(BlendFactor::Zero, BlendFactor::OneMinusSrc), alpha: keep },
        DrawStyle::Brighten => BlendState { color: color(BlendFactor::One, BlendFactor::OneMinusSrc), alpha: keep },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Blending, DirectionUsage, DrawStyle, Handle, LIT_LYING, ParticleMaterial, ERenderStyle, SOFT_UNITS_PER_LEVEL, SpriteBlend, modern_params,
        sprite_blend, takes_light,
    };
    use p2_particles::EmitterProperties;

    #[test]
    fn the_shader_tells_a_lying_sprite_by_the_same_number() {
        assert_eq!(crate::shaders::shader_const(include_str!("particle.wesl"), "LIT_LYING"), f64::from(LIT_LYING));
    }

    fn material(use_direction_as: DirectionUsage, draw_style: DrawStyle) -> ParticleMaterial {
        ParticleMaterial::new(Handle::default(), &EmitterProperties { use_direction_as, draw_style, ..EmitterProperties::default() })
    }

    #[test]
    fn modern_params_are_off_at_level_zero_and_for_exempt_materials() {
        assert_eq!(modern_params(0, 0, &material(DirectionUsage::None, DrawStyle::AlphaBlend)), (0.0, 0));
        assert_eq!(modern_params(5, 1, &material(DirectionUsage::Up, DrawStyle::Translucent)), (0.0, 0));
        assert_eq!(modern_params(5, 1, &material(DirectionUsage::None, DrawStyle::AlphaBlend)), (5.0 * SOFT_UNITS_PER_LEVEL, 1));
        assert_eq!(modern_params(10, 0, &material(DirectionUsage::None, DrawStyle::AlphaBlend)), (10.0 * SOFT_UNITS_PER_LEVEL, 0));
    }

    #[test]
    fn a_sprite_lying_on_a_surface_takes_its_light_and_one_in_the_air_a_puffs() {
        for usage in [DirectionUsage::Normal, DirectionUsage::UpAndNormal, DirectionUsage::RightAndNormal] {
            assert_eq!(modern_params(5, 1, &material(usage, DrawStyle::AlphaBlend)), (0.0, LIT_LYING), "{usage:?}");
            assert_eq!(modern_params(5, 0, &material(usage, DrawStyle::AlphaBlend)), (0.0, 0), "{usage:?}");
        }
        for usage in [DirectionUsage::None, DirectionUsage::Up, DirectionUsage::Right, DirectionUsage::Forward] {
            assert_eq!(modern_params(5, 1, &material(usage, DrawStyle::AlphaBlend)).1, 1, "{usage:?}");
        }
    }

    #[test]
    fn additive_and_multiplying_sprites_stay_self_lit() {
        for style in [DrawStyle::Regular, DrawStyle::AlphaBlend, DrawStyle::AlphaModulate] {
            assert!(takes_light(style, "nathans.Skins.firegroup3"));
        }
        for style in [DrawStyle::Translucent, DrawStyle::Brighten, DrawStyle::Modulated, DrawStyle::Darken] {
            assert!(!takes_light(style, "nathans.Skins.firegroup3"));
        }
    }

    #[test]
    fn a_brighten_puff_takes_light_and_a_brighten_flame_does_not() {
        for texture in ["nathans.Skins.smoke5", "nathans.Skins.wispsmoke", "CigSmoke3", "nathans.Skins.bloodimpacts"] {
            assert!(takes_light(DrawStyle::Brighten, texture), "{texture}");
            assert!(!takes_light(DrawStyle::Translucent, texture), "{texture}");
        }
        for texture in ["nathans.Skins.expl1color", "nathans.Skins.softwhitedot", "nathans.Skins.blast1", ""] {
            assert!(!takes_light(DrawStyle::Brighten, texture), "{texture}");
        }
    }

    #[test]
    fn sprite_blend_follows_retail_order() {
        assert_eq!(sprite_blend(Some(Blending::Masked), Some(ERenderStyle::Translucent)), SpriteBlend::Masked);
        assert_eq!(sprite_blend(Some(Blending::Alpha), Some(ERenderStyle::Masked)), SpriteBlend::Masked);
        assert_eq!(sprite_blend(None, Some(ERenderStyle::Masked)), SpriteBlend::Masked);
        assert_eq!(sprite_blend(Some(Blending::Alpha), Some(ERenderStyle::Translucent)), SpriteBlend::Alpha);
        assert_eq!(sprite_blend(Some(Blending::Alpha), Some(ERenderStyle::Modulated)), SpriteBlend::Alpha);
        assert_eq!(sprite_blend(None, Some(ERenderStyle::Translucent)), SpriteBlend::Translucent);
        assert_eq!(sprite_blend(None, Some(ERenderStyle::Modulated)), SpriteBlend::Modulated);
        for style in [0, 1, 5, 6] {
            assert_eq!(sprite_blend(None, ERenderStyle::from_byte(style)), SpriteBlend::Opaque);
        }
    }
}
