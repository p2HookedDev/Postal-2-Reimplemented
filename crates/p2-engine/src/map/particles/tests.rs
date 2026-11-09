use super::*;
use super::sprites::{fill_sprites, header_record, sprite_record};

fn to_linear(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn to_display(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// `s`: the texel × vertex colour in display space; `a`: the alpha; `d`: the framebuffer's display brightness. The
/// framebuffer blends in linear light.
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

/// What the engine shows (D3D blending in display space, saturating).
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
fn padded_emitter_meshes_sort_by_the_actors_centre() {
    let sprite = |x: f32| SpriteParticle {
        location: [x, 0.0, 0.0].into(),
        size: [2.0; 2],
        up: [0.0, 0.0, 2.0].into(),
        right: [0.0, 2.0, 0.0].into(),
        color: [255; 4],
        uv: [0.0, 0.0, 1.0, 1.0],
        blend: None,
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
    assert!(emitter_sort_bias(1) > emitter_sort_bias(0));
}

#[test]
fn sprite_boxes_are_the_meshes_bounds() {
    let sprite = |location: Vector, up: Vector, right: Vector| SpriteParticle {
        location,
        size: [2.0; 2],
        up,
        right,
        color: [255; 4],
        uv: [0.0, 0.0, 1.0, 1.0],
        blend: None,
    };
    let sprites = [sprite([0.0; 3].into(), [0.0, 0.0, 2.0].into(), [0.0, 2.0, 0.0].into()), sprite([50.0, -20.0, 7.0].into(), [1.0, 1.0, 3.0].into(), [-2.0, 1.5, 0.0].into())];
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    fill_sprites(&mut mesh, &sprites);
    let aabb = mesh.get_aabb().expect("bounds");
    let (min, max) = sprites.iter().map(sprite_box).reduce(|(lo, hi), (min, max)| (lo.min(min), hi.max(max))).expect("particles");
    assert!(min.abs_diff_eq(Vec3::from(aabb.min()), 1e-4) && max.abs_diff_eq(Vec3::from(aabb.max()), 1e-4), "{min} {max} vs {aabb:?}");
}

#[test]
fn floor_sprites_are_not_occluded_by_their_floor() {
    let puddle = SpriteParticle {
        location: [10.0, 20.0, -0.3].into(),
        size: [30.0; 2],
        up: [30.0, 0.0, 0.0].into(),
        right: [0.0, 30.0, 0.0].into(),
        color: [255; 4],
        uv: [0.0, 0.0, 1.0, 1.0],
        blend: None,
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

fn shown_frames(blend_between_subdivisions: bool) -> Vec<f32> {
    use bevy::mesh::VertexAttributeValues;
    use p2_particles::{EmitterProperties, EmitterSettings, Range};
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

#[test]
fn sprite_records_expand_to_the_cpu_quads() {
    use bevy::mesh::VertexAttributeValues::{Float32x2, Float32x3, Float32x4};
    let sprite = |blend| SpriteParticle {
        location: [10.0, -4.0, 7.0].into(),
        size: [2.0; 2],
        up: [0.5, 1.0, 3.0].into(),
        right: [-2.0, 1.5, 0.25].into(),
        color: [10, 128, 200, 77],
        uv: [0.25, 0.5, 0.5, 0.75],
        blend,
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
    assert_eq!(header_record(5)[7], 5);
    assert!(header_record(5).iter().enumerate().all(|(i, &w)| i == 7 || w == 0));
}

#[test]
fn scene_targets_queries_do_not_conflict() {
    fn uses_scene_targets(_: SceneTargets) {}
    let mut system = bevy::ecs::system::IntoSystem::into_system(uses_scene_targets);
    bevy::ecs::system::System::initialize(&mut system, &mut bevy::ecs::world::World::new());
}

#[test]
fn sprite_grids_sort_at_the_actors_box_centre() {
    let (min, max) = (Vec3::new(100.0, -20.0, 7.0), Vec3::new(300.0, 40.0, 9.0));
    let (place, aabb) = grid_place(min, max);
    for quads in [MIN_QUADS, 64] {
        let sort_centre = place.transform_point(Vec3::from(sprite_grid(quads).get_aabb().expect("bounds").center));
        assert_eq!(sort_centre, (min + max) / 2.0);
    }
    let whole = occlusion_aabb(min, max);
    assert_eq!(place.transform_point(Vec3::from(aabb.center)), Vec3::from(whole.center));
    assert_eq!(aabb.half_extents, whole.half_extents);
}

#[test]
fn sprite_records_keep_their_place_on_a_moved_grid() {
    let location: Vector = [50.0, -20.0, 7.0].into();
    let sprite = SpriteParticle { location, size: [2.0; 2], up: [0.0; 3].into(), right: [0.0; 3].into(), color: [255; 4], uv: [0.0; 4], blend: None };
    let grid_at = Vec3::new(40.0, 3.0, -9.0);
    let record = sprite_record(&sprite, grid_at);
    let centre = Vec3::new(f32::from_bits(record[0]), f32::from_bits(record[1]), f32::from_bits(record[2]));
    assert_eq!(centre + grid_at, bevy_vector(location));
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
