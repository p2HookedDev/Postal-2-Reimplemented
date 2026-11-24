//! The shown map's suns under the modern look, worked out once per map instead of every frame.

use std::sync::Arc;

use bevy::ecs::prelude::{Commands, Query, Res, Resource};
use bevy::math::Vec3;
use p2_render_cpu::{HardwareLight, MeshLight};

use crate::components::SceneView;
use crate::display::Settings;
use crate::level_collision::LevelCollision;
use crate::render::actor_light::{LightEnvironment, SunSet};
use crate::render::modern::{fill_suns, sun_color};
use crate::shown_scene::ShownScene;

/// Present while modern graphics is on and a map is shown.
#[derive(Resource)]
pub struct MapLights {
    source: Arc<LightEnvironment>,
    /// The zone whose suns these are; `None` for the zone of the map's brightest sun, shown until the eye stands in a
    /// zone with suns of its own.
    zone: Option<u8>,
    pub(crate) sun: Option<(MeshLight, HardwareLight)>,
    pub(crate) sun_color: [f32; 3],
    pub(crate) fills: Vec<(Vec3, [f32; 3])>,
}

impl MapLights {
    pub fn new(environment: &Arc<LightEnvironment>) -> Self {
        Self::of(environment, environment.sun_set(), None)
    }

    fn of(environment: &Arc<LightEnvironment>, set: SunSet, zone: Option<u8>) -> Self {
        Self { source: environment.clone(), zone, sun: set.sun.copied(), sun_color: sun_color(set), fills: fill_suns(set) }
    }

    pub(crate) fn is_for(&self, environment: &Arc<LightEnvironment>) -> bool {
        Arc::ptr_eq(&self.source, environment)
    }
}

/// Retail bakes each zone's suns into that zone's surfaces. Here the suns of the zone the eye stands in light the whole
/// view, and a zone without suns keeps the last ones.
pub(crate) fn follow_map_lights(
    settings: Res<Settings>,
    shown: Option<Res<ShownScene>>,
    level: Res<LevelCollision>,
    views: Query<&SceneView>,
    lights: Option<Res<MapLights>>,
    mut commands: Commands,
) {
    let shown = shown.as_deref().filter(|_| settings.modern_graphics());
    let environment = shown.and_then(|shown| shown.lighting.as_ref());
    match (environment, lights) {
        (Some(environment), Some(lights)) if lights.is_for(environment) => {
            let eye = shown.zip(level.model()).zip(views.iter().next());
            let zone = eye.map(|((shown, model), view)| p2_collision::point_zone(model, p2_bevy_assets::to_bevy(view.position - shown.origin).into()));
            if lights.zone != zone
                && let Some(zone) = zone
                && let Some(set) = environment.zone_sun_set(zone)
            {
                commands.insert_resource(MapLights::of(environment, set, Some(zone)));
            }
        }
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
        assert_eq!(map.sun_color, sun_color(environment.sun_set()));
        assert_eq!(map.fills, fill_suns(environment.sun_set()));
        assert_eq!(map.fills.len(), 1);
        assert!(map.is_for(&environment));
    }
}
