use super::*;
use p2_render_cpu::{LE_SPOTLIGHT, LE_STATIC_SPOT, LE_SUNLIGHT, MeshLight};

#[derive(Clone, Copy, Debug)]
pub(super) struct SpriteGrid {
    pub(super) quads: u32,
    pub(super) first: Option<u32>,
}

pub(super) const MIN_QUADS: u32 = 16;

pub(super) fn standard_alpha(style: DrawStyle) -> AlphaMode {
    match style {
        DrawStyle::AlphaBlend => AlphaMode::Blend,
        DrawStyle::Modulated | DrawStyle::Darken => AlphaMode::Multiply,
        DrawStyle::Regular => AlphaMode::Mask(0.5),
        DrawStyle::Translucent | DrawStyle::AlphaModulate | DrawStyle::Brighten => AlphaMode::Add,
    }
}

#[cfg(test)]
pub(crate) fn fill_sprites(mesh: &mut Mesh, sprites: &[SpriteParticle]) {
    let mut positions = Vec::with_capacity(sprites.len() * 4);
    let mut normals = Vec::with_capacity(sprites.len() * 4);
    let mut uvs = Vec::with_capacity(sprites.len() * 4);
    let mut next_uvs = Vec::with_capacity(sprites.len() * 4);
    let mut colors = Vec::with_capacity(sprites.len() * 4);
    let mut indices = Vec::with_capacity(sprites.len() * 6);
    for sprite in sprites {
        let center = Vec3::from(to_bevy(sprite.location));
        let (up, right) = (Vec3::from(to_bevy(sprite.up)), Vec3::from(to_bevy(sprite.right)));
        let [red, green, blue, alpha] = sprite.color.map(|c| f32::from(c) / 255.0);
        let ([u0, v0, u1, v1], [n0, m0, n1, m1], alpha) = match sprite.blend {
            Some((next, factor)) => (sprite.uv, next, f32::from(factor) / 255.0),
            None => (sprite.uv, sprite.uv, alpha),
        };
        let base = positions.len() as u32;
        positions.extend([center - up - right, center - up + right, center + up + right, center + up - right].map(|corner| corner.to_array()));
        normals.extend([up.cross(right).normalize_or_zero().to_array(); 4]);
        uvs.extend([[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
        next_uvs.extend([[n0, m0], [n1, m0], [n1, m1], [n0, m1]]);
        colors.extend([[red, green, blue, alpha]; 4]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    if positions.is_empty() {
        positions.extend([[0.0; 3]; 4]);
        normals.extend([[0.0, 1.0, 0.0]; 4]);
        uvs.extend([[0.0; 2]; 4]);
        next_uvs.extend([[0.0; 2]; 4]);
        colors.extend([[0.0; 4]; 4]);
        indices.extend([0, 1, 2, 0, 2, 3]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, next_uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

pub(super) fn fill_draw_buffers(p: &ParticleActor, buffers: &mut DrawBuffers, seen: bool, view: p2_particles::View) {
    let DrawBuffers { particles, beams, meshes, strip_order } = buffers;
    particles.resize_with(p.draws.len(), Default::default);
    beams.resize_with(p.draws.len(), Vec::new);
    meshes.resize_with(p.draws.len(), Default::default);
    for (index, draw) in p.draws.iter().enumerate() {
        let (strip, sprites) = &mut particles[index];
        let (mesh_particles, reach) = &mut meshes[index];
        strip.clear();
        sprites.clear();
        mesh_particles.clear();
        *reach = 0.0;
        if !seen {
            beams[index].clear();
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
        p.simulation.beams_into(index, &mut beams[index]);
        if let Some(pool) = &draw.pool {
            mesh_particles.extend(p.simulation.meshes(index));
            *reach = pool.reach;
        }
    }
}

/// `shown` 1 is the newest tick's place and 0 the one before. Below 0 the sprite is taken back along its last step, to
/// the time the world is shown at: the simulation keeps no older place.
pub(super) fn drawn_location(previous: Vector, newest: Vector, shown: f32) -> Vector {
    previous + (newest - previous) * shown
}

pub(super) fn sprite_box(sprite: &SpriteParticle) -> (Vec3, Vec3) {
    let (at, reach) = (Vec3::from(to_bevy(sprite.location)), Vec3::from(to_bevy(sprite.up)).abs() + Vec3::from(to_bevy(sprite.right)).abs());
    (at - reach, at + reach)
}

pub(super) fn sprite_grid(quads: u32) -> Mesh {
    let positions: Vec<[f32; 3]> = (0..quads).flat_map(|quad| (0..4).map(move |corner| [quad as f32, corner as f32, 0.0])).collect();
    let indices = (0..quads).flat_map(|quad| [0, 1, 2, 0, 2, 3].map(|i| quad * 4 + i)).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    // Transparent meshes sort by this centre, so the entity's transform sets the draw order.
    mesh.final_aabb = Some(Aabb3d { min: Vec3A::ZERO, max: Vec3A::ZERO });
    mesh
}

pub(super) fn grid_place(min: Vec3, max: Vec3) -> (Transform, Aabb) {
    let centre = (min + max) / 2.0;
    (Transform::from_translation(centre), occlusion_aabb(min - centre, max - centre))
}

pub(super) fn grid_capacity(current: u32, count: usize) -> u32 {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    if count <= current { current } else { count.next_power_of_two().max(MIN_QUADS) }
}

/// Fills `SpriteRecord` in `particle.wesl`, with the sprite's position relative to `grid_at`.
pub(super) fn sprite_record(sprite: &SpriteParticle, grid_at: Vec3) -> [u32; RECORD_WORDS] {
    let [r, g, b, a] = sprite.color;
    let (next, alpha) = sprite.blend.unwrap_or((sprite.uv, a));
    let mut record = [0; RECORD_WORDS];
    let floats = |v: Vector| to_bevy(v).map(f32::to_bits);
    record[0..3].copy_from_slice(&(Vec3::from(to_bevy(sprite.location)) - grid_at).to_array().map(f32::to_bits));
    record[3] = u32::from_le_bytes([r, g, b, alpha]);
    record[4..7].copy_from_slice(&floats(sprite.up));
    record[8..11].copy_from_slice(&floats(sprite.right));
    record[12..16].copy_from_slice(&sprite.uv.map(f32::to_bits));
    record[16..20].copy_from_slice(&next.map(f32::to_bits));
    record
}

/// Marks an emitter drawn by a view that is not the scene's: unlit, as retail draws every sprite.
const UNLIT: Vec3 = Vec3::NEG_ONE;

/// In display light, as `particle.wesl` adds it: a lamp lights a puff in its reach through and through.
pub(super) fn emitter_light(zone_ambient: Vec3, lamps: Vec3) -> Vec3 {
    zone_ambient + lamps_shown(lamps)
}

pub(super) fn lamps_shown(lamps: Vec3) -> Vec3 {
    lamps.max(Vec3::ZERO).powf(1.0 / 2.2)
}

/// A puff's share of the script lights around it (a fire, a gun flash), in display light: their falloff with no facing,
/// at half of what a surface facing them takes.
pub(super) fn glow_at(lights: &[MeshLight], location: Vector, level_brightness: f32) -> Vec3 {
    let points = lights.iter().filter(|light| light.dynamic && !matches!(light.effect, LE_SUNLIGHT | LE_SPOTLIGHT | LE_STATIC_SPOT));
    points
        .map(|light| {
            let a = ((light.location - location).size() / light.world_radius()).min(1.0);
            Vec3::from(light.color(level_brightness)) * (0.5 * (2.0 * a * a * a - 3.0 * a * a + 1.0))
        })
        .sum()
}

/// The emitter's sprite count, in the place of a sprite's centre its `emitter_light` and of its up axis its
/// `lamps_shown`, which a sprite lying on a surface takes in place of the zone's ambient.
pub(super) fn header_record(count: usize, light: Vec3, lamps: Vec3) -> [u32; RECORD_WORDS] {
    let mut record = [0; RECORD_WORDS];
    record[0..3].copy_from_slice(&light.to_array().map(f32::to_bits));
    record[4..7].copy_from_slice(&lamps.to_array().map(f32::to_bits));
    record[7] = u32::try_from(count).unwrap_or(u32::MAX);
    record
}

pub(super) fn write_sprite_records(writes: &mut MeshWrites, p: &mut ParticleActor, particles: &[(Vec<StripPoint>, Vec<SpriteParticle>)], drawn: Option<(Vec3, Vec3)>) {
    let grid_at = drawn.map_or(Vec3::ZERO, |(min, max)| (min + max) / 2.0);
    let lamps = if p.overlay.is_some() { UNLIT } else { p.lamp_light + p.fire_light };
    let puddle_seed = p.puddle_seed.filter(|_| writes.liquid_flow);
    for (draw, (_, sprites)) in p.draws.iter_mut().zip(particles) {
        let (Some(EmitterMesh { entity, shown: true, .. }), Some(grid)) = (&draw.mesh, &mut draw.grid) else { continue };
        let first = u32::try_from(writes.records.len()).unwrap_or(u32::MAX);
        writes.records.push(header_record(sprites.len(), lamps, p.lamps_alone + p.fire_light));
        let sprites_at = writes.records.len();
        writes.records.extend(sprites.iter().map(|sprite| sprite_record(sprite, grid_at)));
        if let Some(seed) = puddle_seed {
            // The record's spare word: a modern puddle's edge seed, 0 for every other sprite.
            writes.records[sprites_at..].iter_mut().for_each(|record| record[11] = seed);
        }
        if grid.first != Some(first) {
            grid.first = Some(first);
            writes.commands.entity(*entity).insert(MeshTag::new(first));
        }
    }
}

pub(super) fn upload_sprites(buffers: &mut Assets<ShaderBuffer>, records: &[[u32; RECORD_WORDS]]) {
    let Some(mut buffer) = buffers.get_mut(PARTICLE_SPRITES.id()) else { return };
    let record_bytes = (RECORD_WORDS * 4) as u64;
    let capacity = grid_capacity(u32::try_from(buffer.buffer_size() / record_bytes).unwrap_or(u32::MAX), records.len());
    buffer.clear();
    buffer.extend_from_slice(records);
    buffer.resize_buffer(u64::from(capacity) * record_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_of_ordinary_light_draws_a_lit_sprite_near_retail() {
        let light = emitter_light(Vec3::splat(0.25), Vec3::splat(0.5));
        assert!((light - Vec3::splat(0.98)).abs().max_element() < 0.01, "{light}");
        assert_eq!(emitter_light(Vec3::splat(0.25), Vec3::splat(-1.0)), Vec3::splat(0.25));
    }

    #[test]
    fn a_puff_takes_half_a_script_light_at_it_and_none_at_its_radius() {
        let light = MeshLight { radius: 3.0, brightness: 255.0, saturation: 255, dynamic: true, ..MeshLight::default() };
        let full = Vec3::from(light.color(1.0));
        assert_eq!(glow_at(&[light], Vector::ZERO, 1.0), full * 0.5);
        assert_eq!(glow_at(&[light], Vector::new(50.0, 0.0, 0.0), 1.0), full * 0.25);
        assert_eq!(glow_at(&[light], Vector::new(100.0, 0.0, 0.0), 1.0), Vec3::ZERO);
        assert_eq!(glow_at(&[light], Vector::new(500.0, 0.0, 0.0), 1.0), Vec3::ZERO);
        assert_eq!(glow_at(&[MeshLight { dynamic: false, ..light }, MeshLight { effect: LE_SPOTLIGHT, ..light }], Vector::ZERO, 1.0), Vec3::ZERO);
    }
}
