use super::*;

fn draw_key(bucket: usize) -> UntypedAssetId {
    AssetId::<Mesh>::Uuid { uuid: Uuid::from_u128(bucket as u128 + 1) }.untyped()
}

fn one_item(entity: Entity) -> NonMeshEntities {
    let mut entities = bevy::render::sync_world::MainEntityHashMap::default();
    entities.insert(MainEntity::from(Entity::PLACEHOLDER), entity);
    NonMeshEntities { entities }
}

fn erased_key(bucket: &Bucket, mesh_key: MeshPipelineKey) -> ErasedMaterialPipelineKey {
    ErasedMaterialPipelineKey { type_id: bucket.type_id, mesh_key: ErasedMeshPipelineKey::new(mesh_key), material_key: bucket.properties.material_key.clone() }
}

pub(super) fn with_buckets<R>(world: &mut World, work: impl FnOnce(&mut World, &[Bucket]) -> R) -> Option<R> {
    let built = world.resource_mut::<WorldPass>().built.take()?;
    let result = work(world, &built.buckets);
    world.resource_mut::<WorldPass>().built = Some(built);
    Some(result)
}

fn queue_main(world: &mut World, buckets: &[Bucket], view: RetainedViewEntity, view_key: MeshPipelineKey) {
    let msaa = Msaa::from_samples(view_key.msaa_samples());
    let draw_opaque = world.resource::<DrawFunctions<Opaque3d>>().read().id::<DrawWorld>();
    let draw_masked = world.resource::<DrawFunctions<AlphaMask3d>>().read().id::<DrawWorld>();
    let mut opaque = Vec::new();
    let mut masked = Vec::new();
    for (index, bucket) in buckets.iter().enumerate() {
        let mut mesh_key = view_key | bucket.mesh_bits | bucket.properties.mesh_pipeline_key_bits.downcast::<MeshPipelineKey>() | alpha_mode_pipeline_key(bucket.properties.alpha_mode, &msaa);
        if bucket.lightmapped {
            mesh_key |= MeshPipelineKey::LIGHTMAPPED;
        }
        let Some(specialize) = bucket.properties.base_specialize else { continue };
        match specialize(world, erased_key(bucket, mesh_key), &bucket.layout, &bucket.properties) {
            Ok(pipeline) if bucket.mask => masked.push((index, pipeline)),
            Ok(pipeline) => opaque.push((index, pipeline)),
            Err(error) => error!("world pass: {error}"),
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<Opaque3d>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in opaque {
            let bucket = &buckets[index];
            let key = Opaque3dBatchSetKey { pipeline, draw_function: draw_opaque, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs, lightmap_slab: None };
            phase.non_mesh_items.insert((key, Opaque3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<AlphaMask3d>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in masked {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey { pipeline, draw_function: draw_masked, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
}

fn depth_only(world: &World, bucket: &Bucket, mesh_key: MeshPipelineKey) -> bool {
    let supported = world.resource::<PrepassPipeline>().depth_clip_control_supported;
    let emulated = mesh_key.contains(MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO) && !supported;
    !emulated && !prepass_reads_material(&bucket.properties) && !mesh_key.intersects(MeshPipelineKey::MAY_DISCARD | MeshPipelineKey::PREPASS_READS_MATERIAL)
}

fn queue_prepass(world: &mut World, buckets: &[Bucket], view: RetainedViewEntity, view_key: MeshPipelineKey, msaa: Msaa) {
    let draw_opaque = world.resource::<DrawFunctions<Opaque3dPrepass>>().read().id::<DrawWorldPrepass>();
    let draw_opaque_depth = world.resource::<DrawFunctions<Opaque3dPrepass>>().read().id::<DrawWorldDepthOnly>();
    let draw_masked = world.resource::<DrawFunctions<AlphaMask3dPrepass>>().read().id::<DrawWorldPrepass>();
    let mut opaque = Vec::new();
    let mut masked = Vec::new();
    for (index, bucket) in buckets.iter().enumerate() {
        if !bucket.properties.prepass_enabled || !matches!(bucket.properties.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage) {
            continue;
        }
        let mut mesh_key = view_key | bucket.mesh_bits | alpha_mode_pipeline_key(bucket.properties.alpha_mode, &msaa);
        if bucket.lightmapped {
            mesh_key |= MeshPipelineKey::LIGHTMAPPED;
        }
        if prepass_reads_material(&bucket.properties) {
            mesh_key |= MeshPipelineKey::PREPASS_READS_MATERIAL;
        }
        let only_depth = depth_only(world, bucket, mesh_key) && mesh_key.intersection(MeshPipelineKey::ALL_PREPASS_BITS) == MeshPipelineKey::DEPTH_PREPASS;
        let Some(specialize) = bucket.properties.prepass_specialize else { continue };
        match specialize(world, &erased_key(bucket, mesh_key), &bucket.layout, &bucket.properties) {
            Ok(pipeline) if bucket.mask => masked.push((index, pipeline)),
            Ok(pipeline) => opaque.push((index, pipeline, only_depth)),
            Err(error) => error!("world pass: {error}"),
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<Opaque3dPrepass>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline, only_depth) in opaque {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey {
                pipeline,
                draw_function: if only_depth { draw_opaque_depth } else { draw_opaque },
                material_bind_group_index: (!only_depth).then_some(bucket.group.0),
                slabs: bucket.slabs,
            };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
    if let Some(phase) = world.resource_mut::<ViewBinnedRenderPhases<AlphaMask3dPrepass>>().get_mut(&view) {
        phase.non_mesh_items.clear();
        for (index, pipeline) in masked {
            let bucket = &buckets[index];
            let key = OpaqueNoLightmap3dBatchSetKey { pipeline, draw_function: draw_masked, material_bind_group_index: Some(bucket.group.0), slabs: bucket.slabs };
            phase.non_mesh_items.insert((key, OpaqueNoLightmap3dBinKey { asset_id: draw_key(index) }), one_item(bucket.entity));
        }
    }
}

pub(super) fn shadow_pipelines(world: &mut World, buckets: &[Bucket], light_key: MeshPipelineKey) -> Vec<(usize, CachedRenderPipelineId, bool)> {
    let mut items = Vec::new();
    for (index, bucket) in buckets.iter().enumerate() {
        if !bucket.casts || !bucket.properties.shadows_enabled {
            continue;
        }
        let mut mesh_key = light_key | bucket.mesh_bits;
        if bucket.lightmapped {
            mesh_key |= MeshPipelineKey::LIGHTMAPPED;
        }
        if bucket.mask {
            mesh_key |= MeshPipelineKey::MAY_DISCARD;
        }
        if prepass_reads_material(&bucket.properties) {
            mesh_key |= MeshPipelineKey::PREPASS_READS_MATERIAL;
        }
        let only_depth = depth_only(world, bucket, mesh_key);
        let Some(specialize) = bucket.properties.prepass_specialize else { continue };
        match specialize(world, &erased_key(bucket, mesh_key), &bucket.layout, &bucket.properties) {
            Ok(pipeline) => items.push((index, pipeline, only_depth)),
            Err(error) => error!("world pass: {error}"),
        }
    }
    items
}

fn redraw_levels(world: &World) {
    if let Some(stale) = world.get_resource::<StaticShadowsStale>() {
        stale.mark();
    }
}

pub(super) fn queue_world(
    world: &mut World,
    cameras: &mut QueryState<(&ExtractedView, Option<&Msaa>), With<WorldCamera>>,
) {
    let first_bucket = |world: &World| world.resource::<WorldPass>().built.as_ref().and_then(|built| built.buckets.first()).map(|bucket| bucket.entity);
    let before = first_bucket(world);
    rebuild_if_stale(world);
    if first_bucket(world) != before {
        redraw_levels(world);
    }
    world.resource_mut::<ModifiedMeshes>().0.clear();
    if world.resource::<WorldPass>().built.is_none() {
        let level_draws = world.get_resource::<SunLevelFrame>().is_some_and(|frame| frame.drawing.is_some());
        if level_draws && world.get_resource::<WorldDraws>().is_some_and(|draws| !draws.sections.is_empty()) {
            redraw_levels(world);
        }
        return;
    }
    // Bevy drops an inactive camera's phases, and with them the items queued here.
    world.resource_scope(|world, mut pass: Mut<WorldPass>| {
        let phases = world.resource::<ViewBinnedRenderPhases<Opaque3d>>();
        pass.queued.retain(|view, _| phases.contains_key(view));
    });
    let camera_views: Vec<(RetainedViewEntity, Msaa)> = cameras.iter(world).map(|(view, msaa)| (view.retained_view_entity, msaa.copied().unwrap_or(Msaa::Off))).collect();
    for (view, msaa) in camera_views {
        let Some(&view_key) = world.resource::<ViewKeyCache>().get(&view) else { continue };
        if !world.resource::<ViewBinnedRenderPhases<Opaque3d>>().contains_key(&view) {
            continue;
        }
        let prepass_key = world.resource::<ViewKeyPrepassCache>().get(&view).copied().filter(|_| world.resource::<ViewBinnedRenderPhases<Opaque3dPrepass>>().contains_key(&view));
        if world.resource::<WorldPass>().queued.get(&view) == Some(&(view_key, prepass_key)) {
            continue;
        }
        with_buckets(world, |world, buckets| {
            queue_main(world, buckets, view, view_key);
            if let Some(prepass_key) = prepass_key {
                queue_prepass(world, buckets, view, prepass_key, msaa);
            }
        });
        world.resource_mut::<WorldPass>().queued.insert(view, (view_key, prepass_key));
    }
    level::queue_level(world);
}
