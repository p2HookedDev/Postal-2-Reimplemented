//! A `bCorona` light is told to the renderer once, and its place and whether the eye's line to it is blocked every
//! frame.

use super::*;
use crate::actors::CORONA_SIZE;
use p2_render::coronas::{CoronaLook, CoronaPlace};

impl Session {
    pub(super) fn add_coronas(&mut self, coronas: Vec<CoronaActor>, origin: Vec3, t: &mut SpawnTargets) {
        if coronas.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for corona in coronas {
            let found = t.spawned.actor_textures.entry(corona.texture.clone()).or_insert_with_key(|path| ActorTexture::read(game, path, |image| t.assets.images.add(image)));
            let Some(ActorTexture { image, .. }) = found.clone() else {
                bevy::log::warn!("corona {:?}: texture {} not loaded, no quad", corona.object, corona.texture);
                continue;
            };
            let CoronaActor { location, rotation, .. } = corona;
            let look = CoronaLook {
                image,
                origin,
                location,
                rotation,
                size: CORONA_SIZE * corona.draw_scale,
                draw_scale3d: corona.draw_scale3d,
                hue: corona.hue,
                saturation: corona.saturation,
                sizes: corona.sizes,
                light_radius: corona.light_radius,
            };
            let parent = t.commands.spawn((MapScoped, ActorModel(corona.object), Transform::from_translation(origin), Visibility::default())).id();
            t.commands.spawn((look, CoronaPlace { gone: false, location, rotation, blocked: false }, Transform::default(), Visibility::Hidden, ChildOf(parent)));
        }
    }
}

/// The VM's trace hits level geometry and movers, not pawns.
pub fn place_coronas(play: Res<Play>, lights: Query<&ActorModel>, mut coronas: Query<(&CoronaLook, &mut CoronaPlace, &ChildOf)>) {
    let world = play.world();
    for (look, mut place, parent) in &mut coronas {
        let Ok(&ActorModel(light)) = lights.get(parent.parent()) else { continue };
        let (gone, (location, rotation)) = world.and_then(|world| Some((world.is_gone(light), world.tick_place(light)?))).unwrap_or((false, (look.location, look.rotation)));
        *place = CoronaPlace { gone, location, rotation, blocked: world.is_some_and(|world| world.corona_blocked(light)) };
    }
}
