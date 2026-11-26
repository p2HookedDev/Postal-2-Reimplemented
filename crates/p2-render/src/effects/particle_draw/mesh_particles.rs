use bevy::asset::{AssetId, Assets, Handle};
use bevy::camera::visibility::Visibility;
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{ChildOf, Commands, Component, Query, With, Without};
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::{Mat4, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::transform::components::Transform;
use p2_bevy_assets::{rotator_matrix, unreal_to_bevy};
use p2_particles::{DrawStyle, EmitterProperties, MeshParticle};
use rustc_hash::FxHashMap;

use crate::world::components::OverlaySlot;
use crate::lighting::lamps::Lamp;
use crate::effects::particle_frame::DrawnActor;
use crate::world::scene::SectionView;
use crate::world::translucent_order::TranslucentDraw;
use crate::world::view::{alpha_key, cull_for};

#[derive(Component)]
pub struct PoolSlot;

#[derive(PartialEq, Eq, Hash)]
pub(super) struct PoolMaterialKey {
    texture: Option<AssetId<Image>>,
    alpha: (std::mem::Discriminant<AlphaMode>, u32),
    two_sided: bool,
}

pub(super) struct MeshPool {
    sections: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    slots: Vec<Vec<Entity>>,
    shown: usize,
}

fn standard_alpha(style: DrawStyle) -> AlphaMode {
    match style {
        DrawStyle::AlphaBlend => AlphaMode::Blend,
        DrawStyle::Modulated | DrawStyle::Darken => AlphaMode::Multiply,
        DrawStyle::Regular => AlphaMode::Mask(0.5),
        DrawStyle::Translucent | DrawStyle::AlphaModulate | DrawStyle::Brighten => AlphaMode::Add,
    }
}

fn mesh_transform(particle: &MeshParticle) -> Transform {
    let turn = rotator_matrix(particle.frame) * rotator_matrix(particle.spin);
    unreal_to_bevy(Mat4::from_translation(Vec3::from_array(particle.location.into())) * turn * Mat4::from_scale(Vec3::from_array(particle.size.into())))
}

impl MeshPool {
    pub(super) fn new(
        sections: &[SectionView],
        props: &EmitterProperties,
        materials: &mut Assets<StandardMaterial>,
        shared: &mut FxHashMap<PoolMaterialKey, Handle<StandardMaterial>>,
    ) -> Self {
        let sections = sections
            .iter()
            .map(|section| {
                let two_sided = section.two_sided || props.render_two_sided;
                let alpha_mode = if props.use_mesh_blend_mode { section.alpha_mode } else { standard_alpha(props.draw_style) };
                let key = PoolMaterialKey { texture: section.texture.as_ref().map(Handle::id), alpha: alpha_key(alpha_mode), two_sided };
                let material = shared.entry(key).or_insert_with(|| {
                    materials.add(StandardMaterial {
                        base_color_texture: section.texture.clone(),
                        alpha_mode,
                        unlit: true,
                        double_sided: two_sided,
                        cull_mode: cull_for(two_sided),
                        ..Default::default()
                    })
                });
                (section.mesh.clone(), material.clone())
            })
            .collect();
        Self { sections, slots: Vec::new(), shown: 0 }
    }

    pub(super) fn slots(&self) -> impl Iterator<Item = Entity> {
        self.slots.iter().flatten().copied()
    }

    pub(super) fn place(&mut self, commands: &mut Commands, placed: &mut Query<&mut Transform, (With<PoolSlot>, Without<Lamp>)>, actor: &DrawnActor, order: u32, particles: &[MeshParticle]) {
        let shown = particles.len();
        while self.slots.len() < shown {
            let layer = OverlaySlot(actor.overlay);
            let at = particles.get(self.slots.len()).map_or_else(Transform::default, mesh_transform);
            let slot: Vec<Entity> = (self.sections.iter())
                .map(|(mesh, material)| {
                    let section = (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), at, Visibility::Hidden, PoolSlot);
                    commands.spawn((section, layer, TranslucentDraw::Part { actor: actor.parent, order }, ChildOf(actor.parent))).id()
                })
                .collect();
            self.slots.push(slot);
        }
        for (index, slot) in self.slots.iter().enumerate() {
            let now = index < shown;
            let visibility = (now != (index < self.shown)).then_some(if now { Visibility::Inherited } else { Visibility::Hidden });
            let transform = particles.get(index).filter(|_| now).map(mesh_transform);
            for &entity in slot {
                if let Some(transform) = transform
                    && let Ok(mut at) = placed.get_mut(entity)
                {
                    at.set_if_neq(transform);
                }
                if let Some(visibility) = visibility {
                    commands.entity(entity).insert(visibility);
                }
            }
        }
        self.shown = shown;
    }
}
