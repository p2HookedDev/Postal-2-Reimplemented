use super::*;

fn pour(p: &ParticleActor) -> Option<Pour> {
    let props = p.simulation.emitters().first()?.props();
    let RangeVector { x, y, z } = props.start_velocity_range;
    let relative = props.coordinate_system == CoordinateSystem::Relative;
    let axes = p.turn.owner.rotation.axes();
    let world = |v: Vector| if relative { p2_math::rotate_by(&axes, v) } else { v };
    Some(Pour {
        from: p.turn.place?,
        velocity: [world([x.min, y.min, z.min].into()), world([x.max, y.max, z.max].into())],
        acceleration: world(props.acceleration),
        rate: props.flow_rate(),
        flowing: props.flow_rate() > 0.0,
    })
}

pub(super) fn fill_draw_buffers(p: &ParticleActor, buffers: &mut DrawBuffers, slot: &mut DrawnActor, seen: bool, view: p2_particles::View) {
    let DrawBuffers { strip_order } = buffers;
    slot.parent = p.parent;
    (slot.seen, slot.drawn, slot.overlay, slot.puddle_seed) = (seen, p.drawn, p.overlay, p.puddle_seed);
    slot.place = p.turn.place;
    slot.liquid = p.liquid.zip(p.fluid).map(|(role, kind)| Liquid {
        kind,
        role,
        burning: p.burning,
        pour: matches!(role, LiquidRole::Stream | LiquidRole::Drip).then(|| pour(p)).flatten(),
    });
    slot.emitters.resize_with(p.draws.len(), Default::default);
    for (index, draw) in p.draws.iter().enumerate() {
        let DrawnEmitter { sprites, strip, beams, meshes, shape } = &mut slot.emitters[index];
        if let Some(props) = p.simulation.emitters().get(index).map(|e| e.props()) {
            let facing = (props.use_direction_as != DirectionUsage::None).then(|| Vec3::from_array(props.real_projection_normal.into()).normalize_or_zero());
            *shape = EmitterShape { kind: props.kind, facing, beam_scale: [props.beam.u_scale, props.beam.v_scale], sheets: props.beam.sheets() };
        }
        strip.clear();
        sprites.clear();
        meshes.clear();
        if !seen {
            beams.clear();
            continue;
        }
        match p.simulation.emitters().get(index).map(|e| e.props().kind) {
            Some(EmitterKind::Strip) => p.simulation.strip_into(index, strip_order, strip),
            Some(EmitterKind::Mesh | EmitterKind::Beam) | None => {}
            Some(_) => {
                let Turn { owner, shown, .. } = p.turn;
                let emitter = p.simulation.emitters().get(index);
                let drawn = |s: SpriteParticle| SpriteParticle { location: drawn_location(s.previous, s.location, shown), ..s };
                sprites.extend(emitter.into_iter().flat_map(|e| e.sprites(owner, view)).map(drawn));
            }
        }
        p.simulation.beams_into(index, beams);
        if draw.mesh_reach.is_some() {
            meshes.extend(p.simulation.meshes(index));
        }
    }
    slot.mesh_reach = (slot.emitters.iter().zip(&p.draws))
        .filter_map(|(emitter, draw)| Some((&emitter.meshes, draw.mesh_reach?)))
        .flat_map(|(particles, reach)| particles.iter().map(move |m| mesh_box(m, reach)))
        .reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max)));
}

pub(super) fn drawn_location(previous: Vector, newest: Vector, shown: f32) -> Vector {
    previous + (newest - previous) * shown
}
