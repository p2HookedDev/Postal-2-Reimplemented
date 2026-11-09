use super::*;

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

/// How far the occlusion-culling `Aabb` reaches past the particle bounds (Unreal units). Flat sprites on a surface
/// (puddles, splats) lie in it or a hair under it, and the culling shader compares the box's nearest depth with the
/// surface's own: from steep angles the floor is in front of the box and the puddle vanishes.
const OCCLUSION_MARGIN: f32 = 8.0;

pub fn occlusion_aabb(min: Vec3, max: Vec3) -> Aabb {
    Aabb::from_min_max(min - Vec3::splat(OCCLUSION_MARGIN), max + Vec3::splat(OCCLUSION_MARGIN))
}

const DRAWN_REACH: f32 = 2.0;

pub(super) fn particles_box(p: &ParticleActor) -> Option<(Vec3, Vec3)> {
    (p.draws.iter().enumerate())
        .filter_map(|(index, draw)| {
            let reach = match (&draw.pool, p.simulation.emitters().get(index)?.props().kind) {
                (Some(pool), _) => pool.reach,
                (None, EmitterKind::Mesh) => return None,
                (None, _) => DRAWN_REACH,
            };
            let (min, max) = p.simulation.bounds(index, reach)?;
            Some((Vec3::from(to_bevy(min)), Vec3::from(to_bevy(max))))
        })
        .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)))
}

pub(super) fn apply_bounds(writes: &mut MeshWrites, p: &mut ParticleActor, changed: bool, (leaves, here): (Option<&Model>, Vec3)) {
    if let Some((min, max)) = p.drawn_bounds {
        for draw in &p.draws {
            let Some(EmitterMesh { entity, mesh, shown: true }) = &draw.mesh else { continue };
            if draw.grid.is_some() {
                if changed {
                    writes.commands.entity(*entity).insert(grid_place(min, max));
                }
                continue;
            }
            if let Some(mut mesh) = writes.meshes.get_mut(mesh) {
                pad_bounds(&mut mesh, min, max);
            }
            writes.commands.entity(*entity).insert(occlusion_aabb(min, max));
        }
    }
    // The parent is at the Unreal origin: the box swapped back is in Unreal space. Grown like the occlusion box: a flat
    // puddle a hair under its floor lay in solid space only, touched no zone and stayed hidden.
    if let (Some(model), Some((min, max))) = (leaves, p.bounds)
        && p.zone_box != Some([min, max, here])
    {
        p.zone_box = Some([min, max, here]);
        let (min, max) = (min.min(here), max.max(here));
        let margin = Vec3::splat(OCCLUSION_MARGIN);
        let (min, max) = (to_bevy((min - margin).to_array()), to_bevy((max + margin).to_array()));
        writes.commands.entity(p.parent).insert((ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None }, LayeredByZone));
    }
}
