use crate::components::FollowActor;
use crate::level_collision::StaticCollision;
use std::sync::Arc;


use bevy::asset::Handle;
use bevy::color::LinearRgba;
use bevy::ecs::prelude::Component;
use bevy::ecs::message::Message;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::{Quat, Vec3};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::Mesh;
use bevy::transform::components::Transform;
use p2_assets::{Blending, RenderState, UvModifier};

use crate::actor_light::{ActorLit, LightEnvironment};
use crate::components::FollowBone;
use crate::shader_layers::Layers;
use crate::terrain_material::TerrainPassLayer;
use p2_render_cpu::AntiportalActor;
use p2_vm::ObjectId;
use crate::material_class::with_class;
use p2_bevy_assets::material_class::MaterialClass;

pub struct ModelView {
    pub sections: Vec<SectionView>,
    pub instances: Vec<Transform>,
    pub zone: Option<ZoneCulled>,
    pub casts: bool,
}

#[derive(Component)]
pub struct LayeredByZone;

#[derive(Component, Clone)]
pub struct ZoneCulled {
    pub zones: Vec<u8>,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub only_from: Option<Vec<u8>>,
}

pub struct MovingModel<F> {
    pub sections: Vec<SectionView>,
    pub transform: Transform,
    pub follow: F,
    pub lit: Option<ActorLit>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneFog {
    pub color: [u8; 3],
    pub start: f32,
    pub end: f32,
}

pub struct Scene {
    pub models: Vec<ModelView>,
    pub center: Vec3,
    pub radius: f32,
    pub sky: Option<Sky>,
    pub start: Option<(Vec3, i32)>,
    pub collision: Option<StaticCollision>,
    pub fogs: Vec<Option<ZoneFog>>,
    pub decals: Vec<(Handle<Image>, Transform, u32, bool)>,
    pub actors: Vec<MovingModel<FollowActor>>,
    pub attachments: Vec<MovingModel<FollowBone>>,
    pub lighting: Option<Arc<LightEnvironment>>,
    pub light_bakes: Option<Arc<crate::lamps::LightBakes>>,
    pub antiportals: Vec<AntiportalActor>,
    pub light_maps: Vec<Option<Handle<Image>>>,
    pub shadow_surfaces: Arc<[crate::lamps::grid::ShadowSurface]>,
}

/// Placed from the `SkyZoneInfo`'s location, turned by its rotation and then the view's;
/// shows only what lies in that zone (unverified).
pub struct Sky {
    pub zone: u8,
    pub position: Vec3,
    pub rotation: Quat,
    pub models: Vec<ModelView>,
}

#[derive(Message)]
pub enum SceneChange {
    Show(Box<Scene>),
    Clear,
}

#[derive(Message)]
pub struct ZoneFogs(pub Vec<Option<ZoneFog>>);

#[derive(Clone)]
pub struct MaterialImages {
    pub image: Handle<Image>,
    pub glow: Option<Handle<Image>>,
    pub detail: Option<Handle<Image>>,
    pub specular: Option<(Handle<Image>, p2_bevy_assets::library::SpecularParams)>,
    pub state: RenderState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorldPassUse {
    Drawn,
    /// A material switch restyles the surface's material, which the world pass cannot follow.
    CastOnly,
}

#[derive(Clone)]
pub struct SectionView {
    pub mesh: Handle<Mesh>,
    pub texture: Option<Handle<Image>>,
    pub glow: Option<Handle<Image>>,
    pub layers: Layers,
    pub alpha_mode: AlphaMode,
    pub no_scene_lights: bool,
    pub fullbright: bool,
    pub lightmap: Option<Handle<Image>>,
    pub two_sided: bool,
    pub uv: Vec<UvModifier>,
    pub skin: Option<Skin>,
    pub tint: [f32; 4],
    pub alpha_map: Option<Handle<Image>>,
    pub terrain: Vec<TerrainPassLayer>,
    pub chunks: Option<Arc<[crate::world_pass::Chunk]>>,
    pub world_pass: WorldPassUse,
    pub bsp_node: Option<usize>,
    pub terrain_layer: Option<u32>,
}

#[derive(Clone)]
pub struct Skin {
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub bind: Arc<[Transform]>,
}

pub(crate) fn alpha_mode(state: &RenderState) -> AlphaMode {
    let blending = match state.blending.unwrap_or(Blending::Opaque) {
        Blending::Opaque | Blending::Masked if state.color_blend => Blending::Alpha,
        blending => blending,
    };
    match blending {
        Blending::Opaque => AlphaMode::Opaque,
        // Texels pass above the reference: half a step over it keeps an alpha equal to it out.
        Blending::Masked => AlphaMode::Mask((f32::from(state.alpha_ref.unwrap_or(127)) + 0.5) / 255.0),
        Blending::Alpha => AlphaMode::Blend,
        Blending::Additive => AlphaMode::Add,
        Blending::Modulate => AlphaMode::Multiply,
        Blending::Invisible => AlphaMode::Mask(2.0),
    }
}

impl SectionView {
    pub fn textured(mesh: Handle<Mesh>, found: Option<MaterialImages>) -> Self {
        let state = found.as_ref().map(|found| found.state.clone()).unwrap_or_default();
        let alpha_mode = alpha_mode(&state);
        let mut layers = Layers::new(
            found.as_ref().and_then(|found| found.detail.clone()),
            state.detail.map_or(0.0, |detail| detail.scale),
            found.as_ref().and_then(|found| found.specular.clone()),
            state.specular_uv.clone(),
            state.opacity.is_some() && (state.glow.is_some() || state.specular.is_some()),
        );
        layers.params = with_class(layers.params, MaterialClass::from_index(f32::from(state.class)));
        Self {
            mesh,
            glow: found.as_ref().and_then(|found| found.glow.clone()),
            layers: if state.darken { layers.darkening() } else { layers },
            texture: found.map(|found| found.image),
            alpha_mode,
            no_scene_lights: state.unlit,
            fullbright: state.unlit,
            lightmap: None,
            two_sided: state.two_sided,
            uv: state.uv,
            skin: None,
            tint: state.color.map_or([1.0; 4], |color| color.map(|c| (f32::from(c) / 255.0).powf(2.2))),
            alpha_map: None,
            terrain: Vec::new(),
            chunks: None,
            world_pass: WorldPassUse::Drawn,
            bsp_node: None,
            terrain_layer: None,
        }
    }

    pub fn emissive(&self) -> LinearRgba {
        if self.glow.is_some() { LinearRgba::WHITE } else { LinearRgba::BLACK }
    }

    pub fn material_key(&self) -> String {
        format!(
            "{:?} {:?} {} {:?} {} {:?} {} {:?} {:?} {:?} {:?}",
            self.texture.as_ref().map(Handle::id),
            self.glow.as_ref().map(Handle::id),
            self.layers.key(),
            self.alpha_mode,
            self.no_scene_lights,
            self.lightmap.is_some(),
            self.two_sided,
            self.uv,
            self.tint,
            self.alpha_map.as_ref().map(Handle::id),
            self.terrain
        )
    }
}

#[derive(Component, Clone, Copy)]
pub struct SkinJoint {
    pub actor: ObjectId,
    pub bone: usize,
}

pub fn merged_box((min, max): ([f32; 3], [f32; 3]), (low, high): ([f32; 3], [f32; 3])) -> ([f32; 3], [f32; 3]) {
    ([0, 1, 2].map(|k| min[k].min(low[k])), [0, 1, 2].map(|k| max[k].max(high[k])))
}

pub(crate) fn points_box(points: impl IntoIterator<Item = [f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    points.into_iter().fold(None, |bounds, p| Some(merged_box(bounds.unwrap_or((p, p)), (p, p))))
}

pub(crate) fn sky_and_world(zones: &[u8], sky_zone: Option<u8>) -> (bool, bool) {
    let in_sky = sky_zone.is_some_and(|sky| zones.contains(&sky));
    (in_sky, zones.is_empty() || zones.iter().any(|&zone| Some(zone) != sky_zone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_actor_is_drawn_by_each_camera_whose_zones_its_box_touches() {
        assert_eq!(sky_and_world(&[54], Some(54)), (true, false));
        assert_eq!(sky_and_world(&[1, 54], Some(54)), (true, true));
        assert_eq!(sky_and_world(&[1], Some(54)), (false, true));
        assert_eq!(sky_and_world(&[], Some(54)), (false, true));
        assert_eq!(sky_and_world(&[1], None), (false, true));
    }
}
