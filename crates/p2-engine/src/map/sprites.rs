//! Retail draws a quad of the actor's `Texture` at its `Location`: no `Rotation` or `PrePivot`,
//! white and unlit (no `ScaleGlow` or `AmbientGlow`).

use super::*;
use crate::render::particle_material::sprite_blend;
use p2_math::{Rotator, Vector};

#[derive(bevy::ecs::component::Component, Clone, Copy)]
pub struct SpriteQuad {
    texels: f32,
}

impl Session {
    pub(super) fn add_sprites(&mut self, sprites: Vec<SpriteActor>, origin: Vec3, t: &mut SceneTargets) {
        if sprites.is_empty() {
            return;
        }
        let Some(game) = open_game(&mut self.library, self.map_system.as_deref()) else { return };
        let quad = t.assets.meshes.add(sprite_quad());
        for sprite in sprites {
            let found = self.actor_textures.entry(sprite.texture.clone()).or_insert_with_key(|path| ActorTexture::read(game, path, |image| t.assets.images.add(image)));
            let Some(ActorTexture { image, texels, blending }) = found.clone() else { continue };
            let blend = sprite_blend(blending, sprite.style);
            let material = ParticleMaterial::sprite(image, blend);
            let follow = FollowActor { actor: sprite.object, scale: sprite.scale, pre_pivot: Vector::from([0.0; 3]), skeletal: None };
            let frame = t.play.world().and_then(|world| world.actor(sprite.object)).map(|f| (f.gone || f.hidden, f.location));
            let (hidden, location) = frame.unwrap_or((sprite.hidden, sprite.location));
            let placed = follow.transform(location, Rotator::from([0; 3]));
            let shown = if hidden { Visibility::Hidden } else { Visibility::Inherited };
            let parent = t.commands.spawn((ActorModel(sprite.object), Transform::from_translation(origin), Visibility::default())).id();
            t.commands.spawn((
                Mesh3d(quad.clone()),
                MeshMaterial3d(t.assets.particle_materials.add(material)),
                placed,
                shown,
                follow,
                SpriteQuad { texels },
                ChildOf(parent),
            ));
        }
    }
}

/// Corners are `Location ± X ± Y` with texture coordinates 0 / 1 at their ends. Which camera axis is
/// up is taken from sprites showing upright in the editor (unverified in the binary).
pub(super) fn sprite_quad() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let uvs = vec![[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-0.5f32, 0.5, 0.0], [0.5, 0.5, 0.0], [0.5, -0.5, 0.0], [-0.5, -0.5, 0.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 0.0, 1.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; 4]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

/// Half width `USize · DrawScale · DrawScale3D.X / 2`, half height the same with `.Y`. Both sides take
/// the texture's `USize`, so a non-square texture draws square, as in retail.
pub fn face_sprites(views: Query<&SceneView>, mut sprites: Query<(&SpriteQuad, &FollowActor, &mut Transform)>) {
    let Some(view) = views.iter().next() else { return };
    for (quad, follow, mut transform) in &mut sprites {
        let Vector { x, y, .. } = follow.scale;
        let faced = Transform { translation: transform.translation, rotation: view.rotation, scale: Vec3::new(quad.texels * x, quad.texels * y, 1.0) };
        if *transform != faced {
            *transform = faced;
        }
    }
}
