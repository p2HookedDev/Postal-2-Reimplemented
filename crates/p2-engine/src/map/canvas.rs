//! The map's canvas, painted each frame in the egui pass: the HUD and screens the scripts draw, the matinee overlay,
//! and while a map loads the last image with the loading bar over it.

use super::*;

pub(super) fn spawn_hud_camera(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
    egui_settings.auto_create_primary_context = false;
    commands.spawn((PrimaryEguiContext, p2_render::view::hud_camera()));
}

/// Where the host shows the map, in egui points: its canvas is painted there. `None` paints nothing.
#[derive(Resource, Default)]
pub struct CanvasArea(pub Option<egui::Rect>);

#[derive(SystemParam)]
pub(super) struct CanvasTargets<'w, 's> {
    assets: AssetStores<'w>,
    view: ResMut<'w, MeshView>,
    times: ResMut<'w, FrameTimes>,
    play: ResMut<'w, Play>,
    commands: Commands<'w, 's>,
    hud: ResMut<'w, Hud>,
    painter: ResMut<'w, p2_render::canvas::CanvasPainter>,
}

pub(super) fn paint_map_canvas(area: Res<CanvasArea>, mut session: ResMut<Session>, mut contexts: EguiContexts, mut t: CanvasTargets) -> bevy::ecs::error::Result {
    let Some(rect) = area.0 else { return Ok(()) };
    let ctx = contexts.ctx_mut()?.clone();
    match session.loading() {
        Some(progress) => loading_frame(&mut session, &ctx, rect, progress, &mut contexts, &mut t),
        None => frame(&mut session, &ctx, rect, &mut contexts, &mut t),
    }
    Ok(())
}

fn frame(session: &mut Session, ctx: &egui::Context, rect: egui::Rect, contexts: &mut EguiContexts, t: &mut CanvasTargets) {
    let started = std::time::Instant::now();
    t.hud.start_frame(started);
    t.play.set_canvas_size([rect.width(), rect.height()]);
    t.play.set_hud_inset(if t.view.modern.safe_area { p2_render::modern_view::hud_area(rect).min.x - rect.min.x } else { 0.0 });
    t.view.hide_canvas_layers();
    // Until a new map's first VM frame is presented, the last image (the load screen) stays up, as in the
    // engine's frame buffer; its canvas actors belong to the level left.
    if t.play.world().is_some_and(|world| !world.presented()) {
        let items = Arc::clone(&t.hud.last_hud);
        paint_items(session, ctx, rect, &items, contexts, t);
        t.painter.paint_progress(ctx, rect, 1.0);
        t.times.add("hud paint", started);
        return;
    }
    let items = t.play.world().map(PlayWorld::hud_items).unwrap_or_default();
    t.hud.movies.update(&session.root, contexts, &mut t.play, &mut t.commands, &mut t.assets, &mut t.painter);
    let mut stores = CanvasStores { contexts, images: &mut t.assets.images };
    let mut source = LibraryReads { session, hud: &mut t.hud };
    if let Some(overlay) = t.play.world().and_then(PlayWorld::screen_overlay) {
        t.painter.paint_overlay(ctx, rect, overlay, &mut source, &mut stores);
    }
    if !items.is_empty() {
        t.painter.paint_canvas(ctx, rect, &items, &mut t.view, &mut source, &mut stores);
    }
    t.times.add("hud paint", started);
    t.hud.last_hud = items;
}

fn paint_items(session: &mut Session, ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], contexts: &mut EguiContexts, t: &mut CanvasTargets) {
    let mut stores = CanvasStores { contexts, images: &mut t.assets.images };
    t.painter.paint(ctx, rect, items, &t.view.modern, &mut LibraryReads { session, hud: &mut t.hud }, &mut stores);
}

fn loading_frame(session: &mut Session, ctx: &egui::Context, rect: egui::Rect, progress: f32, contexts: &mut EguiContexts, t: &mut CanvasTargets) {
    t.play.set_canvas_size([rect.width(), rect.height()]);
    t.view.hide_canvas_layers();
    let items = Arc::clone(&t.hud.last_hud);
    paint_items(session, ctx, rect, &items, contexts, t);
    if !std::mem::replace(&mut t.hud.progress_bar_read, true) {
        let entry = map_package("Entry");
        if let Some((bar, image)) = session.library.open(&session.root, &entry).ok().and_then(|(game, id)| read_progress_bar(game, id)) {
            let image = t.assets.images.add(image);
            t.painter.set_progress_bar(bar, image, contexts);
        }
    }
    t.painter.paint_progress(ctx, rect, progress);
}
