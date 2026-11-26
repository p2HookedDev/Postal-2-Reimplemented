use super::*;
use p2_render::effects::sprites::SpriteLook;
use p2_math::{Rotator, Vector};

impl Session {
    pub(super) fn add_sprites(&mut self, sprites: Vec<SpriteActor>, origin: Vec3, t: &mut SpawnTargets) {
        if sprites.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        for sprite in sprites {
            let found = t.spawned.actor_textures.entry(sprite.texture.clone()).or_insert_with_key(|path| ActorTexture::read(game, path, |image| t.assets.images.add(image)));
            let Some(ActorTexture { image, texels, blending }) = found.clone() else { continue };
            let follow = FollowActor { actor: sprite.object, scale: sprite.scale, pre_pivot: Vector::from([0.0; 3]), skeletal: None };
            let frame = t.play.world().and_then(|world| Some((world.actor(sprite.object).is_some_and(|f| f.gone || f.hidden), world.tick_place(sprite.object)?.0)));
            let (hidden, location) = frame.unwrap_or((sprite.hidden, sprite.location));
            let placed = follow.transform(location, Rotator::from([0; 3]));
            let shown = if hidden { Visibility::Hidden } else { Visibility::Inherited };
            let parent = t.commands.spawn((MapScoped, ActorModel(sprite.object), Transform::from_translation(origin), Visibility::default())).id();
            t.commands.spawn((SpriteLook { image, blending, style: sprite.style, texels }, placed, shown, follow, ChildOf(parent)));
        }
    }
}
