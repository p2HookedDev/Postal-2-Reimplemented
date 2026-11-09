use super::*;

pub(super) fn cull_pipeline(cache: &PipelineCache) -> (BindGroupLayoutDescriptor, CachedComputePipelineId) {
    let layout = BindGroupLayoutDescriptor::new(
        "world cull",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                storage_buffer_read_only_sized(false, None),
                storage_buffer_read_only_sized(false, None),
                storage_buffer_sized(false, None),
                storage_buffer_sized(false, None),
                uniform_buffer_sized(true, NonZero::new(VIEW_SIZE)),
            ),
        ),
    );
    let pipeline = cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("world cull".into()),
        layout: vec![layout.clone()],
        shader: CULL_SHADER,
        ..Default::default()
    });
    (layout, pipeline)
}

pub(super) fn world_box(world_from_local: &Affine3A, chunk: &Chunk) -> (Vec3, Vec3) {
    let m = world_from_local.matrix3;
    let half = Vec3::from(m.x_axis).abs() * chunk.half.x + Vec3::from(m.y_axis).abs() * chunk.half.y + Vec3::from(m.z_axis).abs() * chunk.half.z;
    (world_from_local.transform_point3(chunk.center), half)
}

pub(super) fn push_u32s(bytes: &mut Vec<u8>, values: &[u32]) {
    bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
}

fn push_vec3(bytes: &mut Vec<u8>, value: Vec3) {
    bytes.extend(value.to_array().iter().flat_map(|value| value.to_le_bytes()));
}

pub(super) fn lightmap_slots(draws: &WorldDraws) -> HashMap<AssetId<Image>, usize> {
    let mut slots = HashMap::default();
    for image in draws.sections.iter().filter_map(|section| section.lightmap) {
        let next = slots.len();
        slots.entry(image).or_insert(next);
    }
    slots
}

/// As Bevy decides it for its light map layout, which it keeps private: 16 material textures and 8 probes.
pub(super) fn lightmap_arrays_usable(device: &RenderDevice) -> bool {
    let limits = device.limits();
    limits.max_binding_array_elements_per_shader_stage >= 24
        && limits.max_binding_array_sampler_elements_per_shader_stage >= 24
        && device.features().contains(WgpuFeatures::TEXTURE_BINDING_ARRAY | WgpuFeatures::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING)
}

pub(super) fn prepass_reads_material(properties: &MaterialProperties) -> bool {
    properties.get_shader(PrepassVertexShader).is_some() || properties.get_shader(PrepassFragmentShader).is_some()
}

pub(super) fn section_uniform(section: &WorldSection, material: &PreparedMaterial, first_vertex: u32, metadata_index: Option<u32>, light_slot: Option<usize>) -> MeshUniform {
    let mut flags = MeshFlags::SHADOW_RECEIVER | MeshFlags::LOD_INDEX_MASK;
    if section.world_from_local.matrix3.determinant().is_sign_positive() {
        flags |= MeshFlags::SIGN_DETERMINANT_MODEL_3X3;
    }
    let affine = Affine3::from(section.world_from_local);
    let transforms = MeshTransforms { world_from_local: affine, previous_world_from_local: affine, flags: flags.bits() };
    let mut uniform = MeshUniform::new(&transforms, first_vertex, material.binding.slot, None, None, None, None, metadata_index);
    if let Some(slot) = light_slot {
        uniform.material_and_lightmap_bind_group_slot = u32::from(material.binding.slot) | ((slot as u32) << 16);
        uniform.lightmap_uv_rect = UVec2::new(0, u32::MAX);
    }
    uniform
}

pub(super) fn chunk_table(
    draws: &WorldDraws,
    buckets: &mut [Bucket],
    chunk_lists: &[Vec<(usize, usize)>],
    mesh_chunks: &mut HashMap<AssetId<Mesh>, Vec<(u32, u32)>>,
    allocator: &MeshAllocator,
) -> Option<(Vec<u8>, Vec<u32>, u32)> {
    let mut chunk_bytes = Vec::new();
    let mut firsts = Vec::new();
    let mut next_draw = 0u32;
    for (bucket_index, (bucket, list)) in buckets.iter_mut().zip(chunk_lists).enumerate() {
        bucket.first_draw = next_draw;
        bucket.draws = list.len() as u32;
        firsts.push(next_draw);
        next_draw += bucket.draws;
        for (position, &(section_index, chunk_index)) in list.iter().enumerate() {
            let section = &draws.sections[section_index];
            let chunk = &section.chunks[chunk_index];
            mesh_chunks.entry(section.mesh).or_default().push((bucket.first_draw + position as u32, chunk.first_index));
            let (center, half) = world_box(&section.world_from_local, chunk);
            let first_index = allocator.mesh_index_slice(&section.mesh)?.range.start + chunk.first_index;
            let base_vertex = allocator.mesh_vertex_slice(&section.mesh)?.range.start;
            push_vec3(&mut chunk_bytes, center);
            push_u32s(&mut chunk_bytes, &[bucket_index as u32]);
            push_vec3(&mut chunk_bytes, half);
            let zone = section.zone.map_or(0, |zone| ZONED | (u32::from(zone) << 8));
            push_u32s(&mut chunk_bytes, &[section_index as u32, first_index, chunk.index_count, base_vertex, zone]);
        }
    }
    Some((chunk_bytes, firsts, next_draw))
}

pub(super) fn cull_buffers(world: &World, layout: &BindGroupLayoutDescriptor, chunk_bytes: &[u8], firsts: &[u32], draws: u64, buckets: u64) -> (Buffer, Buffer, Buffer, Buffer, BindGroup) {
    let device = world.resource::<RenderDevice>();
    let queue = world.resource::<RenderQueue>();
    let empty = |label: &'static str, size: u64, usage: BufferUsages| device.create_buffer(&BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false });
    let filled = |label: &'static str, contents: &[u8]| {
        let buffer = empty(label, contents.len() as u64, BufferUsages::STORAGE | BufferUsages::COPY_DST);
        queue.write_buffer(&buffer, 0, contents);
        buffer
    };
    let chunks = filled("world chunks", chunk_bytes);
    let mut first_bytes = Vec::with_capacity(firsts.len() * 4);
    push_u32s(&mut first_bytes, firsts);
    let firsts = filled("world bucket firsts", &first_bytes);
    let args = empty("world draw args", MAX_VIEWS as u64 * draws * ARGS_SIZE, BufferUsages::STORAGE | BufferUsages::INDIRECT);
    let counts = empty("world draw counts", MAX_VIEWS as u64 * buckets * COUNT_SIZE, BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST);
    let views = empty("world cull views", MAX_VIEWS as u64 * VIEW_STRIDE, BufferUsages::UNIFORM | BufferUsages::COPY_DST);
    let view_binding = BindingResource::Buffer(bevy::render::render_resource::BufferBinding { buffer: &views, offset: 0, size: NonZero::new(VIEW_SIZE) });
    let group = device.create_bind_group(
        "world cull",
        &world.resource::<PipelineCache>().get_bind_group_layout(layout),
        &BindGroupEntries::sequential((chunks.as_entire_binding(), firsts.as_entire_binding(), args.as_entire_binding(), counts.as_entire_binding(), view_binding)),
    );
    (chunks, args, counts, views, group)
}

/// Bevy's light map layout needs its arrays full.
pub(super) fn mesh_bind_groups(
    world: &World,
    model: &BindingResource,
    keys: &[(Option<MeshSlabId>, Option<usize>)],
    images_by_slot: &[Option<&GpuImage>],
    slab_size: usize,
) -> Vec<BindGroup> {
    let device = world.resource::<RenderDevice>();
    let cache = world.resource::<PipelineCache>();
    let allocator = world.resource::<MeshAllocator>();
    let fallback = world.resource::<FallbackImage>();
    let metadata_fallback = world.resource::<MeshMetadataFallbackBuffer>();
    let layouts = &world.resource::<MeshPipeline>().mesh_layouts;
    let mut groups = Vec::with_capacity(keys.len());
    for &(metadata_slab, lightmap_slab) in keys {
        let metadata = metadata_slab.and_then(|slab| allocator.buffer_for_slab(slab)).unwrap_or(&metadata_fallback.buffer);
        let Some(slab) = lightmap_slab else {
            groups.push(layouts.model_only(device, cache, model, metadata));
            continue;
        };
        let image_at = |slot: usize| images_by_slot.get(slab * slab_size + slot).copied().flatten().unwrap_or(&fallback.d2);
        let layout = cache.get_bind_group_layout(&layouts.lightmapped);
        let model_entry = BindGroupEntry { binding: 0, resource: model.clone() };
        let metadata_entry = BindGroupEntry { binding: 9, resource: metadata.as_entire_binding() };
        let views: [_; LIGHTMAPS_PER_SLAB] = std::array::from_fn(|slot| &*image_at(slot).texture_view);
        let samplers: [_; LIGHTMAPS_PER_SLAB] = std::array::from_fn(|slot| &*image_at(slot).sampler);
        let (view_resource, sampler_resource) = if slab_size > 1 {
            (BindingResource::TextureViewArray(&views), BindingResource::SamplerArray(&samplers))
        } else {
            (BindingResource::TextureView(&image_at(0).texture_view), BindingResource::Sampler(&image_at(0).sampler))
        };
        groups.push(device.create_bind_group(
            "world lightmapped",
            &layout,
            &[model_entry, metadata_entry, BindGroupEntry { binding: 4, resource: view_resource }, BindGroupEntry { binding: 5, resource: sampler_resource }],
        ));
    }
    groups
}
