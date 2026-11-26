use std::sync::Arc;

use bevy::asset::Handle;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::Vec3;
use p2_render_cpu::ZonePortals;

use crate::world::scene::ZoneFog;
use crate::lighting::actor_light::LightEnvironment;

#[derive(Resource, Default)]
pub struct ShownScene {
    pub fogs: Vec<Option<ZoneFog>>,
    pub origin: Vec3,
    pub portals: Option<Arc<ZonePortals>>,
    pub lighting: Option<Arc<LightEnvironment>>,
    pub light_bakes: Option<Arc<crate::lighting::lamps::LightBakes>>,
    pub start: Option<(Vec3, i32)>,
    pub light_maps: Vec<Option<Handle<Image>>>,
    pub sky_zone: Option<u8>,
    pub sky_place: Option<(Vec3, bevy::math::Quat)>,
    pub shadow_surfaces: Arc<[crate::lighting::lamps::grid::ShadowSurface]>,
}
