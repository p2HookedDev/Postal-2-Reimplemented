use super::*;

/// With pipelined rendering, time waiting for the GPU falls in the render stage.
pub struct StageTimingPlugin;

const MAIN_STAGES: [&str; 6] = ["transforms", "bounds and frusta", "camera culling", "clusters", "cascades and frusta", "light visibility"];
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

pub(super) fn a_second_passed(since: &mut f32, delta: f32) -> bool {
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
    let ticks = std::mem::take(&mut times.vm_frames);
    let per_tick = f64::from(ticks.max(1));
    let ms = |time: &std::time::Duration| time.as_secs_f64() * 1000.0;
    let list = |parts: &mut dyn Iterator<Item = (&str, std::time::Duration)>, per: f64| {
        parts.map(|(name, time)| format!("{name} {:.2}", ms(&time) / per)).collect::<Vec<_>>().join(", ")
    };
    bevy::log::info!(
        "{} frames {:.2} to {:.2} ms, {ticks} game ticks; per frame (ms), main thread: {}; per game tick (ms), vm thread: {}",
        since.frames,
        since.shortest * 1000.0,
        since.longest * 1000.0,
        list(&mut times.totals.iter().map(|(n, t)| (*n, *t)), frames),
        list(&mut times.vm_totals.iter().map(|(n, t)| (*n, *t)), per_tick)
    );
    if times.vm_profile == VmProfile::TickByClass {
        let mut vm: Vec<_> = times.vm_parts.drain().collect();
        vm.sort_by(|(a, a_total), (b, b_total)| b_total.cmp(a_total).then_with(|| a.cmp(b)));
        bevy::log::info!("vm tick by class and phase (ms per game tick): {}", list(&mut vm.iter().take(16).map(|(n, t)| (n.as_str(), *t)), per_tick));
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

fn log_stage_timing(clocks: Res<StageClocks>, spans: Option<Res<SystemSpans>>, time: Res<Time<Real>>, mut since: Local<f32>) {
    if a_second_passed(&mut since, time.delta_secs()) {
        *since = 0.0;
        let render_frames = frames_of(&clocks.render);
        let main_frames = frames_of(&clocks.main);
        let [slowest, longest] = spans.map(|spans| spans.take_slowest(60, 10, main_frames)).unwrap_or_default();
        if !slowest.is_empty() {
            let list = |systems: &[(Arc<str>, f64)]| systems.iter().map(|(name, ms)| format!("{name} {ms:.3}")).collect::<Vec<_>>().join(", ");
            bevy::log::info!("systems ms per frame, all threads: {}", list(&slowest));
            bevy::log::info!("longest single run of a system this second (ms): {}", list(&longest));
        }
        let update = stage_list(&clocks.update, &UPDATE_STAGES, main_frames);
        let main = stage_list(&clocks.main, &MAIN_STAGES, main_frames);
        let render = stage_list(&clocks.render, &RENDER_STAGES, render_frames);
        let graph = stage_list(&clocks.graph, &GRAPH_STAGES, render_frames);
        bevy::log::info!("cpu ms per frame: {update}; {main}; render world: {render}; of render: {graph}; recording: {}", stage_list(&clocks.passes, &PASS_STAGES, render_frames));
    }
}

/// A changed asset is a queued write, which `finish encoders and submit` pays for.
/// What rewrote a mesh, for the profile log: each rewrite is uploaded again.
#[derive(Clone, Copy)]
pub(crate) enum MeshWrite {
    Strip,
    Beam,
    SpriteGrid,
    Fluid,
    Corona,
    ActorLight,
    TerrainLight,
}

const MESH_WRITERS: [&str; 7] = ["strips", "beams", "sprite grids", "fluids", "coronas", "actor vertex light", "terrain light"];
static MESH_WRITES: [std::sync::atomic::AtomicU32; 7] = [const { std::sync::atomic::AtomicU32::new(0) }; 7];

pub(crate) fn mesh_written(by: MeshWrite) {
    MESH_WRITES[by as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

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
    let by: Vec<String> = MESH_WRITERS
        .iter()
        .zip(&MESH_WRITES)
        .map(|(name, count)| format!("{name} {}", (count.swap(0, std::sync::atomic::Ordering::Relaxed) as f32 * per_second) as u32))
        .collect();
    bevy::log::info!("mesh writes a second by writer: {}", by.join(", "));
}

fn log_scene_counts(
    clocks: Res<StageClocks>,
    settings: Option<Res<Settings>>,
    time: Res<Time<Real>>,
    (meshes, culled): (Query<(&ViewVisibility, Has<FollowActor>, Has<FollowBone>), With<Mesh3d>>, Query<&InheritedVisibility, Without<NoCpuCulling>>),
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
        "scene: {} mesh entities, {} of them shown, movers {movers_all} with {movers_shown} shown; culling walks {} entities a camera, {} not hidden; {views} views, {shadow_views_drawing} shadow views with something to draw; {} cameras active; window {} x {}; draw groups: opaque {opaque}, masked {masked}, prepass {prepass}, masked prepass {masked_prepass}, shadow {shadow}, blended {blended}",
        meshes.iter().len(),
        meshes.iter().filter(|(visible, ..)| shown(visible)).count(),
        culled.iter().len(),
        culled.iter().filter(|inherited| inherited.get()).count(),
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
        use bevy::camera::CameraUpdateSystems;
        use bevy::camera::visibility::VisibilitySystems as Visibility;
        use bevy::light::SimulationLightSystems as Light;
        use bevy::transform::TransformSystems;
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
                main_mark::<-1>.before(TransformSystems::Propagate),
                main_mark::<0>.after(TransformSystems::Propagate).before(Visibility::CalculateBounds).before(Visibility::UpdateFrusta).before(Visibility::VisibilityPropagate),
                main_mark::<1>.after(Visibility::CalculateBounds).after(Visibility::UpdateFrusta).after(Visibility::VisibilityPropagate).before(Visibility::CheckVisibility),
                main_mark::<2>.after(Visibility::CheckVisibility).after(CameraUpdateSystems).before(Light::AssignLightsToClusters),
                main_mark::<3>.after(Light::AssignLightsToClusters).before(Light::UpdateDirectionalLightCascades),
                main_mark::<4>.after(Light::UpdateLightFrusta).before(Light::CheckLightVisibility),
                main_mark::<5>.after(Light::CheckLightVisibility),
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
