//! Video menu's Brightness, Contrast and Gamma: away from identity, 3D cameras render to an image
//! drawn through the ramp. `bevy_egui` input needs window cameras, so HUD and menus are not ramped.

use bevy::app::{App, Plugin, Startup, Update};
use bevy::asset::{Asset, Assets, Handle, RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{Camera, Camera2d, Camera3d, ClearColorConfig, ImageRenderTarget, RenderTarget};
use bevy::color::Color;
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::prelude::{Commands, Or, Query, Res, ResMut, Resource, Single, SystemSet, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::Vec4;
use bevy::shape::Rectangle;
use bevy::mesh::{Mesh, Mesh2d};
use bevy::reflect::TypePath;
use bevy::render::RenderApp;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureFormat, TextureUsages};
use bevy::shader::{Shader, ShaderRef};
use bevy::sprite_render::{Material2d, Material2dPlugin, MeshMaterial2d};
use bevy::window::{PrimaryWindow, Window};

use crate::display::Settings;
use crate::components::CanvasLayer;
use crate::render::shaders::add_shaders;

const GAMMA_SHADER: Handle<Shader> = uuid_handle!("3b8e5a14-6c27-4f0d-9d52-71a4c0e8b6f3");
const GAMMA_LAYER: usize = 3;
const BRIGHTNESS: &str = "ini:Engine.Engine.ViewportManager Brightness";
const CONTRAST: &str = "ini:Engine.Engine.ViewportManager Contrast";
const GAMMA: &str = "ini:Engine.Engine.ViewportManager Gamma";

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GammaSystems {
    Apply,
}

pub struct GammaPlugin;

impl Plugin for GammaPlugin {
    fn build(&self, app: &mut App) {
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        let shader = Shader::from_wgsl(include_str!("gamma.wgsl"), "p2_engine/gamma.wgsl");
        add_shaders(app, [(GAMMA_SHADER, shader)]);
        app.add_plugins(Material2dPlugin::<GammaMaterial>::default())
            .add_systems(Startup, setup)
            .add_systems(Update, apply.in_set(GammaSystems::Apply));
    }
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
struct GammaMaterial {
    #[uniform(0)]
    ramp: Vec4,
    #[texture(1)]
    #[sampler(2)]
    scene: Handle<Image>,
}

impl Material2d for GammaMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(GAMMA_SHADER)
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(GAMMA_SHADER)
    }
}

#[derive(Resource)]
struct Gamma {
    image: Handle<Image>,
    material: Handle<GammaMaterial>,
    camera: bevy::ecs::entity::Entity,
    ramp: Option<[f32; 3]>,
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>, mut materials: ResMut<Assets<GammaMaterial>>, mut meshes: ResMut<Assets<Mesh>>) {
    let mut image = Image::new_target_texture(1, 1, TextureFormat::Rgba8UnormSrgb, None);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let image = images.add(image);
    let material = materials.add(GammaMaterial { ramp: Vec4::new(0.5, 0.5, 1.0, 0.0), scene: image.clone() });
    let layers = RenderLayers::layer(GAMMA_LAYER);
    commands.spawn((Mesh2d(meshes.add(Rectangle::new(2.0, 2.0))), MeshMaterial2d(material.clone()), layers.clone(), NoFrustumCulling));
    // After the HUD's actors and canvas layers (cameras 1 to 8), before the HUD (10).
    let camera = commands
        .spawn((Camera2d, layers, Camera { is_active: false, order: 9, clear_color: ClearColorConfig::Custom(Color::BLACK), ..Default::default() }))
        .id();
    commands.insert_resource(Gamma { image, material, camera, ramp: None });
}

fn apply(
    settings: Res<Settings>,
    mut gamma: ResMut<Gamma>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<GammaMaterial>>,
    mut cameras: Query<&mut RenderTarget, Or<(With<Camera3d>, With<CanvasLayer>)>>,
    mut ramp_camera: Query<&mut Camera>,
) {
    let last = gamma.ramp;
    let [b, c, g] = match last {
        Some(ramp) if !settings.is_changed() => ramp,
        _ => {
            let ramp = [(BRIGHTNESS, 0.5), (CONTRAST, 0.5), (GAMMA, 1.0)].map(|(query, default)| settings.number(query, default));
            gamma.ramp = Some(ramp);
            ramp
        }
    };
    let g = g.max(0.01);
    let active = (b, c, g) != (0.5, 0.5, 1.0);
    if active {
        let size = Extent3d { width: window.physical_width().max(1), height: window.physical_height().max(1), depth_or_array_layers: 1 };
        if images.get(&gamma.image).is_some_and(|image| image.texture_descriptor.size != size)
            && let Some(mut image) = images.get_mut(&gamma.image)
        {
            image.resize(size);
        }
        let ramp = Vec4::new(b, c, g, 0.0);
        if materials.get(&gamma.material).is_some_and(|material| material.ramp != ramp)
            && let Some(mut material) = materials.get_mut(&gamma.material)
        {
            material.ramp = ramp;
        }
    }
    let scale_factor = window.scale_factor();
    for mut target in &mut cameras {
        let rescaled = matches!(&*target, RenderTarget::Image(image) if image.scale_factor != scale_factor);
        if matches!(*target, RenderTarget::Image(_)) != active || rescaled {
            let image = ImageRenderTarget { handle: gamma.image.clone(), scale_factor };
            *target = if active { RenderTarget::Image(image) } else { RenderTarget::default() };
        }
    }
    if let Ok(mut camera) = ramp_camera.get_mut(gamma.camera)
        && camera.is_active != active
    {
        camera.is_active = active;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gamma_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(include_str!("gamma.wgsl")).expect("parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("validates");
    }
}
