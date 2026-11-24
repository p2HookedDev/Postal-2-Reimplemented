//! Modern graphics: the tessellation box acts on the models already drawn.

use bevy::app::{App, Plugin, Update};
use bevy::asset::Assets;
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::schedule::common_conditions::resource_exists_and_changed;
use bevy::mesh::Mesh;
use p2_host::display::{MODERN_TESSELLATION, Settings};
use p2_render::level::skeletal_draw_meshes;

use super::Animated;

pub struct TessellationPlugin;

impl Plugin for TessellationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, retessellate.run_if(resource_exists_and_changed::<Settings>));
    }
}

/// A model built from now on takes the level where it is built; the ones on screen are built again here, into the
/// mesh assets they already draw.
fn retessellate(settings: Res<Settings>, animated: Res<Animated>, mut meshes: ResMut<Assets<Mesh>>, mut last: Local<Option<u8>>) {
    let level = settings.modern_level(MODERN_TESSELLATION);
    if last.replace(level).is_none_or(|before| before == level) {
        return;
    }
    for item in &animated.0 {
        let gpu_skin = item.meshes.first().and_then(|handle| meshes.get(handle)).is_some_and(|mesh| mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX).is_some());
        if !gpu_skin {
            continue;
        }
        for (handle, (mesh, _)) in item.meshes.iter().zip(skeletal_draw_meshes(&item.mesh, gpu_skin, level)) {
            let _ = meshes.insert(handle.id(), mesh);
        }
    }
}
