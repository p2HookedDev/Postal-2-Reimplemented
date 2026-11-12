use super::*;

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

pub(super) fn header_record(count: usize) -> [u32; RECORD_WORDS] {
    let mut record = [0; RECORD_WORDS];
    record[7] = u32::try_from(count).unwrap_or(u32::MAX);
    record
}

pub(super) fn write_sprite_records(writes: &mut MeshWrites, p: &mut ParticleActor, particles: &[(Vec<StripPoint>, Vec<SpriteParticle>)], drawn: Option<(Vec3, Vec3)>) {
    let grid_at = drawn.map_or(Vec3::ZERO, |(min, max)| (min + max) / 2.0);
    for (draw, (_, sprites)) in p.draws.iter_mut().zip(particles) {
        let (Some(EmitterMesh { entity, shown: true, .. }), Some(grid)) = (&draw.mesh, &mut draw.grid) else { continue };
        let first = u32::try_from(writes.records.len()).unwrap_or(u32::MAX);
        writes.records.push(header_record(sprites.len()));
        writes.records.extend(sprites.iter().map(|sprite| sprite_record(sprite, grid_at)));
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
