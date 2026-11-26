use super::*;

const DRAWN_REACH: f32 = 2.0;

pub(super) fn particles_box(p: &ParticleActor) -> Option<(Vec3, Vec3)> {
    (p.draws.iter().enumerate())
        .filter_map(|(index, draw)| {
            let reach = match (draw.mesh_reach, p.simulation.emitters().get(index)?.props().kind) {
                (Some(reach), _) => reach,
                (None, EmitterKind::Mesh) => return None,
                (None, _) => DRAWN_REACH,
            };
            let (min, max) = p.simulation.bounds(index, reach)?;
            Some((Vec3::from(to_bevy(min)), Vec3::from(to_bevy(max))))
        })
        .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)))
}

pub(super) fn apply_zone(commands: &mut Commands, p: &mut ParticleActor, (leaves, here): (Option<&Model>, Vec3)) {
    if let (Some(model), Some((min, max))) = (leaves, p.bounds)
        && p.zone_box != Some([min, max, here])
    {
        p.zone_box = Some([min, max, here]);
        let (min, max) = (min.min(here), max.max(here));
        let margin = Vec3::splat(OCCLUSION_MARGIN);
        let (min, max) = (to_bevy((min - margin).to_array()), to_bevy((max + margin).to_array()));
        commands.entity(p.parent).insert((ZoneCulled { zones: p2_collision::box_zones(model, min.into(), max.into()), min, max, only_from: None }, LayeredByZone));
    }
}
