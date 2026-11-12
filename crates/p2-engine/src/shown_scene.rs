use std::sync::Arc;

use bevy::asset::Handle;
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy::math::Vec3;
use p2_render_cpu::ZonePortals;

use crate::actors::ZoneFog;
use crate::render::actor_light::LightEnvironment;

#[derive(Resource, Default)]
pub struct ShownScene {
    pub(crate) fogs: Vec<Option<ZoneFog>>,
    pub(crate) volume_fog: Option<ZoneFog>,
    pub origin: Vec3,
    pub(crate) portals: Option<Arc<ZonePortals>>,
    pub lighting: Option<Arc<LightEnvironment>>,
    pub radius: f32,
    pub start: Option<(Vec3, i32)>,
    pub light_maps: Vec<Option<Handle<Image>>>,
    pub sky_zone: Option<u8>,
}
