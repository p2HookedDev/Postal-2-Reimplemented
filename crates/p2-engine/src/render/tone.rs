//! Modern graphics: the world's cameras draw linear light, bloom included, into a half-float image; one full-screen quad
//! draws it through the tone curve into the frame the HUD's actors, canvas layers and the gamma ramp then draw on. The
//! same quad adds the sun's rays, the colour settings, the vignette and the colour fringe.

use bevy::app::{App, Plugin, Startup, Update};
use bevy::asset::{Asset, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{Camera, Camera2d, ClearColorConfig, Hdr};
use bevy::color::Color;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Has, Query, Res, ResMut, Resource, Single, SystemSet, With, Without};
use bevy::light::DirectionalLight;
use bevy::math::{Mat4, Vec2, Vec3, Vec4};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::mesh::{Mesh, Mesh2d};
use bevy::reflect::TypePath;
use bevy::render::RenderApp;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::shader::{Shader, ShaderRef};
use bevy::shape::Rectangle;
use bevy::sprite_render::{Material2d, Material2dPlugin, MeshMaterial2d};
use bevy::transform::components::GlobalTransform;
use bevy::window::{PrimaryWindow, Window};

use crate::components::{CanvasLayer, SceneView, SunShadow};
use crate::display::{
    MODERN_CHROMATIC_ABERRATION, MODERN_CONTRAST, MODERN_EXPOSURE, MODERN_GOD_RAYS, MODERN_SATURATION, MODERN_VIGNETTE, NEUTRAL_LEVEL, Settings,
};
use crate::render::shaders::{add_libraries, add_shaders};

const TONE_SHADER: Handle<Shader> = uuid_handle!("6d1f3a92-5b0e-4c87-a2d4-8e7c19b05f3a");
const TONE_LAYER: usize = 9;
/// After the sky (-2) and the view (-1), before the HUD's actors and canvas layers (1 to 8).
const TONE_ORDER: isize = 0;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToneSystems {
    Apply,
}

pub struct TonePlugin;

impl Plugin for TonePlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_libraries(app);
        add_shaders(app, [(TONE_SHADER,Shader::from_wesl(include_str!("tone.wesl"), "p2_engine/tone.wesl"))]);
        app.add_plugins(Material2dPlugin::<ToneMaterial>::default())
            .add_systems(Startup, setup)
            .add_systems(Update, (apply, follow_effects).in_set(ToneSystems::Apply));
    }
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub(crate) struct ToneMaterial {
    #[texture(0)]
    #[sampler(1)]
    scene: Handle<Image>,
    #[uniform(2)]
    post: ToneEffects,
}

/// As `tone.wesl` reads it.
#[derive(ShaderType, Debug, Clone, Copy, PartialEq)]
pub(crate) struct ToneEffects {
    sun: Vec4,
    tint: Vec4,
    grade: Vec4,
    lens: Vec4,
}

impl Default for ToneEffects {
    fn default() -> Self {
        Self { sun: Vec4::new(0.5, 0.5, 0.0, 1.0), tint: Vec4::ONE, grade: Vec4::new(1.0, 1.0, 1.0, 0.0), lens: Vec4::ZERO }
    }
}

const RAYS_PER_LEVEL: f32 = 0.08;
const VIGNETTE_PER_LEVEL: f32 = 0.05;
const FRINGE_PER_LEVEL: f32 = 0.0005;
const SATURATION_PER_LEVEL: f32 = 0.1;
const CONTRAST_PER_LEVEL: f32 = 0.05;
const EXPOSURE_STOPS_PER_LEVEL: f32 = 0.2;
/// The rays fade in as the view turns towards the sun, over these cosines of the angle between them.
const RAYS_FACING: (f32, f32) = (0.1, 0.45);

/// Where the sun stands on the screen, in uv, and how much of its rays the view's turn leaves. `None` behind the eye.
pub(crate) fn sun_on_screen(clip_from_world: Mat4, forward: Vec3, to_sun: Vec3) -> Option<(Vec2, f32)> {
    let clip = clip_from_world * to_sun.extend(0.0);
    if clip.w <= 0.0 {
        return None;
    }
    let facing = ((forward.dot(to_sun) - RAYS_FACING.0) / (RAYS_FACING.1 - RAYS_FACING.0)).clamp(0.0, 1.0);
    (facing > 0.0).then(|| (Vec2::new(0.5 + 0.5 * clip.x / clip.w, 0.5 - 0.5 * clip.y / clip.w), facing * facing * (3.0 - 2.0 * facing)))
}

fn follow_effects(
    settings: Res<Settings>,
    tone: Res<ToneFrame>,
    window: Single<&Window, With<PrimaryWindow>>,
    views: Query<(&Camera, &GlobalTransform), With<SceneView>>,
    suns: Query<(&DirectionalLight, &GlobalTransform), With<SunShadow>>,
    mut materials: ResMut<Assets<ToneMaterial>>,
) {
    if !settings.modern_graphics() {
        return;
    }
    let level = |query: &str| f32::from(settings.modern_level(query));
    let around_neutral = |query: &str| level(query) - f32::from(NEUTRAL_LEVEL);
    let aspect = window.physical_width().max(1) as f32 / window.physical_height().max(1) as f32;
    let mut wanted = ToneEffects {
        sun: Vec4::new(0.5, 0.5, 0.0, aspect),
        tint: Vec4::ONE,
        grade: Vec4::new(
            (around_neutral(MODERN_EXPOSURE) * EXPOSURE_STOPS_PER_LEVEL).exp2(),
            1.0 + around_neutral(MODERN_SATURATION) * SATURATION_PER_LEVEL,
            1.0 + around_neutral(MODERN_CONTRAST) * CONTRAST_PER_LEVEL,
            level(MODERN_VIGNETTE) * VIGNETTE_PER_LEVEL,
        ),
        lens: Vec4::new(level(MODERN_CHROMATIC_ABERRATION) * FRINGE_PER_LEVEL, 0.0, 0.0, 0.0),
    };
    let rays = level(MODERN_GOD_RAYS) * RAYS_PER_LEVEL;
    let view = views.iter().find(|(camera, _)| camera.is_active);
    let sun = suns.iter().map(|(light, place)| (light.color.to_linear(), place)).find(|(color, _)| color.red + color.green + color.blue > 0.0);
    if rays > 0.0
        && let (Some((camera, eye)), Some((color, place))) = (view, sun)
        && let Some((at, facing)) = sun_on_screen(camera.clip_from_view() * eye.to_matrix().inverse(), eye.forward().as_vec3(), place.back().as_vec3())
    {
        let brightest = color.red.max(color.green).max(color.blue);
        wanted.sun = Vec4::new(at.x, at.y, rays * facing, aspect);
        wanted.tint = Vec4::new(color.red / brightest, color.green / brightest, color.blue / brightest, 1.0);
    }
    if materials.get(&tone.material).is_some_and(|material| material.post != wanted)
        && let Some(mut material) = materials.get_mut(&tone.material)
    {
        material.post = wanted;
    }
}

impl Material2d for ToneMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(TONE_SHADER)
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(TONE_SHADER)
    }
}

#[derive(Component)]
pub struct ToneCamera;

/// The image the world's cameras draw into under modern graphics: window sized while modern is on, else 1 by 1.
#[derive(Resource)]
pub(crate) struct ToneFrame {
    pub(crate) image: Handle<Image>,
    pub(crate) material: Handle<ToneMaterial>,
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>, mut materials: ResMut<Assets<ToneMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let mut image = Image::new_target_texture(1, 1, TextureFormat::Rgba16Float, None);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    // A resize copies the old image into the new one.
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let image = images.add(image);
    let material = materials.add(ToneMaterial { scene: image.clone(), post: ToneEffects::default() });
    let layers = RenderLayers::layer(TONE_LAYER);
    commands.spawn((Mesh2d(meshes.add(Rectangle::new(2.0, 2.0))), MeshMaterial2d(material.clone()), layers.clone(), NoFrustumCulling));
    // Bevy shares a main texture between cameras on one target only while their `Msaa` and `Hdr` agree: these match
    // the canvas layers' so they draw on top of the quad.
    commands.spawn((
        Camera2d,
        ToneCamera,
        layers,
        Msaa::Off,
        Tonemapping::None,
        DebandDither::Disabled,
        Camera { is_active: false, order: TONE_ORDER, clear_color: ClearColorConfig::Custom(Color::BLACK), ..Default::default() },
    ));
    commands.insert_resource(ToneFrame { image, material });
}

#[expect(clippy::too_many_arguments, reason = "one system sizes the world image and follows the cameras around it")]
fn apply(
    mut commands: Commands,
    settings: Res<Settings>,
    tone: Res<ToneFrame>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<ToneMaterial>>,
    views: Query<&Camera, (With<SceneView>, Without<ToneCamera>)>,
    canvases: Query<Has<Hdr>, With<CanvasLayer>>,
    camera: Single<(Entity, &mut Camera, Has<Hdr>), With<ToneCamera>>,
) {
    let modern = settings.modern_graphics();
    let (width, height) = if modern { (window.physical_width().max(1), window.physical_height().max(1)) } else { (1, 1) };
    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    if images.get(&tone.image).is_some_and(|image| image.texture_descriptor.size != size)
        && let Some(mut image) = images.get_mut(&tone.image)
    {
        image.resize(size);
        // A resized image is a new texture: the material is prepared again, or it keeps reading the old one.
        if let Some(mut material) = materials.get_mut(&tone.material) {
            material.scene = tone.image.clone();
        }
    }
    let (entity, mut camera, hdr) = camera.into_inner();
    let active = modern && views.iter().any(|view| view.is_active);
    if camera.is_active != active {
        camera.is_active = active;
    }
    let hdr_wanted = canvases.iter().any(|hdr| hdr);
    if hdr != hdr_wanted {
        if hdr_wanted {
            commands.entity(entity).insert(Hdr);
        } else {
            commands.entity(entity).remove::<Hdr>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_is_placed_on_the_screen_and_fades_as_the_view_turns_away() {
        let clip_from_world = Mat4::from_cols(Vec4::X, Vec4::Y, Vec4::new(0.0, 0.0, 0.0, -1.0), Vec4::new(0.0, 0.0, 0.1, 0.0));
        let (at, facing) = sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::NEG_Z).expect("the sun ahead");
        assert_eq!((at, facing), (Vec2::splat(0.5), 1.0));
        let up_right = Vec3::new(0.3, 0.3, -1.0).normalize();
        let (at, _) = sun_on_screen(clip_from_world, Vec3::NEG_Z, up_right).expect("the sun up and to the right");
        assert!(at.x > 0.5 && at.y < 0.5, "{at}");
        assert_eq!(sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::Z), None);
        assert_eq!(sun_on_screen(clip_from_world, Vec3::NEG_Z, Vec3::new(1.0, 0.0, -0.05).normalize()), None);
    }
}
