use std::path::{Path, PathBuf};

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Assets, Handle};
use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::ecs::error::Result;
use bevy::ecs::prelude::{Commands, Entity, Res, ResMut, Resource, SystemSet};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::math::{UVec2, Vec3};
use bevy::mesh::Mesh;
use bevy::state::prelude::{NextState, in_state};
use bevy::time::Time;
use bevy::transform::components::Transform;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, EguiTextureHandle, egui};
use p2_assets::{read_palette, read_skeletal_mesh, read_sound, read_static_mesh, read_static_mesh_materials, read_texture};
use p2_bevy_assets::{skin_points, texture_to_image, to_bevy, unreal_mesh_transform};
use p2_package::Package;
use p2_script::Source;

use crate::ViewerState;
use crate::controls::ViewerInput;
use p2_engine::animation::{Animated, AnimationSystems};
use p2_engine::game_dir_form::GameDirForm;
use p2_engine::library::Library;
use p2_engine::map::{
    SceneTargets, SectionChunks, Session, SessionHolder, animate_mesh_with, material_images, present_session, sections_of_static_mesh, skeletal_mesh_sections, travel_package,
};
use p2_engine::mesh_view::{ModelView, Scene};
use p2_engine::play::{PlaySystems, PlayWorld};

fn loop_browsed_meshes(time: Res<Time>, animated: Res<Animated>, mut meshes: ResMut<Assets<Mesh>>) {
    for item in animated.0.iter().filter(|item| item.actor.is_none()) {
        let Some(sequence) = item.animation.sequences.get(item.sequence) else { continue };
        let pose = item.bone_pose(time.elapsed_secs() * sequence.rate);
        let points = skin_points(&item.mesh, &item.inverse_bind, &pose);
        let positions: Vec<[f32; 3]> = item
            .mesh
            .wedges
            .iter()
            .map(|w| to_bevy(points.get(w.point as usize).copied().unwrap_or_default()))
            .collect();
        for handle in &item.meshes {
            if let Some(mut mesh) = meshes.get_mut(handle) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
            }
        }
    }
}

const EXTENSIONS: &[&str] = &["u", "utx", "usx", "ukx", "fuk", "uax"];

pub struct AssetBrowserPlugin;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetBrowserSystems {
    Ui,
}

impl Plugin for AssetBrowserPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            EguiPrimaryContextPass,
            ui.in_set(AssetBrowserSystems::Ui).run_if(in_state(ViewerState::Browse)),
        )
        .add_systems(Update, play_sound.run_if(in_state(ViewerState::Browse)))
        .add_systems(Update, loop_browsed_meshes.in_set(AnimationSystems::Pose))
        .add_systems(PostUpdate, present_session::<Browser>.in_set(PlaySystems::Present).run_if(in_state(ViewerState::Browse)));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Texture,
    StaticMesh,
    SkeletalMesh,
    Level,
    Sound,
}

impl Kind {
    const ALL: [Self; 5] = [Self::Texture, Self::StaticMesh, Self::SkeletalMesh, Self::Level, Self::Sound];

    fn class(self) -> &'static str {
        match self {
            Self::Texture => "Texture",
            Self::StaticMesh => "StaticMesh",
            Self::SkeletalMesh => "SkeletalMesh",
            Self::Level => "Level",
            Self::Sound => "Sound",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Texture => "Textures",
            Self::StaticMesh => "Static meshes",
            Self::SkeletalMesh => "Skeletal meshes",
            Self::Level => "Map",
            Self::Sound => "Sounds",
        }
    }
}

#[derive(Resource)]
pub struct Browser {
    root: PathBuf,
    packages: Vec<String>,
    package_filter: String,
    package_rows: Vec<usize>,
    asset_filter: String,
    kind: Kind,
    open: Option<OpenPackage>,
    preview: Option<TexturePreview>,
    session: Session,
    status: String,
    pending: Vec<Action>,
    sound: Option<Vec<u8>>,
    playing: Option<Entity>,
}

impl SessionHolder for Browser {
    fn session(&mut self) -> &mut Session {
        &mut self.session
    }
}

struct OpenPackage {
    index: usize,
    data: Vec<u8>,
    package: Package,
    assets: [Vec<usize>; 5],
    selected: Option<(Kind, usize)>,
    rows: Vec<usize>,
    rows_kind: Option<Kind>,
}

impl OpenPackage {
    fn assets(&self, kind: Kind) -> &[usize] {
        &self.assets[kind as usize]
    }

    fn name(&self, kind: Kind, slot: usize) -> &str {
        self.package.name(self.package.exports[self.assets(kind)[slot]].object_name)
    }
}

struct TexturePreview {
    image: Handle<Image>,
    id: egui::TextureId,
    size: egui::Vec2,
}

enum Action {
    OpenPackage(usize),
    Show(Kind, usize),
    ShowNamed(String),
}

#[derive(SystemParam)]
struct Targets<'w, 's> {
    contexts: EguiContexts<'w, 's>,
    scene: SceneTargets<'w, 's>,
    input: ResMut<'w, ViewerInput>,
}

impl Browser {
    pub fn new(root: PathBuf, start_package: Option<&str>, start_asset: Option<String>) -> std::io::Result<Self> {
        let mut packages = Vec::new();
        collect_packages(&root, &root, &mut packages)?;
        packages.sort_by_key(|p| p.to_lowercase());

        let mut pending = Vec::new();
        if let Some(wanted) = start_package {
            let wanted = wanted.to_lowercase().replace('/', std::path::MAIN_SEPARATOR_STR);
            if let Some(index) = packages.iter().position(|p| p.to_lowercase().ends_with(&wanted)) {
                pending.push(Action::OpenPackage(index));
                pending.extend(start_asset.map(Action::ShowNamed));
            }
        }
        let status = format!("{} packages", packages.len());
        let package_rows = (0..packages.len()).collect();
        Ok(Self {
            root: root.clone(),
            packages,
            package_filter: String::new(),
            package_rows,
            asset_filter: String::new(),
            kind: Kind::Texture,
            open: None,
            preview: None,
            session: Session::new(root),
            status,
            pending,
            sound: None,
            playing: None,
        })
    }

    fn apply(&mut self, action: Action, t: &mut Targets) {
        match action {
            Action::OpenPackage(index) => self.open_package(index, t),
            Action::Show(kind, slot) => self.show(kind, slot, t),
            Action::ShowNamed(name) => {
                let found = self.open.as_ref().and_then(|open| {
                    Kind::ALL.into_iter().find_map(|kind| {
                        (0..open.assets(kind).len()).find(|&slot| open.name(kind, slot).eq_ignore_ascii_case(&name)).map(|slot| (kind, slot))
                    })
                });
                match found {
                    Some((kind, slot)) => {
                        self.kind = kind;
                        self.show(kind, slot, t);
                    }
                    None => self.status = format!("no asset {name}"),
                }
            }
        }
    }

    fn open_package(&mut self, index: usize, t: &mut Targets) {
        self.clear_preview(t);
        self.open = None;
        let path = self.root.join(&self.packages[index]);
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|data| Package::parse(&data).map(|package| (data, package)).map_err(|e| e.to_string()));
        let (data, package) = match parsed {
            Ok(parsed) => parsed,
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
                return;
            }
        };
        let assets = Kind::ALL.map(|kind| {
            let mut exports: Vec<usize> = (0..package.exports.len())
                .filter(|&i| package.object_name(package.exports[i].class_index) == Some(kind.class()))
                .collect();
            exports.sort_by_cached_key(|&i| package.name(package.exports[i].object_name).to_lowercase());
            exports
        });
        let open = OpenPackage { index, data, package, assets, selected: None, rows: Vec::new(), rows_kind: None };
        if open.assets(self.kind).is_empty()
            && let Some(kind) = Kind::ALL.into_iter().find(|&kind| !open.assets(kind).is_empty())
        {
            self.kind = kind;
        }
        let counts = Kind::ALL.map(|kind| format!("{} {}", open.assets(kind).len(), kind.label().to_lowercase()));
        self.status = format!("{}: {}", self.packages[index], counts.join(", "));
        self.open = Some(open);
    }

    fn show(&mut self, kind: Kind, slot: usize, t: &mut Targets) {
        self.clear_preview(t);
        self.session.clear(&mut t.scene);
        let Some(open) = &mut self.open else { return };
        // A click's slot can be stale: a travel earlier in the frame may have opened another package.
        let Some(&index) = open.assets(kind).get(slot) else { return };
        open.selected = Some((kind, slot));
        let open = &*open;
        let path = &self.packages[open.index];
        let status = match kind {
            Kind::Texture => show_texture(open, index, &mut self.session.library, &self.root, path, &mut self.preview, t),
            Kind::StaticMesh => show_static_mesh(open, index, &mut self.session.library, &self.root, path, t),
            Kind::SkeletalMesh => show_skeletal_mesh(open, index, &mut self.session.library, &self.root, path, t),
            Kind::Sound => {
                let pkg = &open.package;
                let export = &pkg.exports[index];
                let name = pkg.name(export.object_name);
                match read_sound(pkg, &open.data, export) {
                    Ok(sound) => {
                        let status = format!("{name}: {}, {} bytes", sound.file_type, sound.data.len());
                        self.sound = Some(sound.data);
                        Ok(status)
                    }
                    Err(e) => Err(format!("{name}: {e}")),
                }
            }
            Kind::Level => self.session.load_map(path, &mut t.scene).map_err(|e| e.to_string()),
        };
        self.status = match status {
            Ok(text) | Err(text) => text,
        };
    }

    fn travel_actions(&mut self, url: &str) -> Vec<Action> {
        let index = match travel_package(url) {
            None => self.open.as_ref().map(|open| open.index),
            Some(package) => self.packages.iter().position(|p| p.eq_ignore_ascii_case(&package)),
        };
        match index {
            Some(index) => vec![Action::OpenPackage(index), Action::ShowNamed("MyLevel".to_owned())],
            None => {
                self.status = format!("travel to {url}: map not found");
                Vec::new()
            }
        }
    }
    fn clear_preview(&mut self, t: &mut Targets) {
        if let Some(preview) = self.preview.take() {
            t.contexts.remove_image(preview.image.id());
        }
        t.scene.view.clear();
    }
}

fn show_texture(open: &OpenPackage, index: usize, library: &mut Library, root: &Path, path: &str, preview: &mut Option<TexturePreview>, t: &mut Targets) -> Result<String, String> {
    let pkg = &open.package;
    let name = pkg.name(pkg.exports[index].object_name);
    let texture = library.open(root, path).map_err(|e| e.to_string()).and_then(|(game, _)| {
        let source = Source { package: pkg, data: &open.data };
        read_texture(&mut game.objects(), source, index).map_err(|e| e.to_string())
    });
    let texture = texture.map_err(|e| format!("{name}: {e}"))?;
    let palette = texture
        .palette
        .filter(|&index| index > 0)
        .and_then(|index| pkg.exports.get(index as usize - 1))
        .and_then(|palette| read_palette(pkg, &open.data, palette).ok());
    let mut status = match texture.mips.first() {
        Some(mip) => {
            format!("{name}: {:?} {}x{}, {} mips", texture.format, mip.width, mip.height, texture.mips.len())
        }
        None => format!("{name}: no mips"),
    };
    match texture_to_image(&texture, palette.as_deref()) {
        Some(image) => {
            let extent = image.texture_descriptor.size;
            let image = t.scene.assets.images.add(image);
            let id = t.contexts.add_image(EguiTextureHandle::Strong(image.clone()));
            let size = egui::vec2(extent.width as f32, extent.height as f32);
            *preview = Some(TexturePreview { image, id, size });
        }
        None => status.push_str(" (cannot show)"),
    }
    Ok(status)
}

fn show_static_mesh(open: &OpenPackage, index: usize, library: &mut Library, root: &Path, path: &str, t: &mut Targets) -> Result<String, String> {
    let pkg = &open.package;
    let export = &pkg.exports[index];
    let name = pkg.name(export.object_name);
    let mesh = read_static_mesh(pkg, &open.data, export).map_err(|e| format!("{name}: {e}"))?;
    let (min, max) = bounds(mesh.positions.iter().map(|&p| Vec3::from(to_bevy(p))));
    let triangles: usize = mesh.sections.iter().map(|s| usize::from(s.num_primitives)).sum();
    let status = format!(
        "{name}: {} vertices, {triangles} triangles, {} sections, {} UV sets",
        mesh.positions.len(),
        mesh.sections.len(),
        mesh.uvs.len()
    );
    if mesh.positions.is_empty() {
        return Ok(status);
    }

    let materials = read_static_mesh_materials(pkg, &open.data, export).unwrap_or_default();
    let mut game = library.open(root, path);
    let sections = sections_of_static_mesh(&mesh, &mut t.scene.assets, SectionChunks::Whole, |t, i| {
        let material = *materials.get(i)?;
        let (game, id) = game.as_mut().ok()?;
        material_images(game, t, *id, material)
    });
    let textured = sections.iter().filter(|s| s.texture.is_some()).count();
    let textures = match &game {
        Ok(_) => format!(", {textured} textured"),
        Err(e) => format!(" (no textures: {e})"),
    };
    show_model(&mut t.scene, ModelView { sections, instances: vec![Transform::IDENTITY], zone: None }, min, max);
    Ok(format!("{status}{textures}"))
}

fn show_skeletal_mesh(open: &OpenPackage, index: usize, library: &mut Library, root: &Path, path: &str, t: &mut Targets) -> Result<String, String> {
    let pkg = &open.package;
    let export = &pkg.exports[index];
    let name = pkg.name(export.object_name);
    let mesh = read_skeletal_mesh(pkg, &open.data, export).map_err(|e| format!("{name}: {e}"))?;
    let status = format!(
        "{name}: {} points, {} wedges, {} triangles, {} sections, {} bones (bind pose)",
        mesh.positions.len(),
        mesh.wedges.len(),
        mesh.indices.len() / 3,
        mesh.sections.len(),
        mesh.bones.len()
    );
    let placement = unreal_mesh_transform([0.0; 3].into(), [0; 3].into(), [1.0; 3].into(), [0.0; 3].into(), mesh.scale, mesh.origin, mesh.rot_origin);
    let (min, max) = bounds(mesh.positions.iter().map(|&p| placement.transform_point(Vec3::from(to_bevy(p)))));
    if mesh.positions.is_empty() {
        return Ok(status);
    }
    let mut handles = Vec::new();
    let mut sections = Vec::new();
    if let Ok((game, id)) = library.open(root, path) {
        (sections, handles) = skeletal_mesh_sections(game, &mut t.scene.assets, &mut material_images, &mesh, id, &[], false);
    }
    let textured = sections.iter().filter(|s| s.texture.is_some()).count();
    let animations: Vec<_> = (pkg.exports.iter())
        .filter(|e| pkg.object_name(e.class_index) == Some("MeshAnimation"))
        .filter_map(|e| p2_assets::read_mesh_animation(pkg, &open.data, e).ok().map(std::sync::Arc::new))
        .collect();
    let playing = match animate_mesh_with(pkg, &animations, handles, &std::sync::Arc::new(mesh.clone()), &["walk", "idle"]) {
        Some((item, sequence)) => {
            t.scene.animated.0.push(item);
            format!(", playing {sequence}")
        }
        None => String::new(),
    };
    show_model(&mut t.scene, ModelView { sections, instances: vec![placement], zone: None }, min, max);
    Ok(format!("{status}, {textured} textured{playing}"))
}

fn bounds(points: impl Iterator<Item = Vec3>) -> (Vec3, Vec3) {
    points.fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)))
}

fn show_model(t: &mut SceneTargets, model: ModelView, min: Vec3, max: Vec3) {
    t.view.show(Scene {
        models: vec![model],
        center: (min + max) / 2.0,
        radius: ((max - min).length() / 2.0).max(1.0),
        sky: None,
        start: None,
        collision: None,
        fogs: Vec::new(),
        decals: Vec::new(),
        actors: Vec::new(),
        attachments: Vec::new(),
        lighting: None,
        antiportals: Vec::new(),
        light_maps: Vec::new(),
    });
}

fn collect_packages(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_packages(root, &path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(relative.display().to_string());
        }
    }
    Ok(())
}

fn ui(
    mut targets: Targets,
    mut browser: ResMut<Browser>,
    mut form: ResMut<GameDirForm>,
    mut next: ResMut<NextState<ViewerState>>,
) -> Result {
    let browser = &mut *browser;
    let mut actions = std::mem::take(&mut browser.pending);
    if let Some(url) = targets.scene.play.world_mut().and_then(PlayWorld::take_travel) {
        actions.extend(browser.travel_actions(&url));
    }
    let mut change_game_dir = false;

    let ctx = targets.contexts.ctx_mut()?.clone();
    let mut root = egui::Ui::new(
        ctx.clone(),
        "browser".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    egui::Panel::left("packages").resizable(true).default_size(280.0).show(&mut root, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Packages");
            change_game_dir = ui.button("Game folder…").clicked();
        });
        if ui.text_edit_singleline(&mut browser.package_filter).changed() {
            let filter = browser.package_filter.to_lowercase();
            browser.package_rows = (0..browser.packages.len())
                .filter(|&i| browser.packages[i].to_lowercase().contains(&filter))
                .collect();
        }
        let open = browser.open.as_ref().map(|open| open.index);
        list(ui, "packages", &browser.package_rows, |ui, i| {
            if ui.selectable_label(open == Some(i), &browser.packages[i]).clicked() {
                actions.push(Action::OpenPackage(i));
            }
        });
    });

    if let Some(open) = &mut browser.open {
        egui::Panel::left("assets").resizable(true).default_size(240.0).show(&mut root, |ui| {
            ui.horizontal(|ui| {
                for kind in Kind::ALL {
                    let label = format!("{} ({})", kind.label(), open.assets(kind).len());
                    if ui.selectable_label(browser.kind == kind, label).clicked() {
                        browser.kind = kind;
                    }
                }
            });
            let filter_changed = ui.text_edit_singleline(&mut browser.asset_filter).changed();
            let kind = browser.kind;
            if filter_changed || open.rows_kind != Some(kind) {
                let filter = browser.asset_filter.to_lowercase();
                open.rows = (0..open.assets(kind).len())
                    .filter(|&slot| open.name(kind, slot).to_lowercase().contains(&filter))
                    .collect();
                open.rows_kind = Some(kind);
            }
            list(ui, "assets", &open.rows, |ui, slot| {
                if ui.selectable_label(open.selected == Some((kind, slot)), open.name(kind, slot)).clicked() {
                    actions.push(Action::Show(kind, slot));
                }
            });
        });
    }

    match browser.kind {
        Kind::Texture | Kind::Sound => {
            targets.scene.view.viewport = None;
            targets.input.hovered = false;
            targets.input.keyboard = false;
            egui::CentralPanel::default().show(&mut root, |ui| {
                ui.label(&browser.status);
                if let Some(preview) = &browser.preview {
                    let available = ui.available_size();
                    let scale = (available.x / preview.size.x).min(available.y / preview.size.y);
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(preview.id, preview.size * scale)));
                }
            });
        }
        Kind::StaticMesh | Kind::SkeletalMesh | Kind::Level => {
            let rect = root.available_rect_before_wrap();
            let scale = ctx.pixels_per_point();
            let position = UVec2::new((rect.min.x * scale) as u32, (rect.min.y * scale) as u32);
            let size = UVec2::new((rect.width() * scale) as u32, (rect.height() * scale) as u32);
            targets.scene.view.viewport = (size.x > 0 && size.y > 0).then_some((position, size));
            targets.input.hovered = ctx.pointer_hover_pos().is_some_and(|p| rect.contains(p))
                && ctx.dragged_id().is_none();
            targets.input.keyboard = !ctx.egui_wants_keyboard_input();
            let help = match browser.kind {
                Kind::Level => "Right-drag: look   W/A/S/D: move   Q/E: down/up   Shift: faster   Wheel: speed   G: walk / fly   Space: jump",
                _ => "Left-drag: orbit   Wheel: zoom",
            };
            browser.session.frame(&ctx, rect, &mut targets.contexts, &mut targets.scene);
            egui::Area::new("mesh_status".into()).fixed_pos(rect.min + egui::vec2(8.0, 8.0)).show(&ctx, |ui| {
                egui::Frame::NONE.fill(egui::Color32::from_black_alpha(170)).inner_margin(6.0).show(ui, |ui| {
                    ui.label(&browser.status);
                    if let Some(world) = targets.scene.play.world() {
                        ui.small(world.summary());
                    }
                    ui.small(help);
                });
            });
        }
    }

    for action in actions {
        browser.apply(action, &mut targets);
    }
    if change_game_dir {
        browser.clear_preview(&mut targets);
        browser.session.clear(&mut targets.scene);
        targets.scene.view.viewport = None;
        form.input = browser.root.display().to_string();
        form.error = None;
        next.set(ViewerState::Setup);
    }
    Ok(())
}

fn list(ui: &mut egui::Ui, id: &str, rows: &[usize], mut row: impl FnMut(&mut egui::Ui, usize)) {
    let height = ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().button_padding.y * 2.0;
    egui::ScrollArea::vertical().id_salt(id).auto_shrink(false).show_rows(ui, height, rows.len(), |ui, range| {
        for &i in &rows[range] {
            row(ui, i);
        }
    });
}

fn play_sound(mut commands: Commands, mut browser: ResMut<Browser>, mut sources: ResMut<Assets<AudioSource>>) {
    let Some(data) = browser.sound.take() else { return };
    if let Some(playing) = browser.playing.take() {
        commands.entity(playing).try_despawn();
    }
    let source = sources.add(AudioSource { bytes: data.into() });
    browser.playing = Some(commands.spawn((AudioPlayer::new(source), PlaybackSettings::DESPAWN)).id());
}
