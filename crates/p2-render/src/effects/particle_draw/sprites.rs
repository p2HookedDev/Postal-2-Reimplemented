use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::primitives::Aabb;
use bevy::math::{Vec3, Vec3A};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::render::storage::ShaderBuffer;
use bevy::shape::Aabb3d;
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;
use p2_math::Vector;
use p2_particles::SpriteParticle;
use p2_render_cpu::{LightEffect, MeshLight};

use super::bounds::occlusion_aabb;
use super::{ActorDraw, EmitterMesh, MeshWrites};
use crate::effects::particle_frame::{DrawnActor, DrawnEmitter};
use bevy::mesh::MeshTag;
use crate::effects::particle_material::RECORD_WORDS;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SpriteGrid {
    pub(crate) quads: u32,
    pub(crate) first: Option<u32>,
}

pub(crate) const MIN_QUADS: u32 = 16;

const UNLIT: Vec3 = Vec3::NEG_ONE;

pub(super) fn write_sprite_records(writes: &mut MeshWrites, actor: &mut ActorDraw, slot: &DrawnActor, drawn: Option<(Vec3, Vec3)>) {
    let grid_at = drawn.map_or(Vec3::ZERO, |(min, max)| (min + max) / 2.0);
    let [light, lamps_alone] = actor.light.header();
    let lamps = if slot.overlay.is_some() { UNLIT } else { light };
    let seed = slot.puddle_seed.filter(|_| writes.liquid_flow).unwrap_or(0) & SEED_BITS;
    for (draw, DrawnEmitter { sprites, .. }) in actor.emitters.iter_mut().zip(&slot.emitters) {
        let (Some(EmitterMesh { entity, shown: true, .. }), Some(grid)) = (&draw.mesh, &mut draw.grid) else { continue };
        let first = u32::try_from(writes.records.len()).unwrap_or(u32::MAX);
        writes.records.push(header_record(sprites.len(), lamps, lamps_alone));
        let sprites_at = writes.records.len();
        writes.records.extend(sprites.iter().map(|sprite| sprite_record(sprite, grid_at)));
        if seed != 0 {
            writes.records[sprites_at..].iter_mut().for_each(|record| record[11] = seed);
        }
        if grid.first != Some(first) {
            grid.first = Some(first);
            writes.commands.entity(*entity).insert(MeshTag::new(first));
        }
    }
}

const SEED_BITS: u32 = 0x3ff;

#[cfg(test)]
pub fn fill_sprites(mesh: &mut Mesh, sprites: &[SpriteParticle]) {
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

pub(crate) fn sprite_box(sprite: &SpriteParticle) -> (Vec3, Vec3) {
    let (at, reach) = (Vec3::from(to_bevy(sprite.location)), Vec3::from(to_bevy(sprite.up)).abs() + Vec3::from(to_bevy(sprite.right)).abs());
    (at - reach, at + reach)
}

pub(crate) fn sprite_grid(quads: u32) -> Mesh {
    let positions: Vec<[f32; 3]> = (0..quads).flat_map(|quad| (0..4).map(move |corner| [quad as f32, corner as f32, 0.0])).collect();
    let indices = (0..quads).flat_map(|quad| [0, 1, 2, 0, 2, 3].map(|i| quad * 4 + i)).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    // Transparent meshes sort by this centre, so the entity's transform sets the draw order.
    mesh.final_aabb = Some(Aabb3d { min: Vec3A::ZERO, max: Vec3A::ZERO });
    mesh
}

pub(crate) fn grid_place(min: Vec3, max: Vec3) -> (Transform, Aabb) {
    let centre = (min + max) / 2.0;
    (Transform::from_translation(centre), occlusion_aabb(min - centre, max - centre))
}

pub(crate) fn grid_capacity(current: u32, count: usize) -> u32 {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    if count <= current { current } else { count.next_power_of_two().max(MIN_QUADS) }
}

pub(crate) fn sprite_record(sprite: &SpriteParticle, grid_at: Vec3) -> [u32; RECORD_WORDS] {
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

/// In display light, as `particle.wesl` adds it.
pub(crate) fn emitter_light(zone_ambient: Vec3, lamps: Vec3) -> Vec3 {
    zone_ambient + lamps_shown(lamps)
}

pub(crate) fn lamps_shown(lamps: Vec3) -> Vec3 {
    lamps.max(Vec3::ZERO).powf(1.0 / 2.2)
}

pub(crate) fn glow_at<'a>(lights: impl IntoIterator<Item = &'a MeshLight>, location: Vector, level_brightness: f32) -> Vec3 {
    let points = lights.into_iter().filter(|light| light.dynamic && !matches!(light.effect, LightEffect::Sunlight | LightEffect::Spotlight | LightEffect::StaticSpot));
    points
        .map(|light| {
            let a = ((light.location - location).size() / light.world_radius()).min(1.0);
            Vec3::from(light.color(level_brightness)) * (0.5 * (2.0 * a * a * a - 3.0 * a * a + 1.0))
        })
        .sum()
}

pub(crate) fn header_record(count: usize, light: Vec3, lamps: Vec3) -> [u32; RECORD_WORDS] {
    let mut record = [0; RECORD_WORDS];
    record[0..3].copy_from_slice(&light.to_array().map(f32::to_bits));
    record[4..7].copy_from_slice(&lamps.to_array().map(f32::to_bits));
    record[7] = u32::try_from(count).unwrap_or(u32::MAX);
    record
}

pub(crate) fn upload_records<T: bytemuck::NoUninit>(buffers: &mut Assets<ShaderBuffer>, buffer: &Handle<ShaderBuffer>, records: &[T]) {
    let Some(mut buffer) = buffers.get_mut(buffer.id()) else { return };
    let record_bytes = size_of::<T>() as u64;
    let capacity = grid_capacity(u32::try_from(buffer.buffer_size() / record_bytes).unwrap_or(u32::MAX), records.len());
    buffer.clear();
    buffer.extend_from_slice(records);
    buffer.resize_buffer(u64::from(capacity) * record_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::primitives::MeshAabb;
    use p2_particles::DrawStyle;

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
        assert_eq!(glow_at(&[MeshLight { dynamic: false, ..light }, MeshLight { effect: LightEffect::Spotlight, ..light }], Vector::ZERO, 1.0), Vec3::ZERO);
    }

    fn to_linear(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    }

    fn to_display(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
    }

    /// `s` and `d` in display space; the framebuffer blends in linear light.
    fn drawn(style: DrawStyle, s: f32, a: f32, d: f32) -> f32 {
        let (b, dl) = (to_linear(0.25), to_linear(d));
        let over = |source: f32, keep: f32| (to_linear(source + 0.25 * keep) - b * keep).max(0.0);
        let out = match style {
            DrawStyle::Translucent => over(s, 1.0) + dl,
            DrawStyle::Brighten => {
                let f = ((to_linear(s + 0.25 * (1.0 - s)) - b) / (1.0 - b)).max(0.0);
                f + dl * (1.0 - f)
            }
            DrawStyle::AlphaBlend => over(s * a, 1.0 - a) + dl * (1.0 - a),
            DrawStyle::AlphaModulate => over(s, 1.0 - a) + dl * (1.0 - a),
            DrawStyle::Darken => dl * to_linear(1.0 - s),
            DrawStyle::Modulated => 2.0 * ((2.0 * s).powf(2.2) * 0.5).min(1.0) * dl,
            DrawStyle::Regular => to_linear(s),
        };
        to_display(out.min(1.0))
    }

    fn engine(style: DrawStyle, s: f32, a: f32, d: f32) -> f32 {
        let out = match style {
            DrawStyle::Translucent => s + d,
            DrawStyle::Brighten => s + d * (1.0 - s),
            DrawStyle::AlphaBlend => s * a + d * (1.0 - a),
            DrawStyle::AlphaModulate => s + d * (1.0 - a),
            DrawStyle::Darken => d * (1.0 - s),
            DrawStyle::Modulated => 2.0 * s * d,
            DrawStyle::Regular => s,
        };
        out.clamp(0.0, 1.0)
    }

    fn before(s: f32, a: f32, d: f32) -> f32 {
        to_display(to_linear(d) + to_linear(s) * a)
    }

    #[test]
    fn fire_sprites_are_as_bright_as_the_engines() {
        let flame = [1.0, 0.6, 0.2];
        let texture_alpha = 0.4;
        for style in [DrawStyle::Translucent, DrawStyle::Brighten] {
            for d in [0.0, 0.25, 0.35, 0.45, 0.6] {
                let engine_rgb = flame.map(|s| engine(style, s, texture_alpha, d));
                let now_rgb = flame.map(|s| drawn(style, s, texture_alpha, d));
                let before_rgb = flame.map(|s| before(s, texture_alpha, d));
                eprintln!("{style:?} over {d:.2}: engine {engine_rgb:.2?}, now {now_rgb:.2?}, before {before_rgb:.2?}");
                for c in 0..3 {
                    let (now, old) = ((now_rgb[c] - engine_rgb[c]).abs(), (before_rgb[c] - engine_rgb[c]).abs());
                    if d == 0.25 {
                        assert!(now < 0.005, "{style:?} over {d}: {now_rgb:?} vs {engine_rgb:?}");
                    }
                    if d > 0.0 {
                        assert!(now <= old + 1e-4, "{style:?} over {d}, channel {c}: now {now} worse than before {old}");
                    }
                    if (0.25..=0.6).contains(&d) {
                        assert!(now < 0.12, "{style:?} over {d}: {now_rgb:?} vs {engine_rgb:?}");
                    }
                }
            }
        }
        for s in [0.1, 0.5, 0.9] {
            for d in [0.3, 0.5, 0.9] {
                let (now, then) = (drawn(DrawStyle::Darken, s, 1.0, d), engine(DrawStyle::Darken, s, 1.0, d));
                assert!((now - then).abs() < 0.02 || then < 0.1, "darken {s} over {d}: {now} vs {then}");
            }
            for style in [DrawStyle::AlphaBlend, DrawStyle::AlphaModulate] {
                assert!((drawn(style, s, 0.5, 0.25) - engine(style, s, 0.5, 0.25)).abs() < 0.005, "{style:?} {s}");
            }
        }
    }

    #[test]
    fn sprite_boxes_are_the_meshes_bounds() {
        let sprite = |location: Vector, up: Vector, right: Vector| SpriteParticle {
            location,
            previous: location,
            size: [2.0; 2],
            up,
            right,
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
            serial: 0,
        };
        let sprites = [sprite([0.0; 3].into(), [0.0, 0.0, 2.0].into(), [0.0, 2.0, 0.0].into()), sprite([50.0, -20.0, 7.0].into(), [1.0, 1.0, 3.0].into(), [-2.0, 1.5, 0.0].into())];
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        fill_sprites(&mut mesh, &sprites);
        let aabb = mesh.get_aabb().expect("bounds");
        let (min, max) = sprites.iter().map(sprite_box).reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max))).expect("particles");
        assert!(min.abs_diff_eq(Vec3::from(aabb.min()), 1e-4) && max.abs_diff_eq(Vec3::from(aabb.max()), 1e-4), "{min} {max} vs {aabb:?}");
    }

    #[test]
    fn sprite_records_expand_to_the_cpu_quads() {
        use bevy::mesh::VertexAttributeValues::{Float32x2, Float32x3, Float32x4};
        let sprite = |blend| SpriteParticle {
            location: [10.0, -4.0, 7.0].into(),
            previous: [10.0, -4.0, 7.0].into(),
            size: [2.0; 2],
            up: [0.5, 1.0, 3.0].into(),
            right: [-2.0, 1.5, 0.25].into(),
            color: [10, 128, 200, 77],
            uv: [0.25, 0.5, 0.5, 0.75],
            blend,
            serial: 0,
        };
        for sprite in [sprite(None), sprite(Some(([0.5, 0.5, 0.75, 0.75], 200)))] {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            fill_sprites(&mut mesh, std::slice::from_ref(&sprite));
            let record = sprite_record(&sprite, Vec3::ZERO);
            let float = |i: usize| f32::from_bits(record[i]);
            let vec = |i: usize| Vec3::new(float(i), float(i + 1), float(i + 2));
            let (center, up, right) = (vec(0), vec(4), vec(8));
            let color = record[3].to_le_bytes().map(|c| f32::from(c) / 255.0);
            let (Some(Float32x3(positions)), Some(Float32x2(uvs)), Some(Float32x2(next)), Some(Float32x4(colors))) = (
                mesh.attribute(Mesh::ATTRIBUTE_POSITION),
                mesh.attribute(Mesh::ATTRIBUTE_UV_0),
                mesh.attribute(Mesh::ATTRIBUTE_UV_1),
                mesh.attribute(Mesh::ATTRIBUTE_COLOR),
            ) else {
                panic!("attributes")
            };
            for corner in 0..4 {
                let (along_right, along_up) = (corner == 1 || corner == 2, corner >= 2);
                let at = center + if along_up { up } else { -up } + if along_right { right } else { -right };
                assert!(at.abs_diff_eq(Vec3::from(positions[corner]), 1e-5), "corner {corner}: {at} vs {:?}", positions[corner]);
                let rect = |i: usize| [if along_right { float(i + 2) } else { float(i) }, if along_up { float(i + 3) } else { float(i + 1) }];
                assert_eq!(rect(12), uvs[corner], "corner {corner}");
                assert_eq!(rect(16), next[corner], "corner {corner}");
                assert_eq!(color, colors[corner], "corner {corner}");
            }
        }
        assert_eq!(header_record(5, Vec3::ZERO, Vec3::ZERO)[7], 5);
        assert!(header_record(5, Vec3::ZERO, Vec3::ZERO).iter().enumerate().all(|(i, &w)| i == 7 || w == 0));
        let header = header_record(5, Vec3::new(0.5, 1.0, 2.0), Vec3::new(0.25, 0.5, 0.75));
        assert_eq!(header[0..3], [0.5f32, 1.0, 2.0].map(f32::to_bits));
        assert_eq!(header[4..7], [0.25f32, 0.5, 0.75].map(f32::to_bits));
    }

    #[test]
    fn sprite_grids_sort_at_the_actors_box_centre() {
        let parent = Transform::from_translation(Vec3::new(-5000.0, 300.0, 1200.0));
        let grid = sprite_grid(MIN_QUADS);
        assert!(grid.asset_usage.contains(RenderAssetUsages::MAIN_WORLD), "a mesh only the render world keeps has its bounds recomputed from the quad numbers");
        let mesh_centre = Vec3::from(grid.get_aabb().expect("bounds").center);
        assert_eq!(mesh_centre, Vec3::from(sprite_grid(64).get_aabb().expect("bounds").center));
        let location: Vector = [50.0, -20.0, 7.0].into();
        let sprite = SpriteParticle { location, previous: location, size: [2.0; 2], up: [0.0; 3].into(), right: [0.0; 3].into(), color: [255; 4], uv: [0.0; 4], blend: None, serial: 0 };
        let boxes = [(Vec3::new(100.0, -20.0, 7.0), Vec3::new(300.0, 40.0, 9.0)), (Vec3::new(-900.0, 5.0, -60.0), Vec3::new(-850.0, 25.0, -10.0))];
        let mut sort_centres = Vec::new();
        for (min, max) in boxes {
            let (place, aabb) = grid_place(min, max);
            let world_from_local = bevy::transform::components::GlobalTransform::from(parent).mul_transform(place).affine();
            let sort_centre = world_from_local.transform_point3(mesh_centre);
            assert!(sort_centre.abs_diff_eq(parent.transform_point((min + max) / 2.0), 1e-3), "{sort_centre} for {min} {max}");
            sort_centres.push(sort_centre);
            let whole = occlusion_aabb(min, max);
            assert_eq!(place.transform_point(Vec3::from(aabb.center)), Vec3::from(whole.center));
            assert_eq!(aabb.half_extents, whole.half_extents);
            let record = sprite_record(&sprite, (min + max) / 2.0);
            let local = Vec3::new(f32::from_bits(record[0]), f32::from_bits(record[1]), f32::from_bits(record[2]));
            let drawn_at = world_from_local.transform_point3(local);
            assert!(drawn_at.abs_diff_eq(parent.transform_point(Vec3::from(to_bevy(location))), 1e-3), "{drawn_at}");
        }
        assert!(sort_centres[0].distance(sort_centres[1]) > 100.0);
    }

    #[test]
    fn sprite_records_keep_their_place_on_a_moved_grid() {
        let location: Vector = [50.0, -20.0, 7.0].into();
        let sprite = SpriteParticle { location, previous: location, size: [2.0; 2], up: [0.0; 3].into(), right: [0.0; 3].into(), color: [255; 4], uv: [0.0; 4], blend: None, serial: 0 };
        let grid_at = Vec3::new(40.0, 3.0, -9.0);
        let record = sprite_record(&sprite, grid_at);
        let centre = Vec3::new(f32::from_bits(record[0]), f32::from_bits(record[1]), f32::from_bits(record[2]));
        assert_eq!(centre + grid_at, Vec3::from(to_bevy(location)));
    }

    #[test]
    fn the_puddle_seed_is_masked_as_the_shader_reads_it() {
        assert_eq!(crate::gpu::shaders::shader_const(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/particle.wesl")), "SEED_BITS"), f64::from(SEED_BITS));
    }

    #[test]
    fn sprite_grids_grow_by_powers_of_two() {
        assert_eq!(grid_capacity(MIN_QUADS, 3), MIN_QUADS);
        assert_eq!(grid_capacity(MIN_QUADS, 16), 16);
        assert_eq!(grid_capacity(MIN_QUADS, 17), 32);
        assert_eq!(grid_capacity(64, 2), 64);
        assert_eq!(grid_capacity(1, 1), 1);
        assert_eq!(grid_capacity(1, 2), MIN_QUADS);
        assert_eq!(grid_capacity(32, 1000), 1024);
        let grid = sprite_grid(3);
        assert_eq!(grid.count_vertices(), 12);
        assert!(grid.attribute(Mesh::ATTRIBUTE_COLOR).is_none(), "told from CPU meshes by its missing colours");
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) = grid.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!("positions") };
        assert_eq!(positions[9], [2.0, 1.0, 0.0], "quad 2, corner 1");
        let indices: Vec<usize> = grid.indices().expect("indices").iter().collect();
        assert_eq!(indices[12..18], [8, 9, 10, 8, 10, 11]);
    }

    fn shown_frames(blend_between_subdivisions: bool) -> Vec<f32> {
        use bevy::mesh::VertexAttributeValues;
        use p2_particles::{Emitter, EmitterProperties, EmitterSettings, Range};
        let props = EmitterProperties {
            max_particles: 1,
            respawn_dead_particles: false,
            initial_particles_per_second: 0.0,
            particles_per_second: 0.0,
            automatic_initial_spawning: false,
            lifetime_range: Range::new(0.6, 0.6),
            texture_u_subdivisions: 1,
            texture_v_subdivisions: 4,
            blend_between_subdivisions,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
        emitter.spawn_one(0, [0.0; 3].into());
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let mut shown = Vec::new();
        for _ in 0..27 {
            emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
            let sprites: Vec<SpriteParticle> = emitter.sprites(0, p2_particles::View::default()).collect();
            fill_sprites(&mut mesh, &sprites);
            let (
                Some(VertexAttributeValues::Float32x2(uvs)),
                Some(VertexAttributeValues::Float32x2(next)),
                Some(VertexAttributeValues::Float32x4(colors)),
            ) = (mesh.attribute(Mesh::ATTRIBUTE_UV_0), mesh.attribute(Mesh::ATTRIBUTE_UV_1), mesh.attribute(Mesh::ATTRIBUTE_COLOR))
            else {
                panic!("no uvs or colours");
            };
            let frame = |uv: &[f32; 2]| uv[1] / 0.25;
            let blend = if blend_between_subdivisions { colors[0][3] } else { 0.0 };
            shown.push(frame(&uvs[0]) * (1.0 - blend) + frame(&next[0]) * blend);
        }
        shown
    }

    #[test]
    fn flipbook_particles_advance_every_frame() {
        let step = 4.0 / 0.6 / 60.0;
        let blended = shown_frames(true);
        for (frame, pair) in blended.windows(2).enumerate() {
            assert!((pair[1] - pair[0] - step).abs() < 0.01, "frame {frame}: {blended:?}");
        }
        let plain = shown_frames(false);
        assert!(plain.windows(2).any(|pair| pair[1] - pair[0] > 0.9), "{plain:?}");
        assert!(plain.windows(2).filter(|pair| pair[1] == pair[0]).count() > 20, "{plain:?}");
    }
}
