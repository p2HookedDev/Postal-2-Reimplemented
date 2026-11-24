//! Retail draws a quad of the actor's `Texture` at its `Location`: no `Rotation` or `PrePivot`,
//! white and unlit (no `ScaleGlow` or `AmbientGlow`).

use bevy::app::{App, Plugin, Update};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Add, Commands, Component, On, Query, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::Local;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::MeshMaterial3d;
use bevy::transform::components::Transform;
use p2_assets::Blending;
use p2_math::Vector;
use p2_vm::ERenderStyle;
use crate::components::{FollowActor, SceneView};
use crate::particle_material::{ParticleMaterial, sprite_blend};

#[derive(Component, Clone)]
pub struct SpriteLook {
    pub image: Handle<Image>,
    pub blending: Option<Blending>,
    pub style: Option<ERenderStyle>,
    pub texels: f32,
}

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpriteSystems;

pub struct SpritePlugin;

impl Plugin for SpritePlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(add_sprite_quad).add_systems(Update, face_sprites.in_set(SpriteSystems));
    }
}

fn add_sprite_quad(
    add: On<Add<SpriteLook>>,
    mut commands: Commands,
    looks: Query<&SpriteLook>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    mut quad: Local<Option<Handle<Mesh>>>,
) {
    let entity = add.event().entity;
    let Ok(look) = looks.get(entity) else { return };
    let quad = quad.get_or_insert_with(|| meshes.add(sprite_quad())).clone();
    let material = ParticleMaterial::sprite(look.image.clone(), sprite_blend(look.blending, look.style));
    commands.entity(entity).insert((Mesh3d(quad), MeshMaterial3d(materials.add(material))));
}

/// Corners are `Location ± X ± Y` with texture coordinates 0 / 1 at their ends. Which camera axis is
/// up is taken from sprites showing upright in the editor (unverified in the binary).
pub(crate) fn sprite_quad() -> Mesh {
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
fn face_sprites(views: Query<&SceneView>, mut sprites: Query<(&SpriteLook, &FollowActor, &mut Transform)>) {
    let Some(view) = views.iter().next() else { return };
    for (look, follow, mut transform) in &mut sprites {
        let Vector { x, y, .. } = follow.scale;
        let faced = Transform { translation: transform.translation, rotation: view.rotation, scale: Vec3::new(look.texels * x, look.texels * y, 1.0) };
        if *transform != faced {
            *transform = faced;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Quat;

    #[test]
    fn a_sprite_gets_its_quad_and_turns_to_the_view_scaled_by_texels() {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>().init_resource::<Assets<ParticleMaterial>>().add_plugins(SpritePlugin);
        let rotation = Quat::from_rotation_y(0.7);
        app.world_mut().spawn(SceneView { position: Vec3::ZERO, rotation });
        let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: [2.0, 3.0, 1.0].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let look = SpriteLook { image: Handle::default(), blending: None, style: Some(ERenderStyle::Translucent), texels: 64.0 };
        let sprite = app.world_mut().spawn((look, follow, Transform::from_xyz(1.0, 2.0, 3.0))).id();
        assert!(app.world().entity(sprite).contains::<Mesh3d>());
        app.update();
        let transform = *app.world().entity(sprite).get::<Transform>().expect("a transform");
        assert_eq!(transform, Transform { translation: Vec3::new(1.0, 2.0, 3.0), rotation, scale: Vec3::new(128.0, 192.0, 1.0) });
        assert!(app.world().entity(sprite).contains::<MeshMaterial3d<ParticleMaterial>>());
    }
}
