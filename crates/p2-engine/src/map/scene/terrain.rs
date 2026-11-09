use super::*;

struct TerrainSurface<'a> {
    terrain: &'a p2_assets::Terrain,
    normals: &'a [[f32; 3]],
    alpha_uvs: &'a [[f32; 2]],
    lighting: Option<&'a [[u8; 4]]>,
}

struct DrawnLayer {
    index: usize,
    image: Handle<Image>,
    uvs: Vec<[f32; 2]>,
    alpha_map: Handle<Image>,
    alpha: Vec<f32>,
    glow: Option<Handle<Image>>,
    shader_layers: crate::render::shader_layers::Layers,
    matrix: [[f32; 4]; 4],
}

struct DecorationLayer {
    mesh: i32,
    placements: Vec<p2_assets::Decoration>,
    in_sky: bool,
    zone: u8,
}

/// The engine uses layers while both their texture and alpha map are set: the first one missing either
/// ends the list, layer 0 included.
fn terrain_layers(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    surface: &TerrainSurface,
) -> Vec<DrawnLayer> {
    let mut layers = Vec::new();
    for (index, layer) in surface.terrain.layers.iter().enumerate() {
        if layer.texture == 0 || layer.alpha_map == 0 {
            break;
        }
        let map = game.texture(id, layer.alpha_map);
        let Some((width, height, alpha)) = map.and_then(|(texture, palette)| texture_alpha(&texture, palette.as_deref())) else {
            continue;
        };
        let Some(MaterialImages { image, glow, detail, specular, state }) = texture(game, t, id, layer.texture) else { continue };
        let premultiplied = state.opacity.is_some() && (state.glow.is_some() || state.specular.is_some());
        let scale = state.detail.map_or(0.0, |detail| detail.scale);
        let shader_layers = crate::render::shader_layers::Layers::new(detail, scale, specular, state.specular_uv, premultiplied);
        let (w, h) = (width as usize, height as usize);
        let per_vertex = surface
            .alpha_uvs
            .iter()
            .map(|&[u, v]| {
                let x = ((u * w as f32) as usize).min(w - 1);
                let y = ((v * h as f32) as usize).min(h - 1);
                f32::from(alpha[y * w + x]) / 255.0
            })
            .collect::<Vec<_>>();
        let alpha_map = t.add_image(alpha_map_image(width, height, &alpha));
        let uvs = surface.terrain.layer_uvs(layer);
        layers.push(DrawnLayer { index, image, uvs, alpha_map, alpha: per_vertex, glow, shader_layers, matrix: layer.matrix });
    }
    layers
}

fn sector_sections(
    t: &mut PendingAssets,
    surface: &TerrainSurface,
    triangles: &[[usize; 3]],
    drawn: &[&DrawnLayer],
    in_sky: bool,
) -> (Vec<SectionView>, Vec<Handle<Mesh>>) {
    let TerrainSurface { terrain, normals, alpha_uvs, lighting } = *surface;
    let shader_layers = drawn.iter().any(|layer| {
        let shader = &layer.shader_layers;
        layer.glow.is_some() || shader.detail.is_some() || shader.specular.is_some() || shader.flat_specular.is_some()
    });
    let mut handles = Vec::new();
    let sector_vertices = p2_bevy_assets::SectorVertices::new(triangles, terrain.vertices.len());
    let sections: Vec<SectionView> = if crate::render::terrain_material::one_pass(drawn.len(), shader_layers) {
        let mut mesh = terrain_layer_mesh(&terrain.vertices, normals, &sector_vertices, &[], alpha_uvs, lighting, None);
        if !in_sky {
            keep_for_relighting(&mut mesh);
        }
        let chunks = crate::render::world_pass::chunks_of(&mut mesh);
        let mesh = t.add_mesh(mesh);
        handles.push(mesh.clone());
        let terrain_layers = drawn
            .iter()
            .map(|layer| crate::render::terrain_material::TerrainPassLayer {
                texture: layer.image.clone(),
                alpha_map: layer.alpha_map.clone(),
                matrix: layer.matrix,
            })
            .collect();
        vec![SectionView {
            mesh,
            texture: Some(drawn[0].image.clone()),
            alpha_mode: AlphaMode::Opaque,
            no_scene_lights: true,
            fullbright: false,
            glow: None,
            layers: Default::default(),
            lightmap: None,
            two_sided: false,
            uv: Vec::new(),
            skin: None,
            tint: [1.0; 4],
            alpha_map: None,
            terrain: terrain_layers,
            chunks,
            bsp_node: None,
            terrain_layer: None,
        }]
    } else {
        let mut first = true;
        drawn
            .iter()
            .map(|layer| {
                let opaque = std::mem::take(&mut first);
                let mut mesh =
                    terrain_layer_mesh(&terrain.vertices, normals, &sector_vertices, &layer.uvs, alpha_uvs, lighting, Some(layer.alpha.as_slice()));
                if !in_sky {
                    keep_for_relighting(&mut mesh);
                }
                let chunks = if opaque { crate::render::world_pass::chunks_of(&mut mesh) } else { None };
                let mesh = t.add_mesh(mesh);
                handles.push(mesh.clone());
                SectionView {
                    mesh,
                    texture: Some(layer.image.clone()),
                    alpha_mode: if opaque { AlphaMode::Opaque } else { AlphaMode::Blend },
                    no_scene_lights: true,
                    fullbright: false,
                    glow: layer.glow.clone(),
                    layers: layer.shader_layers.clone(),
                    lightmap: None,
                    two_sided: false,
                    uv: Vec::new(),
                    skin: None,
                    tint: [1.0; 4],
                    alpha_map: Some(layer.alpha_map.clone()),
                    terrain: Vec::new(),
                    chunks,
                    bsp_node: None,
                    terrain_layer: (!opaque).then_some(layer.index as u32),
                }
            })
            .collect()
    };
    (sections, handles)
}

/// Each decoration's colour (terrain light × colour map) is drawn on its material with `Modulate2X`:
/// one tinted, unlit copy of the sections per colour, rounded to steps of 8.
fn decoration_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    layer: DecorationLayer,
) -> Option<Vec<ModelView>> {
    let DecorationLayer { mesh: mesh_reference, placements, in_sky, zone } = layer;
    let (mesh_id, mesh_export) = game.export_of(id, mesh_reference)?;
    let (mesh, materials) = {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        (read_static_mesh(pkg, file, export).ok()?, read_static_mesh_materials(pkg, file, export).unwrap_or_default())
    };
    let sections = sections_of_static_mesh(&mesh, t, SectionChunks::Chunked, |t, i| texture(game, t, mesh_id, *materials.get(i)?));
    let radius = mesh.positions.iter().map(|&p| Vector::from(p).size_squared()).fold(0.0_f32, f32::max).sqrt();
    let mut by_color: HashMap<([u8; 3], [i32; 2]), (Vec<Transform>, [f32; 3], [f32; 3])> = HashMap::new();
    for d in &placements {
        let color = [0, 1, 2].map(|i| ((u16::from(d.color[i]) + 4) / 8 * 8).min(255) as u8);
        let (location, scale) = (<[f32; 3]>::from(d.location), <[f32; 3]>::from(d.scale));
        let transform = unreal_transform(location.into(), d.rotation, scale.into(), [0.0; 3].into());
        let reach = radius * scale.iter().fold(0.0_f32, |most, s| most.max(s.abs()));
        let (low, high) = (location.map(|c| c - reach), location.map(|c| c + reach));
        let (instances, min, max) = by_color.entry((color, cell(transform.translation))).or_insert((Vec::new(), low, high));
        instances.push(transform);
        (*min, *max) = merged_box((*min, *max), (low, high));
    }
    let models = by_color
        .into_iter()
        .map(|((color, _), (instances, min, max))| {
            let tint = |s: &SectionView| std::array::from_fn(|i| s.tint[i] * color.get(i).copied().map_or(1.0, modulate_2x));
            let sections = sections.iter().map(|s| SectionView { no_scene_lights: true, tint: tint(s), ..s.clone() }).collect();
            let zone = (!in_sky).then(|| ZoneCulled { zones: vec![zone], min, max, only_from: None });
            ModelView { sections, instances, zone }
        })
        .collect();
    Some(models)
}

pub(super) struct TerrainZones<'a> {
    pub(super) model: &'a Model,
    pub(super) sky_zone: Option<u8>,
    pub(super) terrain_zones: &'a [bool],
}

pub(super) struct TerrainParts {
    pub(super) models: Vec<ModelView>,
    pub(super) sky_models: Vec<ModelView>,
    pub(super) static_terrains: Vec<p2_assets::Terrain>,
    pub(super) terrain_sectors: Vec<crate::render::terrain_light::TerrainSector>,
    pub(super) terrains: usize,
    pub(super) hidden_terrains: usize,
    pub(super) drawn_decorations: usize,
}

pub(super) fn terrain_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    zones: &TerrainZones,
    terrain_infos: Vec<p2_assets::Terrain>,
) -> TerrainParts {
    let (mut models, mut sky_models) = (Vec::new(), Vec::new());
    let mut terrains = 0;
    let mut hidden_terrains = 0;
    let mut static_terrains = Vec::new();
    let mut decorations = Vec::new();
    let mut terrain_sectors = Vec::new();
    for mut terrain in terrain_infos {
        let heights =
            game.texture(id, terrain.heightmap).and_then(|(texture, _)| texture_heights(&texture));
        if let Some((width, height, heights)) = heights
            && (width as usize, height as usize) == (terrain.width, terrain.height)
        {
            terrain.vertices = terrain.vertices_from_heights(&heights);
        }
        if terrain.vertices.is_empty() {
            continue;
        }
        terrains += 1;
        // A terrain belongs to the zone its `TerrainInfo` stands in (`ZoneInfo.Terrains`): in the sky zone it is part
        // of the sky (some `SkyZoneInfo`s are `bTerrainZone`).
        let zone = p2_collision::point_zone(zones.model, terrain.location);
        let in_sky = zones.sky_zone == Some(zone);
        // Retail draws a terrain and its decorations only from a `bTerrainZone` zone.
        if !in_sky && !zones.terrain_zones.get(usize::from(zone)).copied().unwrap_or(true) {
            hidden_terrains += 1;
            static_terrains.push(terrain);
            continue;
        }
        for layer in terrain.deco_layers.iter().filter(|layer| layer.show_on_terrain) {
            let mut map = |reference: i32| (reference != 0).then(|| game.texture(id, reference)).flatten();
            let density = map(layer.density_map).and_then(|(texture, palette)| texture_alpha(&texture, palette.as_deref()));
            let scale = map(layer.scale_map).and_then(|(texture, _)| texture_bgra(&texture));
            let color = map(layer.color_map).and_then(|(texture, _)| texture_bgra(&texture));
            let maps = DecoMaps {
                density: density.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
                scale: scale.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
                color: color.as_ref().map(|(w, h, texels)| TexelMap { width: *w as usize, height: *h as usize, texels }),
            };
            let placements = terrain.decorations(layer, &terrain.vertices, &maps);
            decorations.push(DecorationLayer { mesh: layer.static_mesh, placements, in_sky, zone });
        }
        let normals = terrain.vertex_normals();
        let alpha_uvs: Vec<[f32; 2]> = (0..terrain.vertices.len()).map(|i| terrain.alpha_uv(i)).collect();
        let surface = TerrainSurface {
            terrain: &terrain,
            normals: &normals,
            alpha_uvs: &alpha_uvs,
            lighting: (!terrain.vertex_colors.is_empty()).then_some(terrain.vertex_colors.as_slice()),
        };
        let layers = terrain_layers(game, t, texture, id, &surface);
        let size = terrain.sector_size.max(1);
        let (quads_x, quads_y) = (terrain.width.saturating_sub(1), terrain.height.saturating_sub(1));
        for y0 in (0..quads_y).step_by(size) {
            for x0 in (0..quads_x).step_by(size) {
                let terrain = &terrain;
                let triangles: Vec<[usize; 3]> = (y0..(y0 + size).min(quads_y))
                    .flat_map(|y| (x0..(x0 + size).min(quads_x)).flat_map(move |x| terrain.quad_triangles(x, y)))
                    .collect();
                let Some((min, max)) = bounds(triangles.iter().flatten().map(|&i| terrain.vertices[i])) else { continue };
                let drawn: Vec<&DrawnLayer> = layers.iter().filter(|layer| triangles.iter().flatten().any(|&i| layer.alpha[i] > 0.0)).collect();
                let (sections, handles) = sector_sections(t, &surface, &triangles, &drawn, in_sky);
                if !in_sky && !handles.is_empty() {
                    let order = p2_bevy_assets::terrain_sector_vertices(&triangles, terrain.vertices.len());
                    terrain_sectors.push(crate::render::terrain_light::TerrainSector {
                        meshes: handles,
                        positions: order.iter().map(|&i| terrain.vertices[i]).collect(),
                        normals: order.iter().map(|&i| normals[i]).collect(),
                        baked: order.iter().map(|&i| p2_bevy_assets::terrain_vertex_color(surface.lighting, i)).collect(),
                        min: min.into(),
                        max: max.into(),
                    });
                }
                let zone = (!in_sky).then(|| ZoneCulled { zones: vec![zone], min, max, only_from: None });
                let drawn_by = if in_sky { &mut sky_models } else { &mut models };
                drawn_by.push(ModelView { sections, instances: vec![Transform::IDENTITY], zone });
            }
        }
        static_terrains.push(terrain);
    }
    let mut drawn_decorations = 0;
    for layer in decorations.into_iter().filter(|layer| !layer.placements.is_empty()) {
        let (count, in_sky) = (layer.placements.len(), layer.in_sky);
        let Some(decorated) = decoration_models(game, t, texture, id, layer) else { continue };
        drawn_decorations += count;
        let drawn_by = if in_sky { &mut sky_models } else { &mut models };
        drawn_by.extend(decorated);
    }
    TerrainParts { models, sky_models, static_terrains, terrain_sectors, terrains, hidden_terrains, drawn_decorations }
}
