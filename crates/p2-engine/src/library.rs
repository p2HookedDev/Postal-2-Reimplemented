use crate::render::shader_layers::{SpecularCoords, SpecularMode, SpecularParams};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bevy::image::Image;
use p2_assets::{BaseTexture, Blending, CombineOps, Mip, RenderState, Texture, TextureFormat, UvModifier, base_texture, read_combiner, read_cubemap_faces, read_palette, read_texture, read_wet_params};
use p2_render_cpu::{WetSim};
use p2_bevy_assets::{alpha_on_black_image, cube_image, texture_combined, texture_glow, texture_to_image, texture_to_rgba_image, texture_to_ui_image, texture_with_constant_alpha, texture_with_opacity};
use p2_package::{Ini, Loader, Package, PackageId, ResolveError, Resolved};
use p2_script::{Objects, Source};

const MAX_HOPS: usize = 16;

#[derive(Default)]
pub struct Library {
    games: HashMap<PathBuf, Game>,
}

#[derive(Clone)]
pub struct Game {
    loader: Loader,
    system: PathBuf,
}

pub type PlayLoaderTask<T> = std::thread::JoinHandle<Result<T, LoadError>>;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error("{name} is not the package the search paths find under that name")]
    NotOnSearchPaths { name: String },
    #[error(transparent)]
    Thread(std::io::Error),
    #[error("{package}: loading failed")]
    Panicked { package: String },
    #[error("{package}: {source}")]
    Play { package: String, source: ResolveError },
    #[error("{package}: {source}")]
    Read { package: String, source: std::io::Error },
    #[error("{package}: {source}")]
    Parse { package: String, source: p2_package::Error },
    #[error("{package}: no level")]
    NoLevel { package: String },
    #[error("{name}: {source}")]
    Level { name: String, source: p2_assets::Error },
    #[error("{name}: level has no model")]
    NoModel { name: String },
    #[error("{name}: no drawn polygons")]
    NoPolygons { name: String },
}

#[derive(Debug, Default)]
pub struct LoadProgress(AtomicU32);

impl LoadProgress {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub(crate) fn set(&self, value: f32) {
        // Bits of non-negative floats order like the floats.
        self.0.fetch_max(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureSource {
    pub texture: Option<(PackageId, usize)>,
    pub opacity: Option<Opacity>,
    pub wet: Option<(PackageId, usize)>,
    pub combine: Option<CombineSource>,
    pub glow: Option<[(PackageId, usize); 2]>,
    pub detail: Option<(PackageId, usize)>,
    pub specular: Option<SpecularSource>,
    pub switch: Option<(PackageId, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Opacity {
    Texture((PackageId, usize)),
    Constant(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpecularSource {
    pub image: (PackageId, usize),
    pub combine: Option<CombineSource>,
    pub cube: bool,
    pub env_map: Option<u8>,
    pub mask: SpecularMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecularMask {
    None,
    BaseAlpha,
    Texture((PackageId, usize)),
    Constant(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CombineSource {
    pub ops: CombineOps,
    pub material1: (PackageId, usize),
    pub material2: (PackageId, usize),
    pub mask: Option<(PackageId, usize)>,
}

struct Walk {
    texture: Option<(PackageId, usize)>,
    opacity: Option<(PackageId, i32)>,
    wet: Option<(PackageId, usize)>,
    combiner: Option<(PackageId, usize)>,
    glow: Option<(PackageId, p2_assets::GlowLayer)>,
    detail: Option<(PackageId, i32)>,
    specular: Option<(PackageId, p2_assets::SpecularLayer)>,
    cube: Option<(PackageId, usize)>,
    switch: Option<(PackageId, usize)>,
}

pub struct WetSource {
    pub sim: WetSim,
    pub texture: Texture,
    pub palette: Vec<[u8; 4]>,
}

pub struct SectionTexture {
    pub image: Image,
    pub glow: Option<Image>,
    pub detail: Option<Image>,
    pub specular: Option<(Image, SpecularParams)>,
    pub state: RenderState,
}

impl Library {
    pub(crate) fn system_of(root: &Path, package: &str) -> PathBuf {
        let pl_system = root.join("Paradise Lost").join("System");
        if package.to_lowercase().starts_with("paradise lost\\") && pl_system.is_dir() {
            pl_system
        } else {
            root.join("System")
        }
    }

    pub(crate) fn game(&mut self, system: &Path) -> Option<&mut Game> {
        self.games.get_mut(system)
    }

    pub fn open(&mut self, root: &Path, package: &str) -> Result<(&mut Game, PackageId), LoadError> {
        let system = Self::system_of(root, package);
        let game = match self.games.entry(system.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                // Scripts first: assets read with their class defaults.
                entry.insert(Game { loader: Game::script_loader(&system)?, system: system.clone() })
            }
        };

        // The loader must find this very file under the package's name, or its imports mean something else.
        let path = root.join(package);
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let found = game.loader.find(name).map(Path::to_path_buf);
        if !found.is_some_and(|found| same_file(&found, &path)) {
            return Err(LoadError::NotOnSearchPaths { name: name.to_owned() });
        }
        let id = game.loader.load(name)?;
        Ok((game, id))
    }
}

impl Game {
    pub fn play_loader(&self, map: &str) -> Result<(Arc<Loader>, PackageId), ResolveError> {
        Self::add_map(Self::script_loader(&self.system)?, map, &LoadProgress::default())
    }

    /// After travel, `played` with the new map added, so every package keeps its id (`Vm::set_loader`). Package loading
    /// reports 0 to 0.45 to `progress`.
    pub fn spawn_play_loader<T: Send + 'static>(
        &self,
        map: &str,
        played: Option<Loader>,
        progress: Arc<LoadProgress>,
        then: impl FnOnce(Arc<Loader>, PackageId) -> Result<T, LoadError> + Send + 'static,
    ) -> PlayLoaderTask<T> {
        let (system, map) = (self.system.clone(), map.to_owned());
        std::thread::Builder::new()
            .name("map loading".into())
            // The VM's script recursion (`start_vm` runs the map's start-up scripts here).
            .stack_size(crate::play::SCRIPT_STACK)
            .spawn(move || {
                let started = std::time::Instant::now();
                let loader = match played {
                    Some(loader) => loader,
                    None => Self::script_loader(&system)?,
                };
                let (loader, id) = Self::add_map(loader, &map, &progress)?;
                bevy::log::info!("load: {map} packages read in {} ms", started.elapsed().as_millis());
                then(loader, id)
            })
            .map_err(LoadError::Thread)
            .unwrap_or_else(|error| std::thread::spawn(move || Err(error)))
    }

    fn script_loader(system: &Path) -> Result<Loader, ResolveError> {
        let beside_exe = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.to_path_buf()));
        let own: Vec<String> = [crate::game_dir::config_folder(), beside_exe]
            .into_iter()
            .flatten()
            .map(|dir| dir.join("Packages").join("*.u").to_string_lossy().into_owned())
            .collect();
        let mut loader = Loader::open_with(system, &own)?;
        loader.load_scripts()?;
        Ok(loader)
    }

    fn add_map(mut loader: Loader, map: &str, progress: &LoadProgress) -> Result<(Arc<Loader>, PackageId), ResolveError> {
        let id = loader.load(map)?;
        loader.load_imports_with(id, |done, known| progress.set(0.4 * done as f32 / known as f32))?;
        loader.load_imported(&["MeshAnimation", "SkeletalMesh", "Texture"])?;
        progress.set(0.45);
        Ok((Arc::new(loader), id))
    }

    pub fn configs(&self) -> Vec<(&'static str, Ini)> {
        let read = |names: &[&str]| {
            names.iter().find_map(|name| std::fs::read(self.system.join(name)).ok()).map(|bytes| Ini::from_bytes(&bytes))
        };
        [("System", read(&["Postal2.ini", "Default.ini"])), ("User", read(&["User.ini", "DefUser.ini"])), ("Selectors", read(&["Selectors.ini"])), ("Subtitles", read(&["Subtitles.ini"])), ("Settings", Some(crate::display::load_settings()))]
            .into_iter()
            .filter_map(|(name, ini)| Some((name, ini?)))
            .collect()
    }

    pub fn adopt(&mut self, clone: &Self) {
        self.loader.adopt(&clone.loader);
    }

    pub fn adopt_loader(&mut self, loader: &Loader) {
        self.loader.adopt(loader);
    }

    pub fn same_package(&self, other: &Self, id: PackageId) -> Option<PackageId> {
        self.loader.loaded_id(&other.loader.package(id).name)
    }

    pub fn export_by_path(&mut self, path: &str) -> Option<(PackageId, usize)> {
        self.loader.export_by_path(path).ok().flatten()
    }

    pub fn export_by_path_of_class(&mut self, path: &str, class: &str) -> Option<(PackageId, usize)> {
        self.loader.export_by_path_of_class(path, class).ok().flatten()
    }

    pub fn package_name(&self, id: PackageId) -> &str {
        &self.loader.package(id).name
    }

    pub fn data(&self, id: PackageId) -> (&Package, &[u8]) {
        let loaded = self.loader.package(id);
        (&loaded.package, &loaded.data)
    }

    pub fn objects(&self) -> Objects<'_, Loader> {
        Objects::new(&self.loader)
    }

    pub fn export_of(&mut self, id: PackageId, reference: i32) -> Option<(PackageId, usize)> {
        match self.loader.resolve(id, reference).ok()?? {
            Resolved { package, export: Some(export) } => Some((package, export)),
            Resolved { export: None, .. } => None,
        }
    }

    pub fn material_texture(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let (source, state) = self.material_source(id, material)?;
        let decoded = state.darken;
        self.section_texture(source, state, decoded)
    }

    pub fn material_texture_rgba(&mut self, id: PackageId, material: i32) -> Option<SectionTexture> {
        let (source, state) = self.material_source(id, material)?;
        self.section_texture(source, state, true)
    }

    fn section_texture(&mut self, source: TextureSource, state: RenderState, decoded: bool) -> Option<SectionTexture> {
        let (mut image, glow) = match self.glow_bake(source) {
            Some((image, glow)) if source.opacity.is_none() => (image, Some(glow)),
            Some((_, glow)) => (self.source_image(source, decoded)?, Some(glow)),
            None => (self.source_image(source, decoded)?, self.glow_image(source)),
        };
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
        Some((SpecularSource { image, combine, cube, env_map: state.env_map, mask }, uv))
    }

    pub fn specular_layer(&mut self, source: TextureSource) -> Option<(Image, SpecularParams)> {
        let specular = source.specular?;
        let (id, export) = specular.image;
        let image = if specular.cube {
            let faces = {
                let (package, data) = self.data(id);
                read_cubemap_faces(&mut Objects::new(&self.loader), Source { package, data }, export).ok()?
            };
            let mut decoded = Vec::new();
            for face in faces {
                let (face_id, face_export) = self.export_of(id, face)?;
                let (texture, palette) = self.texture_at(face_id, face_export)?;
                decoded.push(p2_assets::texture_rgba(&texture, palette.as_deref())?);
            }
            cube_image(std::array::from_fn(|face| view(&decoded[face])))
        } else if let Some(image) = specular.combine.and_then(|combine| self.combined_image(combine)) {
            image
        } else {
            let (texture, palette) = self.texture_at(id, export)?;
            texture_to_image(&texture, palette.as_deref())?
        };
        let (mode, constant) = match specular.mask {
            SpecularMask::None => (SpecularMode::Add, 1.0),
            SpecularMask::BaseAlpha | SpecularMask::Texture(_) if source.opacity.is_some() => (SpecularMode::GlowAlpha, 1.0),
            SpecularMask::BaseAlpha | SpecularMask::Texture(_) => (SpecularMode::BaseAlpha, 1.0),
            SpecularMask::Constant(alpha) => (SpecularMode::Constant, f32::from(alpha) / 255.0),
        };
        // Without a `TexEnvMap` a cube is looked up as a camera-space env map (unverified); a 2D texture reads the UV
        // stream with the base modifiers (unverified).
        let coords = match (specular.cube, specular.env_map) {
            (true, _) => SpecularCoords::Cube,
            (false, Some(_)) => SpecularCoords::EnvMap,
            (false, None) => SpecularCoords::BaseUv,
        };
        Some((image, SpecularParams { mode, world: specular.env_map == Some(0), constant, coords }))
    }

    pub fn detail_image(&mut self, source: TextureSource) -> Option<Image> {
        let (texture, palette) = self.texture_at(source.detail?.0, source.detail?.1)?;
        texture_to_image(&texture, palette.as_deref())
    }

    pub fn glow_image(&mut self, source: TextureSource) -> Option<Image> {
        if let Some((_, glow)) = self.glow_bake(source) {
            return Some(glow);
        }
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
        let combiner = {
            let (package, data) = self.data(id);
            read_combiner(&mut Objects::new(&self.loader), Source { package, data }, export).ok()?
        };
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
        let (params, size) = {
            let (package, data) = self.data(wet_id);
            let (source, mut objects) = (Source { package, data }, self.objects());
            let texture = read_texture(&mut objects, source, wet_export);
            let size = texture.ok().and_then(|t| t.mips.first().map(|m| (m.width, m.height)));
            (read_wet_params(&mut objects, source, wet_export).ok()?, size)
        };
        let (texture, palette) = self.texture_at(texture_id, texture_export)?;
        let palette = palette.filter(|_| texture.format == TextureFormat::P8)?;
        let mip = texture.mips.first()?;
        let (width, height) = size.unwrap_or((mip.width, mip.height));
        let sim = WetSim::new(&params, width, height, mip)?;
        let mips = vec![Mip { width, height, data: sim.indices().to_vec() }];
        Some(WetSource { sim, texture: Texture { mips, ..texture }, palette })
    }

    pub fn source_image(&mut self, source: TextureSource, decoded: bool) -> Option<Image> {
        if source.opacity.is_none()
            && let Some((image, _)) = self.glow_bake(source)
        {
            return Some(image);
        }
        if let Some(combine) = source.combine
            && let Some(image) = self.combined_image(combine)
        {
            return Some(image);
        }
        if source.opacity.is_none()
            && let Some(wet) = self.wet_source(source)
        {
            // The animation rewrites the pixels every step (`wet.rs`): the main world keeps them.
            let mut image = texture_to_rgba_image(&wet.texture, Some(wet.palette.as_slice()))?;
            image.asset_usage = bevy::asset::RenderAssetUsages::all();
            return Some(image);
        }
        let Some((id, export)) = source.texture else { return Some(white_image()) };
        let (texture, palette) = self.texture_at(id, export)?;
        let mask = match (source.opacity, source.specular) {
            (Some(Opacity::Constant(alpha)), _) => return texture_with_constant_alpha(&texture, palette.as_deref(), alpha),
            (Some(Opacity::Texture(texture)), _) | (None, Some(SpecularSource { mask: SpecularMask::Texture(texture), .. })) => Some(texture),
            (None, _) => None,
        };
        let mask = mask.and_then(|(id, export)| self.texture_at(id, export));
        let image = match mask {
            Some((mask, mask_palette)) => texture_with_opacity(&texture, palette.as_deref(), &mask, mask_palette.as_deref()),
            None => None,
        };
        let convert = if decoded { texture_to_rgba_image } else { texture_to_image };
        image.or_else(|| convert(&texture, palette.as_deref()))
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
        let texture = {
            let (package, data) = self.data(id);
            read_texture(&mut Objects::new(&self.loader), Source { package, data }, export).ok()?
        };
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
            let (package, data) = self.data(id);
            let found = base_texture(&mut Objects::new(&self.loader), Source { package, data }, material, state).ok()?;
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
                BaseTexture::None => return None,
                BaseTexture::Import(import) => {
                    (id, material) = self.export_of(id, import).map(|(id, e)| (id, e as i32 + 1))?;
                }
            }
        }
        None
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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
