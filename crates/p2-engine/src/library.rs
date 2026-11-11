use crate::render::shader_layers::{SpecularCoords, SpecularMode, SpecularParams};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bevy::image::Image;
use p2_assets::{BaseTexture, Blending, CombineOps, Mip, RenderState, Texture, TextureFormat, UvModifier, base_texture, read_combiner, read_cubemap_faces, read_palette, read_texture, read_wet_params};
use p2_render_cpu::{WetSim};
use p2_bevy_assets::{alpha_on_black_image, cube_image, Compression, texture_combined, texture_glow, texture_image, texture_to_image, texture_to_ui_image, texture_with_constant_alpha, texture_with_opacity};
use p2_package::{Ini, Loader, Package, PackageId, ResolveError, Resolved};
use p2_script::{LayoutCache, Objects, Source};
mod materials;

const MAX_HOPS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextureDecode {
    Keep,
    Rgba,
}

#[derive(Default)]
pub struct Library {
    games: HashMap<PathBuf, Game>,
}

#[derive(Clone)]
pub struct Game {
    loader: Loader,
    system: PathBuf,
    /// Safe to keep: the loader never drops a package, so no address in its keys is reused.
    layouts: LayoutCache,
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
    pub tint: Option<[u8; 4]>,
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

pub struct SourceLayers {
    pub image: Image,
    pub glow: Option<Image>,
    pub wet: Option<WetSource>,
}

pub fn specular_params(source: TextureSource) -> Option<SpecularParams> {
    let specular = source.specular?;
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
    Some(SpecularParams { mode, world: specular.env_map == Some(0), constant, coords })
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
                entry.insert(Game { loader: Game::script_loader(&system)?, system: system.clone(), layouts: LayoutCache::default() })
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

    fn with_objects<T>(&mut self, id: PackageId, read: impl for<'a> FnOnce(&mut Objects<'a, Loader>, Source<'a>) -> T) -> T {
        let loaded = self.loader.package(id);
        let mut objects = Objects::with_layout_cache(&self.loader, std::mem::take(&mut self.layouts));
        let value = read(&mut objects, Source { package: &loaded.package, data: &loaded.data });
        self.layouts = objects.into_layout_cache();
        value
    }

    pub fn export_of(&mut self, id: PackageId, reference: i32) -> Option<(PackageId, usize)> {
        match self.loader.resolve(id, reference).ok()?? {
            Resolved { package, export: Some(export) } => Some((package, export)),
            Resolved { export: None, .. } => None,
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
