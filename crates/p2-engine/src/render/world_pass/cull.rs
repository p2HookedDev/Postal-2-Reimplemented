use super::*;

/// `Vec4(normal, d)`: a point is inside at `dot(normal, p) + d >= 0`.
pub(super) fn view_planes(clip_from_world: Mat4) -> [Vec4; 6] {
    let row = |index: usize| clip_from_world.row(index);
    let plane = |raw: Vec4| {
        let length = raw.truncate().length();
        if length > 1e-6 { raw / length } else { Vec4::W }
    };
    [plane(row(3) + row(0)), plane(row(3) - row(0)), plane(row(3) + row(1)), plane(row(3) - row(1)), plane(row(3) - row(2)), plane(row(2))]
}

pub(super) fn prepare_world_views(
    mut commands: Commands,
    mut pass: ResMut<WorldPass>,
    queue: Res<RenderQueue>,
    cameras: Query<(Entity, &ExtractedView, Option<&WorldCullSlot>), With<WorldCamera>>,
    seen_zones: Option<Res<WorldSeenZones>>,
) {
    let Some(built) = pass.built.as_mut() else { return };
    let seen_zones = seen_zones.map_or(WorldSeenZones::default().0, |seen| seen.0);
    let clip_from_world = |view: &ExtractedView| view.clip_from_world.unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());
    let mut bytes = Vec::with_capacity(MAX_VIEWS * VIEW_STRIDE as usize);
    let mut slot = 0u32;
    for (entity, view, had) in cameras.iter().take(MAX_VIEWS) {
        let start = bytes.len();
        bytes.extend(view_planes(clip_from_world(view)).iter().flat_map(|plane| plane.to_array()).flat_map(|value: f32| value.to_le_bytes()));
        push_u32s(&mut bytes, &[slot * built.chunk_count, slot * built.buckets.len() as u32, 0, 0]);
        push_u32s(&mut bytes, &seen_zones);
        bytes.resize(start + VIEW_STRIDE as usize, 0);
        if had != Some(&WorldCullSlot(slot)) {
            commands.entity(entity).insert(WorldCullSlot(slot));
        }
        slot += 1;
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
