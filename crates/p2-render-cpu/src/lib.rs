#![forbid(unsafe_code)]

mod actor_lights;
pub mod corona;
mod lighting;
mod procedural;
mod zones;

pub use actor_lights::{Walls, leaf_lights, leaves_ambient, relevant_lights};
pub use lighting::{LightEffect, LightType, LightEffect::NonIncidence, LightEffect::Spotlight, LightEffect::StaticSpot, LightEffect::Sunlight, LightType::Steady, LightType::SubtlePulse, MAX_HARDWARE_LIGHTS, SUN_DISTANCE,
    HardwareKind, HardwareLight, LightClock, MeshLight, MeshPlacement, actor_ambient, hardware_ambient, hsv_color, instance_fits,
    light_type_scale, light_world_radius, needs_relight, static_mesh_colors, static_mesh_lighting,
};
pub use procedural::WetSim;
pub use zones::{AntiportalActor, Plane, VisibleZones, ZonePortals};
