//! The shown map's suns under the modern look, worked out once per map instead of every frame.

use std::sync::Arc;

use bevy::ecs::prelude::{Commands, Res, Resource};
use bevy::math::Vec3;
use p2_render_cpu::{HardwareLight, MeshLight};

use crate::display::Settings;
use crate::render::actor_light::LightEnvironment;
use crate::render::modern::{fill_suns, sun_color};
use crate::shown_scene::ShownScene;

/// Present while modern graphics is on and a map is shown.
#[derive(Resource)]
pub struct MapLights {
    source: Arc<LightEnvironment>,
    pub(crate) sun: Option<(MeshLight, HardwareLight)>,
    pub(crate) sun_color: [f32; 3],
    pub(crate) fills: Vec<(Vec3, [f32; 3])>,
}

impl MapLights {
    pub fn new(environment: &Arc<LightEnvironment>) -> Self {
        Self { source: environment.clone(), sun: environment.sun, sun_color: sun_color(environment), fills: fill_suns(environment, environment.sun.as_ref()) }
    }

    pub(crate) fn is_for(&self, environment: &Arc<LightEnvironment>) -> bool {
        Arc::ptr_eq(&self.source, environment)
    }
}

pub(crate) fn follow_map_lights(settings: Res<Settings>, shown: Option<Res<ShownScene>>, lights: Option<Res<MapLights>>, mut commands: Commands) {
    let environment = shown.as_deref().and_then(|shown| shown.lighting.as_ref()).filter(|_| settings.modern_graphics());
    match (environment, lights) {
        (Some(environment), Some(lights)) if lights.is_for(environment) => {}
        (Some(environment), _) => commands.insert_resource(MapLights::new(environment)),
        (None, Some(_)) => commands.remove_resource::<MapLights>(),
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2_math::Rotator;
    use p2_render_cpu::LE_SUNLIGHT;
    use rustc_hash::FxHashMap;

    #[test]
    fn map_lights_match_the_sun_and_fills_worked_out_directly() {
        let sun = |brightness: f32, pitch: i32, x: f32| MeshLight {
            effect: LE_SUNLIGHT,
            rotation: Rotator { pitch, yaw: 0, roll: 0 },
            brightness,
            location: p2_math::Vector::new(x, 0.0, 0.0),
            ..MeshLight::default()
        };
        let lights = [sun(128.0, -16384, 1.0), sun(64.0, -16384, 2.0), sun(40.0, -8192, 3.0)];
        let environment = Arc::new(LightEnvironment::new(FxHashMap::from_iter(lights.into_iter().enumerate().map(|(key, light)| (key as i32, light))), Vec::new(), 1.0));
        let map = MapLights::new(&environment);
        assert_eq!(map.sun, environment.sun);
        assert_eq!(map.sun_color, sun_color(&environment));
        assert_eq!(map.fills, fill_suns(&environment, environment.sun.as_ref()));
        assert_eq!(map.fills.len(), 1);
        assert!(map.is_for(&environment));
    }
}
