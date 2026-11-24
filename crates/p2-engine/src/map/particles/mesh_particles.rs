use super::*;

pub(super) fn mesh_reach(sections: &[SectionView], meshes: &Assets<Mesh>) -> f32 {
    sections
        .iter()
        .filter_map(|section| meshes.get(&section.mesh)?.get_aabb())
        .map(|aabb| Vec3::from(aabb.min()).abs().max(Vec3::from(aabb.max()).abs()).length())
        .fold(0.0, f32::max)
}

pub(super) fn mesh_box(particle: &MeshParticle, reach: f32) -> (Vec3, Vec3) {
    let at = Vec3::from(to_bevy(particle.location));
    let reach = Vec3::splat(reach * <[f32; 3]>::from(particle.size).iter().fold(0.0_f32, |most, size| most.max(size.abs())));
    (at - reach, at + reach)
}
