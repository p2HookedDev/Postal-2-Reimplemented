use super::*;

impl Game {
    pub fn material_texture(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let (source, state) = self.material_source(id, material)?;
        let decode = if state.darken { TextureDecode::Rgba } else { TextureDecode::Keep };
        self.section_texture(source, state, decode)
    }

    #[cfg(test)]
    pub fn material_texture_rgba(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let (source, state) = self.material_source(id, material)?;
        self.section_texture(source, state, TextureDecode::Rgba)
    }


    pub fn fluid_texture(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let walk = self.base_texture_export(id, material, &mut RenderState::default())?;
        let Some((package, export)) = walk.combiner else { return self.material_texture(id, material) };
        let combiner = self.combiner_at(package, export)?;
        let (mut state1, mut state2) = (RenderState::default(), RenderState::default());
        let side1 = self.base_texture_export(package, combiner.material1, &mut state1);
        let side2 = self.base_texture_export(package, combiner.material2, &mut state2);
        let is_cube = |side: &(Option<Walk>, RenderState)| side.0.as_ref().is_some_and(|walk| walk.cube.is_some());
        let Ok([(cube_side, cube_state), (base, mut base_state)]) = split_by_cube([(side1, state1), (side2, state2)], is_cube) else {
            let tint = self.combiner_tint(package, export);
            let mut texture = self.material_texture(id, material)?;
            if tint.is_some() {
                texture.state.color = tint;
            }
            return Some(texture);
        };
        let cube = cube_side?.cube?;
        base_state.color = base_state.color.map(|[r, g, b, _]| [r, g, b, 255]);
        let specular = SpecularSource { image: cube, combine: None, cube: true, env_map: cube_state.env_map, mask: SpecularMask::None, tint: None };
        let base = base.as_ref();
        let source = TextureSource { texture: base.and_then(|base| base.texture), opacity: None, wet: base.and_then(|base| base.wet), combine: None, glow: None, detail: None, specular: Some(specular), switch: None };
        let mut texture = self.section_texture(source, base_state, TextureDecode::Keep)?;
        if combiner.ops.color == p2_assets::ColorOperation::Multiply
            && let Some((_, params)) = &mut texture.specular
        {
            params.mode = SpecularMode::Multiply;
        }
        Some(texture)
    }

    fn combiner_at(&mut self, id: PackageId, export: usize) -> Option<p2_assets::Combiner> {
        self.with_objects(id, |objects, source| read_combiner(objects, source, export).ok())
    }

    fn combiner_tint(&mut self, id: PackageId, export: usize) -> Option<[u8; 4]> {
        let combiner = self.combiner_at(id, export)?;
        if combiner.ops.color != p2_assets::ColorOperation::Multiply {
            return None;
        }
        [combiner.material1, combiner.material2].into_iter().find_map(|side| {
            let mut state = RenderState::default();
            let walk = self.base_texture_export(id, side, &mut state)?;
            state.color.filter(|_| walk.texture.is_none() && walk.cube.is_none()).map(|[r, g, b, _]| [r, g, b, 255])
        })
    }

    fn section_texture(&mut self, source: TextureSource, state: RenderState, decode: TextureDecode) -> Option<SectionTexture> {
        let SourceLayers { mut image, glow, .. } = self.source_layers(source, decode)?;
        if state.darken {
            invert_color(&mut image);
        }
        Some(SectionTexture { image, glow, detail: self.detail_image(source), specular: self.specular_layer(source), state })
    }

    pub fn material_source(&mut self, id: PackageId, material: i32) -> Option<(TextureSource, RenderState)> {
        let mut state = RenderState::default();
        let walk = self.base_texture_export(id, material, &mut state)?;
        if walk.cube.is_some() {
            return None;
        }
        let opacity = walk.opacity.and_then(|(id, reference)| {
            let mut opacity_state = RenderState::default();
            Some(match self.base_texture_export(id, reference, &mut opacity_state)?.texture {
                Some(texture) => Opacity::Texture(texture),
                None => Opacity::Constant(opacity_state.color.map_or(255, |color| color[3])),
            })
        });
        let combine = walk.combiner.and_then(|(id, export)| self.combine_source(id, export));
        let plain = combine.is_none() && walk.wet.is_none() && walk.texture.is_some() && state.uv.is_empty();
        let glow = walk.glow.filter(|_| plain).and_then(|(id, layer)| Some([self.still_texture(id, layer.material)?, self.still_texture(id, layer.mask)?]));
        let detail = walk.detail.and_then(|(id, reference)| self.base_texture_export(id, reference, &mut RenderState::default())?.texture);
        let specular = walk.specular.and_then(|(id, specular)| self.specular_source(id, specular, plain && glow.is_none()));
        let (specular, specular_uv) = specular.map_or((None, Vec::new()), |(specular, uv)| (Some(specular), uv));
        state.specular_uv = specular_uv;
        Some((TextureSource { texture: walk.texture, opacity, wet: walk.wet, combine, glow, detail, specular, switch: walk.switch }, state))
    }

    fn specular_source(&mut self, id: PackageId, p2_assets::SpecularLayer { material: specular, mask, mask_is_diffuse: diffuse_mask }: p2_assets::SpecularLayer, bake: bool) -> Option<(SpecularSource, Vec<UvModifier>)> {
        let mut state = RenderState::default();
        let walk = self.base_texture_export(id, specular, &mut state)?;
        let (image, cube) = match (walk.cube, walk.texture) {
            (Some(cube), _) => (cube, true),
            (None, Some(texture)) => (texture, false),
            _ => return None,
        };
        let mask = match mask {
            0 => SpecularMask::None,
            _ if diffuse_mask => SpecularMask::BaseAlpha,
            _ => {
                let mut mask_state = RenderState::default();
                match self.base_texture_export(id, mask, &mut mask_state)?.texture {
                    Some(texture) if bake && mask_state.uv.is_empty() => SpecularMask::Texture(texture),
                    Some(_) => return None,
                    None => SpecularMask::Constant(mask_state.color.map_or(255, |color| color[3])),
                }
            }
        };
        let uv = if cube { Vec::new() } else { state.uv };
        let combine = walk.combiner.filter(|_| !cube).and_then(|(id, export)| self.combine_source(id, export));
        let tint = walk.combiner.filter(|_| cube).and_then(|(id, export)| self.combiner_tint(id, export));
        Some((SpecularSource { image, combine, cube, env_map: state.env_map, mask, tint }, uv))
    }

    pub fn specular_layer(&mut self, source: TextureSource) -> Option<(Image, SpecularParams)> {
        let specular = source.specular?;
        Some((self.specular_image(specular)?, specular_params(source)?))
    }

    /// Reads only `image`, `combine`, `cube` and `tint`: sources that differ elsewhere share the image.
    pub fn specular_image(&mut self, specular: SpecularSource) -> Option<Image> {
        let (id, export) = specular.image;
        Some(if specular.cube {
            let faces = self.with_objects(id, |objects, source| read_cubemap_faces(objects, source, export).ok())?;
            let mut decoded = Vec::new();
            for face in faces {
                let (face_id, face_export) = self.export_of(id, face)?;
                let (texture, palette) = self.texture_at(face_id, face_export)?;
                let mut face = p2_assets::texture_rgba(&texture, palette.as_deref())?;
                if let Some(tint) = specular.tint {
                    tint_texels(&mut face.2, tint);
                }
                decoded.push(face);
            }
            cube_image(std::array::from_fn(|face| view(&decoded[face])))
        } else if let Some(image) = specular.combine.and_then(|combine| self.combined_image(combine)) {
            image
        } else {
            let (texture, palette) = self.texture_at(id, export)?;
            texture_to_image(&texture, palette.as_deref())?
        })
    }

    pub fn detail_image(&mut self, source: TextureSource) -> Option<Image> {
        let (texture, palette) = self.texture_at(source.detail?.0, source.detail?.1)?;
        texture_to_image(&texture, palette.as_deref())
    }

    fn alpha_glow(&mut self, source: TextureSource) -> Option<Image> {
        source.opacity?;
        let (id, export) = match source.specular?.mask {
            SpecularMask::BaseAlpha => source.texture?,
            SpecularMask::Texture(texture) => texture,
            SpecularMask::None | SpecularMask::Constant(_) => return None,
        };
        let (texture, palette) = self.texture_at(id, export)?;
        alpha_on_black_image(&texture, palette.as_deref())
    }

    fn glow_bake(&mut self, source: TextureSource) -> Option<(Image, Image)> {
        let [glow, mask] = source.glow?;
        let mut rgba = |(id, export): (PackageId, usize)| {
            let (texture, palette) = self.texture_at(id, export)?;
            p2_assets::texture_rgba(&texture, palette.as_deref())
        };
        let (diffuse, glow, mask) = (rgba(source.texture?)?, rgba(glow)?, rgba(mask)?);
        Some(texture_glow(view(&diffuse), view(&glow), view(&mask)))
    }

    fn combine_source(&mut self, id: PackageId, export: usize) -> Option<CombineSource> {
        let combiner = self.combiner_at(id, export)?;
        let material1 = self.still_texture(id, combiner.material1)?;
        let material2 = self.still_texture(id, combiner.material2)?;
        let mask = match combiner.mask {
            0 => None,
            mask => Some(self.still_texture(id, mask)?),
        };
        Some(CombineSource { ops: combiner.ops, material1, material2, mask })
    }

    fn still_texture(&mut self, id: PackageId, material: i32) -> Option<(PackageId, usize)> {
        let mut state = RenderState::default();
        let texture = self.base_texture_export(id, material, &mut state)?.texture;
        texture.filter(|_| state.uv.is_empty())
    }

    fn combined_image(&mut self, combine: CombineSource) -> Option<Image> {
        let mut rgba = |(id, export): (PackageId, usize)| {
            let (texture, palette) = self.texture_at(id, export)?;
            p2_assets::texture_rgba(&texture, palette.as_deref())
        };
        let (material1, material2) = (rgba(combine.material1)?, rgba(combine.material2)?);
        let mask = match combine.mask {
            Some(mask) => Some(rgba(mask)?),
            None => None,
        };
        Some(texture_combined(&combine.ops, view(&material1), view(&material2), mask.as_ref().map(view)))
    }

    pub fn wet_source(&mut self, source: TextureSource) -> Option<WetSource> {
        let (wet_id, wet_export) = source.wet?;
        let (texture_id, texture_export) = source.texture?;
        let (params, size) = self.with_objects(wet_id, |objects, source| {
            let texture = read_texture(objects, source, wet_export);
            let size = texture.ok().and_then(|t| t.mips.first().map(|m| (m.width, m.height)));
            Some((read_wet_params(objects, source, wet_export).ok()?, size))
        })?;
        let (texture, palette) = self.texture_at(texture_id, texture_export)?;
        let palette = palette.filter(|_| texture.format == TextureFormat::P8)?;
        let mip = texture.mips.first()?;
        let (width, height) = size.unwrap_or((mip.width, mip.height));
        let sim = WetSim::new(&params, width, height, mip)?;
        let mips = vec![Mip { width, height, data: sim.indices().to_vec() }];
        Some(WetSource { sim, texture: Texture { mips, ..texture }, palette })
    }

    pub fn source_layers(&mut self, source: TextureSource, decode: TextureDecode) -> Option<SourceLayers> {
        let (baked, glow) = self.glow_bake(source).unzip();
        let (image, wet) = match baked.filter(|_| source.opacity.is_none()) {
            Some(image) => (image, self.wet_source(source)),
            None => self.unbaked_image(source, decode)?,
        };
        let glow = glow.or_else(|| self.alpha_glow(source));
        Some(SourceLayers { image, glow, wet })
    }

    fn unbaked_image(&mut self, source: TextureSource, decode: TextureDecode) -> Option<(Image, Option<WetSource>)> {
        let wet = if source.opacity.is_none() { self.wet_source(source) } else { None };
        if let Some(combine) = source.combine
            && let Some(image) = self.combined_image(combine)
        {
            return Some((image, wet));
        }
        if let Some(wet) = wet {
            // The animation rewrites the pixels every step (`wet.rs`): the main world keeps them.
            let mut image = texture_image(&wet.texture, Some(wet.palette.as_slice()), Compression::Off)?;
            image.asset_usage = bevy::asset::RenderAssetUsages::all();
            return Some((image, Some(wet)));
        }
        let Some((id, export)) = source.texture else { return Some((white_image(), None)) };
        let (texture, palette) = self.texture_at(id, export)?;
        let mask = match (source.opacity, source.specular) {
            (Some(Opacity::Constant(alpha)), _) => return texture_with_constant_alpha(&texture, palette.as_deref(), alpha).map(|image| (image, None)),
            (Some(Opacity::Texture(texture)), _) | (None, Some(SpecularSource { mask: SpecularMask::Texture(texture), .. })) => Some(texture),
            (None, _) => None,
        };
        let mask = mask.and_then(|(id, export)| self.texture_at(id, export));
        let image = match mask {
            Some((mask, mask_palette)) => texture_with_opacity(&texture, palette.as_deref(), &mask, mask_palette.as_deref()),
            None => None,
        };
        let convert = |texture: &Texture, palette: Option<&[[u8; 4]]>| {
            if decode == TextureDecode::Rgba { texture_image(texture, palette, Compression::Off) } else { texture_to_image(texture, palette) }
        };
        image.or_else(|| convert(&texture, palette.as_deref())).map(|image| (image, None))
    }

    pub fn ui_texture(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let mut state = RenderState::default();
        let (id, export) = self.base_texture_export(id, material, &mut state)?.texture?;
        let (texture, palette) = self.texture_at(id, export)?;
        let image = texture_to_ui_image(&texture, palette.as_deref(), state.blending == Some(Blending::Additive))?;
        Some(SectionTexture { image, glow: None, detail: None, specular: None, state })
    }

    pub fn texture(&mut self, id: PackageId, reference: i32) -> Option<(Texture, Option<Vec<[u8; 4]>>)> {
        let (id, export) = self.export_of(id, reference)?;
        self.texture_at(id, export)
    }

    fn texture_at(&mut self, id: PackageId, export: usize) -> Option<(Texture, Option<Vec<[u8; 4]>>)> {
        let texture = self.with_objects(id, |objects, source| read_texture(objects, source, export).ok())?;
        let palette = match texture.palette {
            Some(reference) => {
                let (palette_id, palette_export) = self.export_of(id, reference)?;
                let (pkg, file) = self.data(palette_id);
                read_palette(pkg, file, &pkg.exports[palette_export]).ok()
            }
            None => None,
        };
        Some((texture, palette))
    }

    fn base_texture_export(
        &mut self,
        mut id: PackageId,
        mut material: i32,
        state: &mut RenderState,
    ) -> Option<Walk> {
        // `RenderState::opacity` is a reference in the package of the shader that set it; so are `wet` and `combiner`.
        let mut opacity = None;
        let mut wet = None;
        let mut combiner = None;
        let mut glow = None;
        let mut detail = None;
        let mut specular = None;
        let mut switch = None;
        for _ in 0..MAX_HOPS {
            let found = self.with_objects(id, |objects, source| base_texture(objects, source, material, &mut *state)).ok()?;
            let export = |reference: Option<i32>| reference.and_then(|reference| usize::try_from(reference - 1).ok().map(|export| (id, export)));
            opacity = opacity.or(state.opacity.map(|reference| (id, reference)));
            wet = wet.or(export(state.wet));
            combiner = combiner.or(export(state.combiner));
            glow = glow.or(state.glow.map(|references| (id, references)));
            detail = detail.or(state.detail.map(|detail| (id, detail.material)));
            specular = specular.or(state.specular.map(|references| (id, references)));
            switch = switch.or(export(state.switch));
            let walk = |texture, cube| Some(Walk { texture, opacity, wet, combiner, glow, detail, specular, cube, switch });
            match found {
                BaseTexture::Texture(export) => return walk(Some((id, usize::try_from(export - 1).ok()?)), None),
                BaseTexture::Color => return walk(None, None),
                BaseTexture::Cubemap(export) => return walk(None, Some((id, usize::try_from(export - 1).ok()?))),
                // An invisible shader often has no texture at all; it still must hide the mesh's own material.
                BaseTexture::None if state.blending == Some(Blending::Invisible) => return walk(None, None),
                BaseTexture::None => return None,
                BaseTexture::Import(import) => {
                    (id, material) = self.export_of(id, import).map(|(id, e)| (id, e as i32 + 1))?;
                }
            }
        }
        None
    }
}

fn split_by_cube<T>([a, b]: [T; 2], is_cube: impl Fn(&T) -> bool) -> Result<[T; 2], [T; 2]> {
    if is_cube(&a) {
        Ok([a, b])
    } else if is_cube(&b) {
        Ok([b, a])
    } else {
        Err([a, b])
    }
}

fn tint_texels(texels: &mut [[u8; 4]], tint: [u8; 4]) {
    for texel in texels {
        for (c, t) in texel[..3].iter_mut().zip(tint) {
            *c = p2_math::mul_byte_round(*c, t);
        }
    }
}

fn white_image() -> Image {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let size = Extent3d { width: 1, height: 1, depth_or_array_layers: 1 };
    Image::new_fill(size, TextureDimension::D2, &[255; 4], TextureFormat::Rgba8UnormSrgb, bevy::asset::RenderAssetUsages::RENDER_WORLD)
}

fn view((width, height, texels): &(u32, u32, Vec<[u8; 4]>)) -> (u32, u32, &[[u8; 4]]) {
    (*width, *height, texels.as_slice())
}

/// `OB_Darken`: one minus the colour in display space; multiplied with the frame buffer it darkens as the driver's
/// `INVSRCCOLOR` does (an sRGB byte's product converts exactly). The alpha stays.
fn invert_color(image: &mut Image) {
    if let Some(data) = image.data.as_mut() {
        for texel in data.as_chunks_mut::<4>().0 {
            for c in &mut texel[..3] {
                *c = 255 - *c;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cube_on_either_side_leaves_the_other_side_as_the_base() {
        let is_cube = |side: &&str| *side == "cube";
        assert_eq!(split_by_cube(["cube", "base"], is_cube), Ok(["cube", "base"]));
        assert_eq!(split_by_cube(["base", "cube"], is_cube), Ok(["cube", "base"]));
        assert_eq!(split_by_cube(["base", "color"], is_cube), Err(["base", "color"]));
    }

    #[test]
    fn a_specular_tint_dims_the_cube_and_keeps_alpha() {
        let mut texels = [[255, 255, 255, 200], [100, 200, 0, 7]];
        tint_texels(&mut texels, [47, 47, 0, 255]);
        assert_eq!(texels, [[47, 47, 0, 200], [18, 37, 0, 7]]);
    }
}
