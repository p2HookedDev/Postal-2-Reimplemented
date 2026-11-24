use bevy::camera::primitives::Aabb;
use bevy::math::Vec3;
use bevy::mesh::Mesh;

use super::sprites::grid_place;
use crate::particle_frame::OCCLUSION_MARGIN;
use super::{ActorDraw, EmitterMesh, MeshWrites};

/// Two vertices no triangle uses: the mesh's bounds, and the centre it is sorted by, become the actor's bounds.
/// Bevy coordinates.
pub fn pad_bounds(mesh: &mut Mesh, min: Vec3, max: Vec3) {
    use bevy::mesh::VertexAttributeValues::{Float32x2, Float32x3, Float32x4};
    if let Some(Float32x3(positions)) = mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
        positions.extend([min.to_array(), max.to_array()]);
    }
    if let Some(Float32x3(normals)) = mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL) {
        normals.extend([[0.0, 1.0, 0.0]; 2]);
    }
    for uvs in [Mesh::ATTRIBUTE_UV_0, Mesh::ATTRIBUTE_UV_1] {
        if let Some(Float32x2(uvs)) = mesh.attribute_mut(uvs) {
            uvs.extend([[0.0; 2]; 2]);
        }
    }
    if let Some(Float32x4(colors)) = mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR) {
        colors.extend([[0.0; 4]; 2]);
    }
}

pub(crate) fn occlusion_aabb(min: Vec3, max: Vec3) -> Aabb {
    Aabb::from_min_max(min - Vec3::splat(OCCLUSION_MARGIN), max + Vec3::splat(OCCLUSION_MARGIN))
}

pub(super) fn apply_bounds(writes: &mut MeshWrites, actor: &mut ActorDraw, changed: bool) {
    if let Some((min, max)) = actor.drawn_bounds {
        for draw in &mut actor.emitters {
            let Some(EmitterMesh { entity, mesh, shown: true, strip_key, written }) = &mut draw.mesh else { continue };
            if draw.grid.is_some() {
                if changed {
                    writes.commands.entity(*entity).insert(grid_place(min, max));
                }
                continue;
            }
            if !std::mem::take(written) {
                // The bounds padded into a strip left as it was are the old ones: it is made again next frame.
                if changed {
                    *strip_key = None;
                }
                continue;
            }
            if let Some(mut mesh) = writes.meshes.get_mut(&*mesh) {
                pad_bounds(&mut mesh, min, max);
            }
            writes.commands.entity(*entity).insert(occlusion_aabb(min, max));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::sprites::fill_sprites;
    use bevy::asset::RenderAssetUsages;
    use bevy::camera::primitives::MeshAabb;
    use bevy::mesh::PrimitiveTopology;
    use p2_particles::SpriteParticle;

    #[test]
    fn padded_emitter_meshes_sort_by_the_actors_centre() {
        let sprite = |x: f32| SpriteParticle {
            location: [x, 0.0, 0.0].into(),
            previous: [x, 0.0, 0.0].into(),
            size: [2.0; 2],
            up: [0.0, 0.0, 2.0].into(),
            right: [0.0, 2.0, 0.0].into(),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
            serial: 0,
        };
        let mut meshes = [sprite(0.0), sprite(100.0)].map(|s| {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            fill_sprites(&mut mesh, &[s]);
            mesh
        });
        let own: Vec<Vec3> = meshes.iter().map(|m| Vec3::from(m.get_aabb().expect("bounds").center)).collect();
        assert_ne!(own[0], own[1]);
        let (min, max) = (Vec3::new(-2.0, -2.0, -2.0), Vec3::new(102.0, 2.0, 2.0));
        for mesh in &mut meshes {
            pad_bounds(mesh, min, max);
            assert_eq!(Vec3::from(mesh.get_aabb().expect("bounds").center), (min + max) / 2.0);
            assert_eq!(mesh.count_vertices(), 6);
            assert!(mesh.indices().expect("indices").iter().all(|i| i < 4), "the padding is drawn");
        }
    }

    #[test]
    fn floor_sprites_are_not_occluded_by_their_floor() {
        let puddle = SpriteParticle {
            location: [10.0, 20.0, -0.3].into(),
            previous: [10.0, 20.0, -0.3].into(),
            size: [30.0; 2],
            up: [30.0, 0.0, 0.0].into(),
            right: [0.0, 30.0, 0.0].into(),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
            serial: 0,
        };
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        fill_sprites(&mut mesh, &[puddle]);
        let own = mesh.get_aabb().expect("bounds");
        let (min, max) = (Vec3::from(own.min()), Vec3::from(own.max()));
        assert_eq!((min.y, max.y), (-0.3, -0.3), "flat, under the floor");
        let culled = occlusion_aabb(min, max);
        assert!(culled.max().y > 1.0, "{culled:?}");
        assert!(Vec3::from(culled.center).abs_diff_eq((min + max) / 2.0, 1e-4), "{culled:?}");
        assert!(culled.min().x < -19.0 && culled.max().z > 49.0, "the puddle's extent in Bevy X / Z: {culled:?}");
    }
}
