//! Always-on diagnostics: every log event appended to `diagnostics.txt` beside the executable, and the F10 egui overlay.
//! Release builds have no console, so this is the file a player sends.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use bevy::app::{App, AppExit, First, Last, Plugin, PostUpdate, Update};
use bevy::camera::Camera;
use bevy::camera::visibility::{InheritedVisibility, NoCpuCulling, ViewVisibility};
use bevy::core_pipeline::core_3d::{AlphaMask3d, Opaque3d, Transparent3d};
use bevy::core_pipeline::prepass::{AlphaMask3dPrepass, Opaque3dPrepass};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::diagnostic::DiagnosticsStore;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::mesh::Mesh3d;
use bevy::pbr::Shadow;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::render_phase::{BinnedPhaseItem, ViewBinnedRenderPhases, ViewSortedRenderPhases};
use bevy::render::renderer::{RenderGraph, RenderGraphSystems};
use bevy::render::view::ExtractedView;
use bevy::window::Window;
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::error::Result;
use bevy::ecs::prelude::{Has, Local, MessageReader, Query, Res, ResMut, Resource, With, Without};
use bevy::input::ButtonInput;
use bevy::input::keyboard::KeyCode;
use bevy::log::BoxedLayer;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::{Event, Level, Subscriber};
use bevy::log::tracing_subscriber::Layer;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::time::{Real, Time};
use bevy::transform::components::Transform;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};

use crate::display::{MODERN_AO, MODERN_CONTACT_SHADOWS, MODERN_POST, MODERN_RELIEF, MODERN_SHADOWS, MODERN_SPECULAR, Settings};
use crate::map::{Particles, Session};
use crate::components::{FollowActor, FollowBone};
use crate::mesh_view::SceneView;
use crate::play::{FrameTimes, Play, VmProfile};
use crate::render::actor_light::ActorLitMaterial;
use crate::render::mesh_light::MeshLitMaterial;
use crate::render::blended::TerrainLayerMaterial;
use crate::render::terrain_material::TerrainMaterial;
use bevy::light::ClusteredDecal;
use bevy::asset::{Asset, AssetEvent, Assets};
use bevy::ecs::prelude::resource_exists;
use bevy::mesh::Mesh;
use bevy::image::Image;

mod system_spans;
mod stage_timing;

use system_spans::{SpanTiming, SystemSpans};
use stage_timing::a_second_passed;
pub use stage_timing::{FrameWindow, StageTimingPlugin, log_frame_times};
pub(crate) use stage_timing::{MeshWrite, mesh_written};

const MAX_LINES: usize = 500;
const NEAR: f32 = 1500.0;
const NEAREST: usize = 10;
const RECENT: usize = 20;

#[derive(Default)]
struct Lines {
    lines: Vec<(String, u32)>,
    index: HashMap<String, usize>,
    dropped: u32,
    pending: Vec<String>,
}

#[derive(Resource, Clone, Default)]
pub struct DiagnosticsLog {
    lines: Arc<Mutex<Lines>>,
}

impl DiagnosticsLog {
    fn record(&self, level: Level, target: &str, message: &str) {
        let line = file_line(level, target, message);
        if let Ok(mut log) = self.lines.lock() {
            log.pending.push(line);
        }
    }

    /// A panic elsewhere must not lose them, so a poisoned lock still reads (and no code panics while it holds the lock, so
    /// the panic hook cannot wait for itself).
    fn take_pending(&self) -> String {
        let mut text = String::new();
        let mut log = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        for line in log.pending.drain(..) {
            let _ = writeln!(text, "{line}");
        }
        text
    }

    fn flush(&self) {
        append_text(&self.take_pending());
    }

    fn push(&self, line: String) {
        let Ok(mut guard) = self.lines.lock() else { return };
        let log = &mut *guard;
        match log.index.get(&line) {
            Some(&at) => {
                if let Some(entry) = log.lines.get_mut(at) {
                    entry.1 += 1;
                }
            }
            None if log.lines.len() < MAX_LINES => {
                log.index.insert(line.clone(), log.lines.len());
                log.lines.push((line, 1));
            }
            None => log.dropped += 1,
        }
    }
}

fn file_line(level: Level, target: &str, message: &str) -> String {
    let message = message.trim_start().replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "");
    format!("{:.3}\t{level}\t{target}\t{message}", STARTED.get_or_init(Instant::now).elapsed().as_secs_f64())
}

fn file_path() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("diagnostics.txt")))
}

static RUNNING: OnceLock<DiagnosticsLog> = OnceLock::new();

static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn append_now(target: &str, message: &str) {
    append_text(&format!("{}\n", file_line(Level::ERROR, target, message)));
}

fn append_text(text: &str) {
    let Some(path) = file_path() else { return };
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()));
}

pub fn install_panic_hook() {
    STARTED.get_or_init(Instant::now);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(log) = RUNNING.get() {
            log.flush();
        }
        append_now("panic", &format!("{info}\nbacktrace:\n{}", std::backtrace::Backtrace::force_capture()));
        previous(info);
    }));
}

fn shown((line, count): &(String, u32)) -> String {
    if *count > 1 { format!("{line} (x{count})") } else { line.clone() }
}

pub fn capture_layer(app: &mut App) -> Option<BoxedLayer> {
    let log = DiagnosticsLog::default();
    let spans = SystemSpans::default();
    app.insert_resource(log.clone());
    app.insert_resource(spans.clone());
    let _ = RUNNING.set(log.clone());
    Some(Box::new(Capture(log).and_then(SpanTiming(spans))))
}

/// The terminal gets warnings and errors only. Every line is in `diagnostics.txt`, and a line written to a Windows
/// console costs the logging thread a fraction of a millisecond: a burst of lines in one frame was a hitch.
pub fn console_layer(_: &mut App) -> Option<bevy::log::BoxedFmtLayer> {
    use bevy::log::tracing_subscriber::{filter::LevelFilter, fmt};
    Some(Box::new(fmt::Layer::default().with_writer(std::io::stderr).with_filter(LevelFilter::WARN)))
}

struct Capture(DiagnosticsLog);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let meta = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let target = fields.target.as_deref().unwrap_or(meta.target());
        self.0.record(*meta.level(), target, &fields.text);
        // Bevy quits on a render error after this frame's flush has passed, and a quit must not lose its reason.
        if *meta.level() == Level::ERROR {
            self.0.flush();
        }
        if *meta.level() <= Level::WARN {
            self.0.push(format!("{} {target}:{}", meta.level(), fields.text));
        }
    }
}

#[derive(Default)]
struct Fields {
    text: String,
    target: Option<String>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "log.target" {
            self.target = Some(value.to_owned());
        } else {
            self.record_debug(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = match field.name() {
            "message" => write!(self.text, " {value:?}"),
            name if name.starts_with("log.") => Ok(()),
            name => write!(self.text, " {name}={value:?}"),
        };
    }
}

pub struct DiagnosticsPlugin;

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DiagnosticsLog>()
            .init_resource::<Overlay>()
            .add_systems(Update, (log_state, write_file))
            .add_systems(Last, flush_on_exit)
            .add_systems(EguiPrimaryContextPass, overlay);
    }
}

#[derive(Resource, Default)]
struct Overlay {
    open: bool,
    summary: String,
}

fn log_state(session: Option<ResMut<Session>>, play: Res<Play>, mut last: Local<(&'static str, String)>) {
    let Some(mut session) = session else { return };
    let session = session.bypass_change_detection();
    let map = session.map().unwrap_or("none");
    let kind = if session.loading().is_some() {
        "loading"
    } else {
        match play.world() {
            None => "not playing",
            Some(world) if world.shows_mouse() => "menu",
            Some(_) => "playing",
        }
    };
    if last.0 != kind || last.1 != map {
        let state = if kind == "loading" { format!("loading (leaving map {map})") } else { format!("{kind}, map {map}") };
        bevy::log::info!(target: "diagnostics", "state {state}");
        *last = (kind, map.to_owned());
    }
}

fn flush_on_exit(mut exits: MessageReader<AppExit>, log: Res<DiagnosticsLog>) {
    if exits.read().next().is_some() {
        log.flush();
    }
}

/// The file stays open between writes (opening it is what a virus scanner or a slow disk holds up), in append mode:
/// [`append_now`] and the flushes write beside it.
fn write_file(
    time: Res<Time<Real>>,
    log: Res<DiagnosticsLog>,
    particles: Option<Res<Particles>>,
    mut overlay: ResMut<Overlay>,
    mut since: Local<Option<(f32, u32)>>,
    mut file: Local<Option<std::fs::File>>,
) {
    let first = since.is_none();
    let (seconds, frames) = since.get_or_insert((0.0, 0));
    *frames += 1;
    if !a_second_passed(seconds, time.delta_secs()) && !first {
        return;
    }
    let ms = *seconds * 1000.0 / *frames as f32;
    (*seconds, *frames) = (0.0, 0);
    let (actors, live) = particles.map_or((0, 0), |particles| particles.particle_counts());
    overlay.summary = format!("frame {ms:.1} ms ({:.0} fps) | particle actors {actors}, live particles {live}", 1000.0 / ms.max(0.001));
    log.record(Level::INFO, "diagnostics", &format!("summary frame_ms={ms:.1} particle_actors={actors} live_particles={live}"));
    let text = log.take_pending();
    let Some(path) = file_path() else { return };
    if file.is_none() {
        let emptied = if first { std::fs::File::create(&path).map(drop) } else { Ok(()) };
        match emptied.and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path)) {
            Ok(opened) => *file = Some(opened),
            Err(error) => bevy::log::warn_once!("opening {}: {error}", path.display()),
        }
    }
    if let Some(file) = file.as_mut()
        && let Err(error) = std::io::Write::write_all(file, text.as_bytes())
    {
        bevy::log::warn_once!("writing {}: {error}", path.display());
    }
}

fn overlay(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: ResMut<Overlay>,
    log: Res<DiagnosticsLog>,
    particles: Option<Res<Particles>>,
    (play, mut times): (Res<Play>, ResMut<FrameTimes>),
    cameras: Query<&Transform, With<SceneView>>,
) -> Result {
    if keys.just_pressed(KeyCode::F10) {
        overlay.open = !overlay.open;
    }
    if !overlay.open {
        return Ok(());
    }
    let near = match (particles, play.world(), cameras.iter().next()) {
        (Some(particles), Some(world), Some(camera)) => particles.particles_in_view(world, camera, NEAR, NEAREST),
        _ => Vec::new(),
    };
    let recent: Vec<String> = log.lines.lock().map(|log| log.lines.iter().skip(log.lines.len().saturating_sub(RECENT)).map(shown).collect()).unwrap_or_default();
    let flag = |on: bool, name: &'static str| if on { name } else { "" };
    let mut profile = times.vm_profile;
    egui::Window::new("Diagnostics (F10)").default_pos([8.0, 40.0]).default_width(600.0).show(contexts.ctx_mut()?, |ui| {
        ui.label(overlay.summary.as_str());
        ui.horizontal(|ui| {
            ui.label("VM profile in the log (costs VM time):");
            ui.radio_value(&mut profile, VmProfile::Off, "off");
            ui.radio_value(&mut profile, VmProfile::TickByClass, "tick by class");
            ui.radio_value(&mut profile, VmProfile::Work, "work and functions");
        });
        ui.separator();
        ui.label(format!("Particle emitter actors within {NEAR} units, nearest the view direction first:"));
        egui::ScrollArea::vertical().id_salt("near").max_height(320.0).show(ui, |ui| {
            for report in &near {
                let slot = report.overlay.map(|slot| format!(", overlay {slot}")).unwrap_or_default();
                ui.monospace(format!(
                    "{} at {:.0}{}{}{}{slot}",
                    report.name,
                    report.distance,
                    flag(report.hidden, ", hidden"),
                    flag(report.unseen, ", not drawn"),
                    flag(report.zone_culled, ", zone-culled")
                ));
                for emitter in &report.emitters {
                    ui.monospace(format!("    {emitter}"));
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("log").max_height(240.0).show(ui, |ui| {
            for line in &recent {
                ui.monospace(line.as_str());
            }
        });
    });
    if times.vm_profile != profile {
        times.vm_profile = profile;
    }
    Ok(())
}
