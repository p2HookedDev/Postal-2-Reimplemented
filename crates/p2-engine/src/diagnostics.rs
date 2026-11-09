//! Always-on diagnostics: every log event appended to `diagnostics.txt` beside the executable, and the F10 egui overlay.
//! Release builds have no console, so this is the file a player sends.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use bevy::app::{App, AppExit, First, Last, Plugin, PostUpdate, Update};
use bevy::camera::Camera;
use bevy::camera::visibility::ViewVisibility;
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
use bevy::ecs::prelude::{Has, Local, MessageReader, Query, Res, ResMut, Resource, With};
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
use crate::map::SessionHolder;
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
    app.insert_resource(log.clone());
    let _ = RUNNING.set(log.clone());
    Some(Box::new(Capture(log)))
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

pub struct DiagnosticsPlugin<H>(PhantomData<fn() -> H>);

impl<H> Default for DiagnosticsPlugin<H> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<H: SessionHolder> Plugin for DiagnosticsPlugin<H> {
    fn build(&self, app: &mut App) {
        app.init_resource::<DiagnosticsLog>()
            .init_resource::<Overlay>()
            .add_systems(Update, (log_state::<H>, write_file::<H>))
            .add_systems(Last, flush_on_exit)
            .add_systems(EguiPrimaryContextPass, overlay::<H>);
    }
}

#[derive(Resource, Default)]
struct Overlay {
    open: bool,
    summary: String,
}

fn log_state<H: SessionHolder>(holder: Option<ResMut<H>>, play: Res<Play>, mut last: Local<(&'static str, String)>) {
    let Some(mut holder) = holder else { return };
    let session = holder.bypass_change_detection().session();
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
fn write_file<H: SessionHolder>(
    time: Res<Time<Real>>,
    log: Res<DiagnosticsLog>,
    holder: Option<ResMut<H>>,
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
    let (actors, live) = holder.map_or((0, 0), |mut holder| holder.bypass_change_detection().session().particle_counts());
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

fn overlay<H: SessionHolder>(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: ResMut<Overlay>,
    log: Res<DiagnosticsLog>,
    holder: Option<ResMut<H>>,
    (play, mut times): (Res<Play>, ResMut<FrameTimes>),
    cameras: Query<&Transform, With<SceneView>>,
) -> Result {
    if keys.just_pressed(KeyCode::F10) {
        overlay.open = !overlay.open;
    }
    if !overlay.open {
        return Ok(());
    }
    let near = match (holder, play.world(), cameras.iter().next()) {
        (Some(mut holder), Some(world), Some(camera)) => holder.bypass_change_detection().session().particles_in_view(world, camera, NEAR, NEAREST),
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

/// With pipelined rendering, time waiting for the GPU falls in the render stage.
pub struct StageTimingPlugin;

const MAIN_STAGES: [&str; 3] = ["clusters", "cascades and frusta", "light visibility"];
const UPDATE_STAGES: [&str; 2] = ["whole update", "wait for render thread and extract"];
const RENDER_STAGES: [&str; 7] = ["wait for main and extract", "assets and views", "specialize", "queue", "sort and prepare", "bind groups", "render"];
const GRAPH_STAGES: [&str; 4] = ["all cameras", "finish encoders and submit", "gpu timers", "last submit and present"];
const PASS_STAGES: [&str; 3] = ["shadows and prepass", "main", "post"];

#[derive(Default)]
struct StageClock {
    last: Option<Instant>,
    spent: Vec<f64>,
    frames: u32,
}

#[derive(Resource, Clone, Default)]
struct StageClocks {
    main: Arc<Mutex<StageClock>>,
    update: Arc<Mutex<StageClock>>,
    render: Arc<Mutex<StageClock>>,
    graph: Arc<Mutex<StageClock>>,
    passes: Arc<Mutex<StageClock>>,
    draw_groups: Arc<Mutex<[usize; 8]>>,
}

#[derive(Resource, Clone, Default)]
struct RenderStageClock(Arc<Mutex<StageClock>>);

#[derive(Resource, Clone, Default)]
struct RenderGraphClock(Arc<Mutex<StageClock>>);

#[derive(Resource, Clone, Default)]
struct RenderPassClock(Arc<Mutex<StageClock>>);

#[derive(Resource, Clone, Default)]
struct RenderDrawGroups(Arc<Mutex<[usize; 8]>>);

fn tick(clock: &Mutex<StageClock>, stage: Option<usize>) {
    let mut clock = clock.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    if let (Some(stage), Some(last)) = (stage, clock.last) {
        if clock.spent.len() <= stage {
            clock.spent.resize(stage + 1, 0.0);
        }
        clock.spent[stage] += now.duration_since(last).as_secs_f64() * 1000.0;
    }
    clock.last = Some(now);
    if stage.is_none() {
        clock.frames += 1;
    }
}

fn main_mark<const STAGE: i32>(clocks: Res<StageClocks>) {
    tick(&clocks.main, usize::try_from(STAGE).ok());
}

fn update_mark<const STAGE: i32>(clocks: Res<StageClocks>) {
    tick(&clocks.update, usize::try_from(STAGE).ok());
}

fn render_mark<const STAGE: i32>(clock: Res<RenderStageClock>) {
    tick(&clock.0, usize::try_from(STAGE).ok());
}

fn graph_mark<const STAGE: i32>(clock: Res<RenderGraphClock>) {
    tick(&clock.0, usize::try_from(STAGE).ok());
}

/// The core 3D schedule runs once per camera: a frame's time is the sum over its cameras.
fn pass_mark<const STAGE: i32>(clock: Res<RenderPassClock>) {
    tick(&clock.0, usize::try_from(STAGE).ok());
}

fn binned<P: BinnedPhaseItem>(phases: &ViewBinnedRenderPhases<P>) -> usize {
    phases.values().map(|phase| phase.multidrawable_meshes.len() + phase.batchable_meshes.len() + phase.unbatchable_meshes.len() + phase.non_mesh_items.len()).sum()
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn count_draw_groups(
    groups: Res<RenderDrawGroups>,
    opaque: Res<ViewBinnedRenderPhases<Opaque3d>>,
    masked: Res<ViewBinnedRenderPhases<AlphaMask3d>>,
    prepass: Res<ViewBinnedRenderPhases<Opaque3dPrepass>>,
    masked_prepass: Res<ViewBinnedRenderPhases<AlphaMask3dPrepass>>,
    shadow: Res<ViewBinnedRenderPhases<Shadow>>,
    blended: Res<ViewSortedRenderPhases<Transparent3d>>,
    views: Query<(), With<ExtractedView>>,
) {
    *groups.0.lock().unwrap_or_else(PoisonError::into_inner) = [
        binned(&opaque),
        binned(&masked),
        binned(&prepass),
        binned(&masked_prepass),
        binned(&shadow),
        blended.values().map(|phase| phase.items.len()).sum(),
        views.iter().len(),
        shadow.values().filter(|phase| !phase.is_empty()).count(),
    ];
}

fn stage_list(clock: &Mutex<StageClock>, names: &[&str], frames: u32) -> String {
    let mut clock = clock.lock().unwrap_or_else(PoisonError::into_inner);
    let frames = f64::from(frames.max(1));
    let list: Vec<String> = names.iter().zip(&clock.spent).map(|(name, spent)| format!("{name} {:.2}", spent / frames)).collect();
    clock.spent.clear();
    clock.frames = 0;
    list.join(", ")
}

fn frames_of(clock: &Mutex<StageClock>) -> u32 {
    clock.lock().unwrap_or_else(PoisonError::into_inner).frames
}

fn a_second_passed(since: &mut f32, delta: f32) -> bool {
    *since += delta;
    *since >= 1.0
}

#[derive(Default)]
pub struct FrameWindow {
    seconds: f32,
    frames: u32,
    shortest: f32,
    longest: f32,
}

pub fn log_frame_times(mut times: ResMut<FrameTimes>, time: Res<Time<Real>>, mut since: Local<FrameWindow>) {
    let delta = time.delta_secs();
    if since.frames == 0 {
        (since.shortest, since.longest) = (delta, delta);
    }
    since.frames += 1;
    (since.shortest, since.longest) = (since.shortest.min(delta), since.longest.max(delta));
    if !a_second_passed(&mut since.seconds, delta) {
        return;
    }
    let frames = f64::from(since.frames);
    let ms = |time: &std::time::Duration| time.as_secs_f64() * 1000.0;
    let list = |parts: &mut dyn Iterator<Item = (&str, std::time::Duration)>, per: f64| {
        parts.map(|(name, time)| format!("{name} {:.2}", ms(&time) / per)).collect::<Vec<_>>().join(", ")
    };
    bevy::log::info!(
        "frames {:.2} to {:.2} ms; per frame (ms), main thread: {}; vm thread: {}",
        since.shortest * 1000.0,
        since.longest * 1000.0,
        list(&mut times.totals.iter().map(|(n, t)| (*n, *t)), frames),
        list(&mut times.vm_totals.iter().map(|(n, t)| (*n, *t)), frames)
    );
    if times.vm_profile == VmProfile::TickByClass {
        let mut vm: Vec<_> = times.vm_parts.drain().collect();
        vm.sort_by(|(a, a_total), (b, b_total)| b_total.cmp(a_total).then_with(|| a.cmp(b)));
        bevy::log::info!("vm tick by class and phase (ms per frame): {}", list(&mut vm.iter().take(10).map(|(n, t)| (n.as_str(), *t)), frames));
    }
    if let Some(mut slowest) = times.take_slowest() {
        slowest.main.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        slowest.vm.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        let named: std::time::Duration = slowest.main.iter().map(|(_, time)| *time).sum();
        bevy::log::info!(
            "slowest frame {:.2} ms: main thread: {}, rest (Bevy, render) {:.2}; its vm frame: {}{}",
            ms(&slowest.length),
            list(&mut slowest.main.iter().take(6).map(|(n, t)| (*n, *t)), 1.0),
            ms(&slowest.length.saturating_sub(named)),
            list(&mut slowest.vm.iter().map(|(n, t)| (*n, *t)), 1.0),
            if slowest.profile.is_empty() {
                String::new()
            } else {
                format!("; vm tick: {}", list(&mut slowest.profile.iter().take(8).map(|(n, t)| (n.as_str(), *t)), 1.0))
            }
        );
    }
    times.totals.clear();
    times.vm_totals.clear();
    *since = Default::default();
}

fn log_stage_timing(clocks: Res<StageClocks>, time: Res<Time<Real>>, mut since: Local<f32>) {
    if a_second_passed(&mut since, time.delta_secs()) {
        *since = 0.0;
        let render_frames = frames_of(&clocks.render);
        let main_frames = frames_of(&clocks.main);
        let update = stage_list(&clocks.update, &UPDATE_STAGES, main_frames);
        let main = stage_list(&clocks.main, &MAIN_STAGES, main_frames);
        let render = stage_list(&clocks.render, &RENDER_STAGES, render_frames);
        let graph = stage_list(&clocks.graph, &GRAPH_STAGES, render_frames);
        bevy::log::info!("cpu ms per frame: {update}; {main}; render world: {render}; of render: {graph}; recording: {}", stage_list(&clocks.passes, &PASS_STAGES, render_frames));
    }
}

/// A changed asset is a queued write, which `finish encoders and submit` pays for.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn log_asset_churn(
    time: Res<Time<Real>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    actor_materials: Res<Assets<ActorLitMaterial>>,
    mesh_materials: Res<Assets<MeshLitMaterial>>,
    terrain_passes: Option<Res<Assets<TerrainMaterial>>>,
    terrain_layers: Option<Res<Assets<TerrainLayerMaterial>>>,
    decals: Query<(), With<ClusteredDecal>>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut actor_events: MessageReader<AssetEvent<ActorLitMaterial>>,
    mut mesh_material_events: MessageReader<AssetEvent<MeshLitMaterial>>,
    mut counts: Local<[u32; 3]>,
    mut since: Local<f32>,
) {
    fn changed<A: Asset>(events: &mut MessageReader<AssetEvent<A>>) -> u32 {
        events.read().filter(|event| matches!(event, AssetEvent::Modified { .. })).count() as u32
    }
    counts[0] += changed(&mut mesh_events);
    counts[1] += changed(&mut actor_events);
    counts[2] += changed(&mut mesh_material_events);
    if !a_second_passed(&mut since, time.delta_secs()) {
        return;
    }
    let per_second = 1.0 / std::mem::take(&mut *since);
    let [mesh_writes, actor_writes, material_writes] = std::mem::take(&mut *counts).map(|count| (count as f32 * per_second) as u32);
    bevy::log::info!(
        "assets, changed a second: meshes {} ({mesh_writes}), images {}, actor materials {} ({actor_writes}), mesh materials {} ({material_writes}); one-pass terrains {}, terrain layer passes {}, decals {}",
        meshes.len(),
        images.len(),
        actor_materials.len(),
        mesh_materials.len(),
        terrain_passes.map_or(0, |store| store.len()),
        terrain_layers.map_or(0, |store| store.len()),
        decals.iter().count()
    );
}

fn log_scene_counts(
    clocks: Res<StageClocks>,
    settings: Option<Res<Settings>>,
    time: Res<Time<Real>>,
    meshes: Query<(&ViewVisibility, Has<FollowActor>, Has<FollowBone>), With<Mesh3d>>,
    cameras: Query<&Camera>,
    windows: Query<&Window>,
    mut since: Local<f32>,
) {
    if let Some(settings) = settings.as_ref().filter(|settings| settings.is_changed()) {
        bevy::log::info!(
            "settings: modern {}, sun shadows {}, contact {}, ao {}, relief {}, specular {}, post {}, msaa {}",
            settings.modern_graphics(),
            settings.modern_effect(MODERN_SHADOWS),
            settings.modern_effect(MODERN_CONTACT_SHADOWS),
            settings.modern_effect(MODERN_AO),
            settings.modern_effect(MODERN_RELIEF),
            settings.modern_effect(MODERN_SPECULAR),
            settings.modern_effect(MODERN_POST),
            settings.msaa_samples()
        );
    }
    if !a_second_passed(&mut since, time.delta_secs()) {
        return;
    }
    *since = 0.0;
    let shown = |visible: &ViewVisibility| visible.get();
    let window = windows.iter().next().map_or([0, 0], |window| [window.physical_width(), window.physical_height()]);
    let [opaque, masked, prepass, masked_prepass, shadow, blended, views, shadow_views_drawing] = *clocks.draw_groups.lock().unwrap_or_else(PoisonError::into_inner);
    let movers = meshes.iter().filter(|(_, actor, bone)| *actor || *bone);
    let (movers_all, movers_shown) = movers.fold((0, 0), |(all, on), (visible, ..)| (all + 1, on + usize::from(shown(visible))));
    bevy::log::info!(
        "scene: {} mesh entities, {} of them shown, movers {movers_all} with {movers_shown} shown; {views} views, {shadow_views_drawing} shadow views with something to draw; {} cameras active; window {} x {}; draw groups: opaque {opaque}, masked {masked}, prepass {prepass}, masked prepass {masked_prepass}, shadow {shadow}, blended {blended}",
        meshes.iter().len(),
        meshes.iter().filter(|(visible, ..)| shown(visible)).count(),
        cameras.iter().filter(|camera| camera.is_active).count(),
        window[0],
        window[1],
    );
}

/// A pass runs once per camera and each run adds a sample, so a path's time per frame is its samples' rate over the frame rate.
fn log_gpu_timing(store: Option<Res<DiagnosticsStore>>, time: Res<Time<Real>>, mut since: Local<f32>, mut frames: Local<u32>) {
    *frames += 1;
    if !a_second_passed(&mut since, time.delta_secs()) {
        return;
    }
    let frames_per_second = f64::from(std::mem::take(&mut *frames)) / f64::from(std::mem::take(&mut *since));
    let Some(store) = store else { return };
    let mut list: Vec<(f64, String)> = store
        .iter()
        .filter_map(|diagnostic| {
            let name = diagnostic.path().as_str().strip_prefix("render/")?.strip_suffix("/elapsed_gpu")?;
            let (first, last) = (diagnostic.measurements().next()?, diagnostic.measurements().last()?);
            let seconds = last.time.duration_since(first.time).as_secs_f64();
            let spent: f64 = diagnostic.measurements().skip(1).map(|sample| sample.value).sum();
            (seconds > 0.0).then(|| (spent / seconds / frames_per_second, name.to_owned()))
        })
        .collect();
    list.sort_by(|a, b| b.0.total_cmp(&a.0));
    list.truncate(14);
    let list: Vec<String> = list.iter().map(|(ms, name)| format!("{name} {ms:.2}")).collect();
    bevy::log::info!("gpu ms per frame by pass, nested passes overlap: {}", list.join(", "));
}

impl Plugin for StageTimingPlugin {
    fn build(&self, app: &mut App) {
        use bevy::light::SimulationLightSystems as Light;
        use bevy::render::RenderSystems as Stage;
        let clocks = StageClocks::default();
        let render_clock = RenderStageClock(clocks.render.clone());
        let graph_clock = RenderGraphClock(clocks.graph.clone());
        let pass_clock = RenderPassClock(clocks.passes.clone());
        let draw_groups = RenderDrawGroups(clocks.draw_groups.clone());
        app.insert_resource(clocks);
        app.add_plugins(RenderDiagnosticsPlugin);
        app.add_systems(Update, (log_stage_timing, log_scene_counts, log_gpu_timing));
        app.add_systems(Update, log_asset_churn.run_if(resource_exists::<Assets<ActorLitMaterial>>).run_if(resource_exists::<Assets<MeshLitMaterial>>));
        app.add_systems(First, update_mark::<1>);
        app.add_systems(Last, update_mark::<0>);
        app.add_systems(
            PostUpdate,
            (
                main_mark::<-1>.before(Light::AssignLightsToClusters),
                main_mark::<0>.after(Light::AssignLightsToClusters).before(Light::UpdateDirectionalLightCascades),
                main_mark::<1>.after(Light::UpdateLightFrusta).before(Light::CheckLightVisibility),
                main_mark::<2>.after(Light::CheckLightVisibility),
            ),
        );
        let Some(render) = app.get_sub_app_mut(bevy::render::RenderApp) else { return };
        render.insert_resource(render_clock);
        render.insert_resource(graph_clock);
        render.insert_resource(pass_clock);
        render.insert_resource(draw_groups);
        render.add_systems(bevy::render::Render, count_draw_groups.after(Stage::PhaseSort).before(Stage::Render));
        render.add_systems(
            RenderGraph,
            (
                graph_mark::<-1>.before(RenderGraphSystems::Begin),
                graph_mark::<0>.after(RenderGraphSystems::Render).before(RenderGraphSystems::Submit),
                graph_mark::<1>.after(RenderGraphSystems::Submit).before(RenderGraphSystems::Finish),
                graph_mark::<2>.after(RenderGraphSystems::Finish),
            ),
        );
        render.add_systems(
            Core3d,
            (
                pass_mark::<-1>.before(Core3dSystems::Prepass),
                pass_mark::<0>.after(Core3dSystems::Prepass).before(Core3dSystems::MainPass),
                pass_mark::<1>.after(Core3dSystems::MainPass).before(Core3dSystems::EarlyPostProcess),
                pass_mark::<2>.after(Core3dSystems::PostProcess),
            ),
        );
        render.add_systems(
            bevy::render::Render,
            (
                render_mark::<0>.before(Stage::ExtractCommands),
                render_mark::<1>.after(Stage::CreateViews).before(Stage::Specialize),
                render_mark::<2>.after(Stage::Specialize).before(Stage::PrepareViews),
                render_mark::<3>.after(Stage::QueueSweep).before(Stage::PhaseSort),
                render_mark::<4>.after(Stage::PrepareResourcesFlush).before(Stage::PrepareBindGroups),
                render_mark::<5>.after(Stage::PrepareBindGroups).before(Stage::Render),
                render_mark::<6>.after(Stage::Render).before(Stage::Cleanup),
                graph_mark::<3>.after(Stage::Render).before(Stage::Cleanup),
                render_mark::<-1>.after(Stage::PostCleanup),
            ),
        );
    }
}
