use super::*;
use p2_render::canvas::{CanvasFont, CanvasImage, CanvasSource};

const FIRST_READS: [std::time::Duration; 2] = [std::time::Duration::from_millis(1), std::time::Duration::from_millis(8)];

#[derive(Resource, Default)]
pub struct Hud {
    pub(super) movies: Movies,
    pub(super) hud_assets: HudAssets,
    pub(super) canvas_actors: Vec<CanvasActor>,
    pub(super) failed_canvas_meshes: FxHashSet<String>,
    /// Loading frames show it again: the engine does not clear the screen.
    pub(super) last_hud: Arc<[CanvasItem]>,
    pub(super) progress_bar_read: bool,
    pub(super) boot_cover: Option<BootCover>,
    painted_at: Option<std::time::Instant>,
    first_reads_left: std::time::Duration,
}

impl Hud {
    pub(super) fn leave_map(&mut self, map: Option<&str>, commands: &mut Commands) {
        if let Some(map) = map {
            self.hud_assets.fonts.retain(|path, _| !p2_render::canvas::in_package(path, map));
            self.hud_assets.textures.retain(|path, _| !p2_render::canvas::in_package(path, map));
        }
        self.movies.stop(commands);
        self.canvas_actors.clear();
        self.failed_canvas_meshes.clear();
    }

    pub(super) fn start_frame(&mut self, now: std::time::Instant) {
        let [least, most] = FIRST_READS;
        self.first_reads_left = self.painted_at.replace(now).map_or(most, |before| ((now - before) / 2).clamp(least, most));
    }

    fn first_read<T>(&mut self, read: impl FnOnce(&mut Self) -> T) -> Option<T> {
        if self.first_reads_left.is_zero() {
            return None;
        }
        let started = std::time::Instant::now();
        let found = read(self);
        self.first_reads_left = self.first_reads_left.saturating_sub(started.elapsed());
        Some(found)
    }
}

/// Seconds: how long the first map stays black once shown if its scene has not started, and the fade after.
const COVER_HOLD: f32 = 1.0;
const COVER_FADE: f32 = 0.5;

/// The first map comes in from black: covered while it loads, faded in once its scene runs.
#[derive(Default)]
pub(super) struct BootCover {
    shown: f32,
    faded: Option<f32>,
}

impl BootCover {
    /// The opacity after `seconds` more of the shown map; 0 once the cover is gone.
    pub(super) fn opacity(&mut self, seconds: f32, scene_started: bool) -> f32 {
        self.shown += seconds;
        match &mut self.faded {
            Some(faded) => *faded += seconds,
            None if scene_started || self.shown >= COVER_HOLD => self.faded = Some(0.0),
            None => {}
        }
        self.faded.map_or(1.0, |faded| (1.0 - faded / COVER_FADE).max(0.0))
    }
}

#[derive(Default)]
pub(super) struct HudAssets {
    fonts: HashMap<String, CanvasFont>,
    textures: HashMap<String, CanvasImage>,
}

impl HudAssets {
    pub(super) fn paths(&self) -> impl Iterator<Item = &String> {
        self.fonts.keys().chain(self.textures.keys())
    }

    pub(super) fn add(&mut self, read: Self) {
        self.fonts.extend(read.fonts);
        self.textures.extend(read.textures);
    }

    pub(super) fn read(game: &mut Game, vm: &Vm<Arc<Loader>>, painted: &HashSet<String>, mut add: impl FnMut(Image) -> Handle<Image>) -> Self {
        let mut found = Vec::new();
        for hud in vm.actors().filter(|&actor| vm.store.is_a(actor, "HUD")) {
            for value in vm.store.properties(hud) {
                object_refs(&value, &mut found);
            }
        }
        let is_font = |id: ObjectId| vm.store.name(vm.store.object(id).class_name).eq_ignore_ascii_case("Font");
        let materials: HashSet<&str> = found
            .into_iter()
            .filter(|&id| vm.store.object(id).class.is_none() && !is_font(id) && !vm.store.name(vm.store.object(id).class_name).eq_ignore_ascii_case("Class"))
            .map(|id| vm.store.object(id).path.as_str())
            .filter(|path| !painted.contains(*path))
            .collect();
        let fonts: HashSet<&str> = vm.object_ids().filter(|&id| is_font(id)).map(|id| vm.store.object(id).path.as_str()).filter(|path| !painted.contains(*path)).collect();
        let mut assets = Self::default();
        for path in fonts {
            if let Some(font) = read_hud_font(game, path, &mut add) {
                assets.fonts.insert(path.to_owned(), font);
            }
        }
        for path in materials {
            if let Some(texture) = read_hud_texture(game, path, &mut add) {
                assets.textures.insert(path.to_owned(), texture);
            }
        }
        assets
    }
}

fn object_refs(value: &p2_vm::Value, found: &mut Vec<ObjectId>) {
    match value {
        p2_vm::Value::Object(Some(id)) => found.push(*id),
        p2_vm::Value::Array(values) | p2_vm::Value::Struct(values) => values.iter().for_each(|value| object_refs(value, found)),
        _ => {}
    }
}

fn read_hud_font(game: &mut Game, path: &str, mut add: impl FnMut(Image) -> Handle<Image>) -> Option<CanvasFont> {
    let (package, export) = game.export_by_path(path)?;
    let (pkg, data) = game.data(package);
    let font = read_font(pkg, data, &pkg.exports[export]).ok()?;
    let pages = font.textures.iter().map(|&reference| read_ui_image(game, package, reference, &mut add)).collect();
    Some(CanvasFont { font, pages })
}

fn read_hud_texture(game: &mut Game, material: &str, add: impl FnMut(Image) -> Handle<Image>) -> Option<CanvasImage> {
    let (package, export) = game.export_by_path(material)?;
    read_ui_image(game, package, export_reference(export), add)
}

fn read_ui_image(game: &mut Game, package: PackageId, reference: i32, mut add: impl FnMut(Image) -> Handle<Image>) -> Option<CanvasImage> {
    let texture = game.ui_texture(package, reference)?;
    let size = texture.image.size().as_vec2();
    Some(CanvasImage { image: add(texture.image), texels: egui::vec2(size.x, size.y) })
}

pub(super) struct LibraryReads<'a> {
    pub(super) session: &'a mut Session,
    pub(super) hud: &'a mut Hud,
}

impl CanvasSource for LibraryReads<'_> {
    fn texture(&mut self, material: &str, images: &mut Assets<Image>) -> Option<Option<CanvasImage>> {
        let Self { session, hud } = self;
        hud.first_read(|hud| {
            let ahead = hud.hud_assets.textures.remove(material);
            ahead.or_else(|| read_hud_texture(open_game(&mut session.library, session.map_system.as_deref())?, material, |image| images.add(image)))
        })
    }

    fn font(&mut self, path: &str, images: &mut Assets<Image>) -> Option<Option<CanvasFont>> {
        let Self { session, hud } = self;
        hud.first_read(|hud| {
            let ahead = hud.hud_assets.fonts.remove(path);
            ahead.or_else(|| read_hud_font(open_game(&mut session.library, session.map_system.as_deref())?, path, |image| images.add(image)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cover_holds_until_the_scene_starts_then_fades() {
        let mut cover = BootCover::default();
        assert_eq!(cover.opacity(0.1, false), 1.0);
        assert_eq!(cover.opacity(0.1, true), 1.0);
        assert!((cover.opacity(COVER_FADE / 2.0, true) - 0.5).abs() < 1e-5);
        assert_eq!(cover.opacity(COVER_FADE, false), 0.0);
    }

    #[test]
    fn a_scene_that_never_starts_still_fades_after_the_hold() {
        let mut cover = BootCover::default();
        assert_eq!(cover.opacity(COVER_HOLD / 2.0, false), 1.0);
        assert_eq!(cover.opacity(COVER_HOLD / 2.0, false), 1.0);
        assert_eq!(cover.opacity(COVER_FADE, false), 0.0);
    }
}
