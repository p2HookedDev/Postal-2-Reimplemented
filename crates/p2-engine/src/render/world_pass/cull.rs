use super::*;
use crate::render::sun_cache::CachedSunLevel;

#[derive(Clone, Copy)]
pub(super) enum Planes {
    Frustum,
    /// A sun's cascade: what lies before its near plane still casts, as its depth is clamped.
    Sides,
}

/// `Vec4(normal, d)`: a point is inside at `dot(normal, p) + d >= 0`. An unused plane is (0, 0, 0, 1), always inside.
pub(super) fn view_planes(clip_from_world: Mat4, planes: Planes) -> [Vec4; 6] {
    let row = |index: usize| clip_from_world.row(index);
    let plane = |raw: Vec4| {
        let length = raw.truncate().length();
        if length > 1e-6 { raw / length } else { Vec4::W }
    };
    let sides = [plane(row(3) + row(0)), plane(row(3) - row(0)), plane(row(3) + row(1)), plane(row(3) - row(1))];
    let (near, far) = match planes {
        Planes::Frustum => (plane(row(3) - row(2)), plane(row(2))),
        Planes::Sides => (Vec4::W, Vec4::W),
    };
    [sides[0], sides[1], sides[2], sides[3], near, far]
}

#[derive(Clone, Copy)]
enum Chunks {
    All,
    CastersOnly,
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn prepare_world_views(
    mut commands: Commands,
    mut pass: ResMut<WorldPass>,
    queue: Res<RenderQueue>,
    cameras: Query<(Entity, &ExtractedView, Option<&WorldCullSlot>), With<WorldCamera>>,
    lights: Query<(Entity, &ExtractedView, &LightEntity, Option<&WorldCullSlot>), With<ShadowView>>,
    levels: Query<&CachedSunLevel>,
    seen_zones: Option<Res<WorldSeenZones>>,
    casts: Option<Res<WorldCasts>>,
) {
    let Some(built) = pass.built.as_mut() else { return };
    let seen_zones = seen_zones.map_or(WorldSeenZones::default().0, |seen| seen.0);
    let clip_from_world = |view: &ExtractedView| view.clip_from_world.unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());
    let mut bytes = Vec::with_capacity(MAX_VIEWS * VIEW_STRIDE as usize);
    let mut slot = 0u32;
    let mut add = |entity: Entity, planes: [Vec4; 6], chunks: Chunks, zones: [u32; 8], had: Option<&WorldCullSlot>| {
        if slot as usize >= MAX_VIEWS {
            return;
        }
        let start = bytes.len();
        bytes.extend(planes.iter().flat_map(|plane| plane.to_array()).flat_map(|value: f32| value.to_le_bytes()));
        push_u32s(&mut bytes, &[slot * built.chunk_count, slot * built.buckets.len() as u32, u32::from(matches!(chunks, Chunks::CastersOnly)), 0]);
        push_u32s(&mut bytes, &zones);
        bytes.resize(start + VIEW_STRIDE as usize, 0);
        if had != Some(&WorldCullSlot(slot)) {
            commands.entity(entity).insert(WorldCullSlot(slot));
        }
        slot += 1;
    };
    for (entity, view, had) in &cameras {
        add(entity, view_planes(clip_from_world(view), Planes::Frustum), Chunks::All, seen_zones, had);
    }
    if casts.is_some_and(|casts| casts.0) {
        for (entity, view, light, had) in &lights {
            if let LightEntity::Directional { light_entity, .. } = light
                && levels.get(*light_entity).is_ok_and(CachedSunLevel::drawn_now)
            {
                add(entity, view_planes(clip_from_world(view), Planes::Sides), Chunks::CastersOnly, [u32::MAX; 8], had);
            }
        }
    }
    built.live_views = slot;
    if !bytes.is_empty() {
        queue.write_buffer(&built.views, 0, &bytes);
    }
}

pub(super) fn cull_world(_view: ViewQuery<Entity, With<WorldCamera>>, pass: Res<WorldPass>, cache: Res<PipelineCache>, mut context: RenderContext) {
    let (Some(built), Some((_, pipeline))) = (&pass.built, &pass.pipeline) else { return };
    let Some(pipeline) = cache.get_compute_pipeline(*pipeline) else { return };
    let diagnostics = context.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let span = bevy::render::diagnostic::RecordDiagnostics::time_span(&diagnostics, context.command_encoder(), "world_cull");
    let encoder = context.command_encoder();
    encoder.clear_buffer(&built.counts, 0, None);
    let mut compute = encoder.begin_compute_pass(&ComputePassDescriptor { label: Some("world cull"), timestamp_writes: None });
    compute.set_pipeline(pipeline);
    let groups = built.chunk_count.div_ceil(WORKGROUP);
    for slot in 0..built.live_views {
        compute.set_bind_group(0, &built.cull_group, &[slot * VIEW_STRIDE as u32]);
        compute.dispatch_workgroups(groups, 1, 1);
    }
    drop(compute);
    span.end(context.command_encoder());
}
