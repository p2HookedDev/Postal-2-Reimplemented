use std::fmt::Write as _;
use std::path::PathBuf;

use bevy::asset::Assets;
use bevy::ecs::change_detection::{DetectChangesMut, Mut};
use bevy::ecs::prelude::{Commands, Query, Res, ResMut, Resource, With};
use bevy::ecs::system::SystemParam;
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::Mesh;
use bevy::pbr::StandardMaterial;
use bevy::render::renderer::RenderDevice;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::transform::components::Transform;
use bevy::window::{PrimaryWindow, Window};
use p2_bevy_assets::to_bevy;
use p2_host::settings::Settings;
use p2_render::post::debug_view::DebugView;
use p2_render::world::view::{SceneView, ShownScene};

use super::stage_timing::settings_line;
use super::{DiagnosticsLog, Overlay, file_path, state_kind};
use crate::map::Session;
use crate::play::Play;

const SETTLE_FRAMES: u32 = 20;
const NEAREST_LAMPS: usize = 40;
const SETTINGS_FILE: &str = "Settings.ini";
const MB: f64 = 1024.0 * 1024.0;

#[derive(Resource, Default)]
pub(super) struct Dump {
    pub(super) requested: bool,
    pub(super) run: Option<Run>,
}

pub(super) struct Run {
    folder: PathBuf,
    owner_view: Option<DebugView>,
    overlay_open: bool,
    title: String,
    shot: usize,
    frames: u32,
}

#[derive(SystemParam)]
pub(super) struct ReportSources<'w, 's> {
    session: Option<Res<'w, Session>>,
    play: Res<'w, Play>,
    settings: Option<Res<'w, Settings>>,
    shown: Option<Res<'w, ShownScene>>,
    cameras: Query<'w, 's, (&'static SceneView, &'static Transform)>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<Image>>,
    materials: Res<'w, Assets<StandardMaterial>>,
    device: Option<Res<'w, RenderDevice>>,
}

pub(super) fn advance_dump(
    mut dump: ResMut<Dump>,
    mut overlay: ResMut<Overlay>,
    mut view: Option<ResMut<DebugView>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    log: Res<DiagnosticsLog>,
    sources: ReportSources<'_, '_>,
    mut commands: Commands,
) {
    let shots = if view.is_some() { DebugView::ALL.len() } else { 1 };
    let mut window = windows.iter_mut().next();
    if std::mem::take(&mut dump.requested) && dump.run.is_none() {
        dump.run = start(&sources, &overlay.summary).map(|folder| Run {
            folder,
            owner_view: view.as_deref().copied(),
            overlay_open: overlay.open,
            title: window.as_ref().map(|window| window.title.clone()).unwrap_or_default(),
            shot: 0,
            frames: 0,
        });
        if dump.run.is_some() {
            overlay.open = false;
        }
    }
    let Some(run) = dump.run.as_mut() else { return };
    run.frames += 1;
    if run.frames == 1 && run.shot < shots {
        show_view(view.as_mut(), run.shot);
        show_progress(window.as_mut(), run, shots);
    }
    if run.frames <= SETTLE_FRAMES {
        return;
    }
    run.frames = 0;
    if run.shot < shots {
        let name = shot_name(run.shot, DebugView::ALL[run.shot]);
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(run.folder.join(name)));
        run.shot += 1;
        return;
    }
    if let (Some(view), Some(owner_view)) = (view.as_mut(), run.owner_view) {
        view.set_if_neq(owner_view);
    }
    if let Some(window) = window.as_mut() {
        window.title.clone_from(&run.title);
    }
    overlay.open = run.overlay_open;
    log.flush();
    if let Some(diagnostics) = file_path() {
        copy_into(&diagnostics, &run.folder);
    }
    bevy::log::info!(target: "diagnostics", "dump written to {}", run.folder.display());
    dump.run = None;
}

fn show_view(view: Option<&mut ResMut<DebugView>>, shot: usize) {
    if let Some(view) = view {
        view.set_if_neq(DebugView::ALL[shot]);
    }
}

fn show_progress(window: Option<&mut Mut<Window>>, run: &Run, shots: usize) {
    if let Some(window) = window {
        window.title = format!("{} - dumping {} of {shots}", run.title, run.shot + 1);
    }
}

fn start(sources: &ReportSources<'_, '_>, summary: &str) -> Option<PathBuf> {
    let now = chrono::Local::now();
    let exe = std::env::current_exe().ok()?;
    let folder = exe.parent()?.join("dumps").join(folder_name(&now.naive_local()));
    if let Err(error) = std::fs::create_dir_all(&folder) {
        bevy::log::warn!("dump folder {}: {error}", folder.display());
        return None;
    }
    let report = folder.join("report.txt");
    let text = sources.report(&now.format("%Y-%m-%d %H:%M:%S").to_string(), summary).unwrap_or_default();
    if let Err(error) = std::fs::write(&report, text) {
        bevy::log::warn!("writing {}: {error}", report.display());
    }
    if let Some(settings) = p2_host::game_dir::config_folder().map(|config| config.join(SETTINGS_FILE)) {
        copy_into(&settings, &folder);
    }
    Some(folder)
}

fn copy_into(file: &std::path::Path, folder: &std::path::Path) {
    let Some(name) = file.file_name() else { return };
    if let Err(error) = std::fs::copy(file, folder.join(name)) {
        bevy::log::warn!("copying {} into the dump: {error}", file.display());
    }
}

fn folder_name(time: &chrono::NaiveDateTime) -> String {
    time.format("%Y-%m-%d_%H-%M-%S").to_string()
}

fn shot_name(index: usize, view: DebugView) -> String {
    format!("{index:02}-{}.png", view.name().replace(' ', "-"))
}

impl ReportSources<'_, '_> {
    fn report(&self, time: &str, summary: &str) -> Result<String, std::fmt::Error> {
        let mut text = String::new();
        writeln!(text, "time {time}")?;
        match &self.session {
            Some(session) => writeln!(text, "state {}, map {}", state_kind(session, &self.play), session.map().unwrap_or("none"))?,
            None => writeln!(text, "state: no session")?,
        }
        let origin = self.shown.as_ref().map_or(Vec3::ZERO, |shown| shown.origin);
        let camera = self.cameras.iter().next();
        let eye = camera.map(|(view, _)| Vec3::from(to_bevy(view.position - origin)));
        match camera {
            Some((view, transform)) => {
                let forward = Vec3::from(to_bevy(view.rotation * Vec3::NEG_Z));
                writeln!(text, "camera bevy: position {}, rotation {}; transform translation {}, rotation {}", view.position, view.rotation, transform.translation, transform.rotation)?;
                writeln!(text, "camera unreal: location {}, forward {forward}", eye.unwrap_or_default())?;
            }
            None => writeln!(text, "camera: none")?,
        }
        match &self.settings {
            Some(settings) => writeln!(text, "settings: {}", settings_line(settings))?,
            None => writeln!(text, "settings: not loaded")?,
        }
        writeln!(text, "{summary}")?;
        self.write_lights(&mut text, eye.unwrap_or_default())?;
        let image_bytes: usize = self.images.iter().filter_map(|(_, image)| image.data.as_ref()).map(Vec::len).sum();
        writeln!(
            text,
            "assets: meshes {}, images {} holding {:.1} MB of CPU-side data, standard materials {}",
            self.meshes.len(),
            self.images.len(),
            image_bytes as f64 / MB,
            self.materials.len()
        )?;
        match self.device.as_ref().and_then(|device| device.wgpu_device().generate_allocator_report()) {
            Some(gpu) => writeln!(text, "gpu memory: allocated {:.1} MB, reserved {:.1} MB", gpu.total_allocated_bytes as f64 / MB, gpu.total_reserved_bytes as f64 / MB)?,
            None => writeln!(text, "gpu memory: not available")?,
        }
        Ok(text)
    }

    fn write_lights(&self, text: &mut String, eye: Vec3) -> std::fmt::Result {
        let Some(environment) = self.shown.as_ref().and_then(|shown| shown.lighting.as_ref()) else {
            return writeln!(text, "lights: no map lighting (no map shown, or modern graphics off)");
        };
        let at = |light: &p2_render_cpu::MeshLight| Vec3::new(light.location.x, light.location.y, light.location.z);
        let mut lamps: Vec<_> = environment.lights.iter().map(|(index, light)| (*index, light, at(light).distance(eye))).collect();
        lamps.sort_by(|a, b| a.2.total_cmp(&b.2));
        writeln!(text, "map lights: {}, the nearest {NEAREST_LAMPS} to the camera (unreal units):", lamps.len())?;
        for (index, light, distance) in lamps.iter().take(NEAREST_LAMPS) {
            writeln!(
                text,
                "  #{index} at {} radius {:.0} brightness {:.1} effect {:?} type {:?} distance {distance:.0}{}",
                at(light),
                light.radius,
                light.brightness,
                light.effect,
                light.light_type,
                if light.dynamic { " dynamic" } else { "" }
            )?;
        }
        writeln!(text, "level brightness {}; brightest sun {:?}", environment.level_brightness, environment.sun.as_ref().map(|(sun, _)| sun))?;
        for sun in &environment.suns {
            writeln!(text, "  sun at {} rotation {:?} brightness {:.1} hue {} saturation {}", at(sun), sun.rotation, sun.brightness, sun.hue, sun.saturation)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_named_for_its_time_in_sortable_order() {
        let time = chrono::NaiveDate::from_ymd_opt(2026, 11, 5).and_then(|date| date.and_hms_opt(7, 8, 9)).expect("a valid time");
        assert_eq!(folder_name(&time), "2026-11-05_07-08-09");
    }

    #[test]
    fn shots_are_numbered_and_named_with_dashes() {
        assert_eq!(shot_name(0, DebugView::Off), "00-off.png");
        assert_eq!(shot_name(6, DebugView::Bounce), "06-bounce-and-probes.png");
    }
}
