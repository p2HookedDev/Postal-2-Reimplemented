use super::*;

/// For the log: a missing asset here means nothing of the world draws.
fn waiting_for(world: &World, draws: &WorldDraws) -> String {
    let meshes = world.resource::<RenderAssets<RenderMesh>>();
    let allocator = world.resource::<MeshAllocator>();
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let images = world.resource::<RenderAssets<GpuImage>>();
    let count = |test: &dyn Fn(&WorldSection) -> bool| draws.sections.iter().filter(|section| test(section)).count();
    format!(
        "{} of {} sections: {} meshes, {} allocations, {} index lists, {} materials, {} light maps",
        count(&|section| meshes.get(section.mesh).is_none() || allocator.mesh_index_slice(&section.mesh).is_none() || materials.get(section.material).is_none()),
        draws.sections.len(),
        count(&|section| meshes.get(section.mesh).is_none()),
        count(&|section| allocator.mesh_vertex_slice(&section.mesh).is_none()),
        count(&|section| allocator.mesh_index_slice(&section.mesh).is_none()),
        count(&|section| materials.get(section.material).is_none()),
        count(&|section| section.lightmap.is_some_and(|image| images.get(image).is_none())),
    )
}

/// `None` until every mesh and material has reached the render world.
pub(super) fn build(world: &mut World, draws: &WorldDraws) -> Option<Built> {
    let meshes = world.resource::<RenderAssets<RenderMesh>>();
    let allocator = world.resource::<MeshAllocator>();
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let images = world.resource::<RenderAssets<GpuImage>>();
    let device = world.resource::<RenderDevice>();
    let queue = world.resource::<RenderQueue>();
    let (cull_layout, _) = world.resource::<WorldPass>().pipeline.clone()?;

    let light_slots = lightmap_slots(draws);
    let bindless_lightmaps = lightmap_arrays_usable(device);
    let slab_size = if bindless_lightmaps { LIGHTMAPS_PER_SLAB } else { 1 };

    let mut bucket_of = HashMap::<BucketKey, usize>::default();
    let mut buckets = Vec::<Bucket>::new();
    let mut uniforms = Vec::with_capacity(draws.sections.len());
    let mut chunk_lists: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut material_list = HashMap::<UntypedAssetId, (MaterialBindingId, Arc<MaterialProperties>)>::default();
    let mut mesh_slices = HashMap::<AssetId<Mesh>, (u32, u32, MeshSlabs)>::default();
    let mut mesh_sections = HashMap::<AssetId<Mesh>, Vec<u32>>::default();
    let mut mesh_group_of = HashMap::<(Option<MeshSlabId>, Option<usize>), usize>::default();
    let mut mesh_group_keys = Vec::new();

    for (index, section) in draws.sections.iter().enumerate() {
        let mesh = meshes.get(section.mesh)?;
        let material = materials.get(section.material)?;
        let slabs = allocator.mesh_slabs(&section.mesh)?;
        let vertices = allocator.mesh_vertex_slice(&section.mesh)?;
        let index_start = allocator.mesh_index_slice(&section.mesh)?.range.start;
        let vertex_start = vertices.range.start;
        let bevy::render::mesh::RenderMeshBufferInfo::Indexed { index_format, .. } = mesh.buffer_info else { return None };
        material_list.insert(section.material, (material.binding, material.properties.clone()));
        mesh_slices.insert(section.mesh, (vertex_start, index_start, slabs));
        mesh_sections.entry(section.mesh).or_default().push(index as u32);

        let light_slot = section.lightmap.and_then(|image| light_slots.get(&image).copied());
        let lightmap_slab = light_slot.map(|slot| slot / slab_size);
        let alpha_mode = material.properties.alpha_mode;
        let alpha = alpha_mode_pipeline_key(alpha_mode, &Msaa::Off);
        let mask = matches!(alpha_mode, AlphaMode::Mask(_));
        let key = BucketKey {
            type_id: section.material.type_id(),
            material_key: material.properties.material_key.clone(),
            material_bits: material.properties.mesh_pipeline_key_bits,
            alpha,
            mask,
            mesh_bits: mesh.key_bits.bits(),
            layout: mesh.layout.clone(),
            group: material.binding.group,
            slabs,
            lightmap_slab,
        };
        let bucket = *bucket_of.entry(key).or_insert_with(|| {
            let group_key = (slabs.metadata_slab_id, lightmap_slab);
            let next = mesh_group_of.len();
            let mesh_group = *mesh_group_of.entry(group_key).or_insert_with(|| {
                mesh_group_keys.push(group_key);
                next
            });
            buckets.push(Bucket {
                entity: Entity::PLACEHOLDER,
                type_id: section.material.type_id(),
                mesh: section.mesh,
                layout: mesh.layout.clone(),
                mesh_bits: MeshPipelineKey::from_bits_retain(mesh.key_bits.bits()),
                properties: material.properties.clone(),
                group: material.binding.group,
                slabs,
                index_format,
                mask,
                lightmapped: lightmap_slab.is_some(),
                first_draw: 0,
                draws: 0,
                mesh_group,
            });
            chunk_lists.push(Vec::new());
            buckets.len() - 1
        });
        chunk_lists[bucket].extend((0..section.chunks.len()).map(|chunk| (index, chunk)));

        let metadata_index = allocator.mesh_metadata_slice(&section.mesh).map(|slice| slice.range.start);
        uniforms.push(section_uniform(section, material, vertex_start, metadata_index, light_slot.map(|slot| slot % slab_size)));
    }

    let mut mesh_chunks = HashMap::default();
    let (chunk_bytes, firsts, next_draw) = chunk_table(draws, &mut buckets, &chunk_lists, &mut mesh_chunks, allocator)?;
    if next_draw == 0 {
        return None;
    }

    let mut images_by_slot = vec![None; light_slots.len()];
    for (&image, &slot) in &light_slots {
        images_by_slot[slot] = Some(images.get(image)?);
    }

    let mut instances = StorageBuffer::from(uniforms);
    instances.set_label(Some("world instances"));
    instances.write_buffer(device, queue);
    let model = instances.binding()?;

    let (chunks, args, counts, views, cull_group) = cull_buffers(world, &cull_layout, &chunk_bytes, &firsts, u64::from(next_draw), buckets.len() as u64);

    let mesh_groups = mesh_bind_groups(world, &model, &mesh_group_keys, &images_by_slot, slab_size);
    let metadata_buffers = mesh_group_keys.iter().filter_map(|(slab, _)| *slab).filter_map(|slab| Some((slab, allocator.buffer_for_slab(slab)?.id()))).collect();
    let lightmap_views = light_slots.keys().filter_map(|&image| images.get(image).map(|gpu| (image, gpu.texture_view.id(), gpu.sampler.id()))).collect();
    let materials = material_list.into_iter().map(|(id, (binding, properties))| (id, binding, properties)).collect();
    info!("world pass: {} sections, {next_draw} chunks, {} buckets, {} light maps", draws.sections.len(), buckets.len(), light_slots.len());
    Some(Built {
        sections: draws.sections.clone(),
        buckets,
        chunk_count: next_draw,
        instances,
        chunks,
        args,
        counts,
        views,
        cull_group,
        mesh_groups,
        materials,
        mesh_slices,
        mesh_chunks,
        mesh_sections,
        metadata_buffers,
        lightmap_views,
        live_views: 0,
    })
}

fn is_stale(world: &World, built: &Built, draws: &WorldDraws) -> bool {
    if !Arc::ptr_eq(&built.sections, &draws.sections) {
        return true;
    }
    // The modern look's sun mask swaps a light map's image under the same handle, and a texture setting swaps its sampler.
    let images = world.resource::<RenderAssets<GpuImage>>();
    if built.lightmap_views.iter().any(|(image, view, sampler)| images.get(*image).is_some_and(|image| image.texture_view.id() != *view || image.sampler.id() != *sampler)) {
        return true;
    }
    let materials = world.resource::<ErasedRenderAssets<PreparedMaterial>>();
    let same = |now: &PreparedMaterial, binding: &MaterialBindingId, built: &MaterialProperties| {
        now.binding.group == binding.group
            && now.binding.slot == binding.slot
            && now.properties.alpha_mode == built.alpha_mode
            && now.properties.material_key == built.material_key
            && now.properties.mesh_pipeline_key_bits == built.mesh_pipeline_key_bits
    };
    if built.materials.iter().any(|(id, binding, properties)| materials.get(*id).is_some_and(|now| !same(now, binding, properties))) {
        return true;
    }
    let allocator = world.resource::<MeshAllocator>();
    built.metadata_buffers.iter().any(|(slab, buffer)| allocator.buffer_for_slab(*slab).map(Buffer::id) != Some(*buffer))
}

/// A relit terrain sector is uploaded again and can land elsewhere in its slab. Only its draws' offsets change, so only those
/// are written. `false`: a mesh left its slab, and the buckets must be built again.
fn follow_moved_meshes(world: &mut World) -> bool {
    let Some(mut built) = world.resource_mut::<WorldPass>().built.take() else { return true };
    let allocator = world.resource::<MeshAllocator>();
    let queue = world.resource::<RenderQueue>();
    let ids = world.resource::<ModifiedMeshes>().0.iter().copied().chain(allocator.meshes_displaced_by_slab_growth());
    let mut same_slabs = true;
    let mut rewrite_instances = false;
    for id in ids {
        let Some(&(vertex, index, slabs)) = built.mesh_slices.get(&id) else { continue };
        let now = allocator.mesh_vertex_slice(&id).map(|slice| slice.range.start).zip(allocator.mesh_index_slice(&id).map(|slice| slice.range.start));
        let Some((new_vertex, new_index)) = now.filter(|_| allocator.mesh_slabs(&id) == Some(slabs)) else {
            same_slabs = false;
            break;
        };
        if (new_vertex, new_index) == (vertex, index) {
            continue;
        }
        for &(slot, first_index) in built.mesh_chunks.get(&id).into_iter().flatten() {
            let at = u64::from(slot) * CHUNK_SIZE;
            queue.write_buffer(&built.chunks, at + CHUNK_FIRST_INDEX_AT, &(new_index + first_index).to_le_bytes());
            queue.write_buffer(&built.chunks, at + CHUNK_BASE_VERTEX_AT, &new_vertex.to_le_bytes());
        }
        let metadata_index = allocator.mesh_metadata_slice(&id).map_or(0, |slice| slice.range.start);
        for &section in built.mesh_sections.get(&id).into_iter().flatten() {
            let uniform = &mut built.instances.get_mut()[section as usize];
            uniform.first_vertex_index = new_vertex;
            uniform.metadata_index = metadata_index;
        }
        built.mesh_slices.insert(id, (new_vertex, new_index, slabs));
        rewrite_instances = true;
    }
    if rewrite_instances {
        built.instances.write_buffer(world.resource::<RenderDevice>(), queue);
    }
    world.resource_mut::<WorldPass>().built = Some(built);
    same_slabs
}

fn clear_phases(world: &mut World) {
    fn clear<P: bevy::render::render_phase::BinnedPhaseItem>(world: &mut World) {
        for phase in world.resource_mut::<ViewBinnedRenderPhases<P>>().values_mut() {
            phase.non_mesh_items.clear();
        }
    }
    clear::<Opaque3d>(world);
    clear::<AlphaMask3d>(world);
    clear::<Opaque3dPrepass>(world);
    clear::<AlphaMask3dPrepass>(world);
}

pub(super) fn rebuild_if_stale(world: &mut World) {
    let Some(draws) = world.get_resource::<WorldDraws>().cloned() else { return };
    let stale = match &world.resource::<WorldPass>().built {
        Some(built) => is_stale(world, built, &draws),
        None => !draws.sections.is_empty(),
    };
    if !stale && follow_moved_meshes(world) {
        return;
    }
    if world.resource::<WorldPass>().pipeline.is_none() {
        let pipeline = cull_pipeline(world.resource::<PipelineCache>());
        world.resource_mut::<WorldPass>().pipeline = Some(pipeline);
    }
    let old = world.resource_mut::<WorldPass>().built.take();
    if let Some(old) = old {
        for bucket in &old.buckets {
            world.despawn(bucket.entity);
        }
        clear_phases(world);
        world.resource_mut::<WorldPass>().queued.clear();
    }
    if draws.sections.is_empty() {
        return;
    }
    let Some(mut built) = build(world, &draws) else {
        let frames = &mut world.resource_mut::<WorldPass>().waited_frames;
        *frames += 1;
        if *frames == WAIT_LOG_FRAMES {
            warn!("world pass still waiting for {}", waiting_for(world, &draws));
        }
        return;
    };
    for (index, bucket) in built.buckets.iter_mut().enumerate() {
        bucket.entity = world.spawn(WorldBucketIndex(index)).id();
    }
    let mut pass = world.resource_mut::<WorldPass>();
    pass.built = Some(built);
    pass.queued.clear();
    pass.waited_frames = 0;
}

pub(super) fn extract_modified_meshes(mut events: Extract<MessageReader<AssetEvent<Mesh>>>, mut modified: ResMut<ModifiedMeshes>) {
    modified.0.extend(events.read().filter_map(|event| match event {
        AssetEvent::Modified { id } => Some(*id),
        _ => None,
    }));
}
