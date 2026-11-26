use crate::map::*;

pub(in crate::map) fn spawn_hud_camera(mut commands: Commands, mut egui_settings: ResMut<EguiGlobalSettings>) {
    egui_settings.auto_create_primary_context = false;
    commands.spawn((PrimaryEguiContext, p2_render::world::view::hud_camera()));
}

#[derive(Resource, Default)]
pub struct CanvasArea(pub Option<egui::Rect>);

#[derive(SystemParam)]
pub(in crate::map) struct CanvasTargets<'w, 's> {
    assets: AssetStores<'w>,
    view: ResMut<'w, MainView>,
    times: ResMut<'w, FrameTimes>,
    play: ResMut<'w, Play>,
    commands: Commands<'w, 's>,
    hud: ResMut<'w, Hud>,
    painter: ResMut<'w, p2_render::hud::canvas::CanvasPainter>,
    time: Res<'w, bevy::time::Time<bevy::time::Real>>,
}

pub(in crate::map) fn paint_map_canvas(area: Res<CanvasArea>, mut session: ResMut<Session>, mut contexts: EguiContexts, mut t: CanvasTargets) -> bevy::ecs::error::Result {
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
    t.play.set_hud_inset(if t.view.modern.safe_area { p2_render::world::modern_view::hud_area(rect).min.x - rect.min.x } else { 0.0 });
    t.view.hide_canvas_layers();
    if t.play.world().is_some_and(|world| !world.presented()) {
        let items = Arc::clone(&t.hud.last_hud);
        if t.hud.boot_cover.is_some() {
            paint_cover(ctx, rect, egui::Order::Background, 1.0);
        }
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
    let scene_started = t.play.world().is_some_and(PlayWorld::cutscene);
    if let Some(cover) = &mut t.hud.boot_cover {
        let opacity = cover.opacity(t.time.delta_secs(), scene_started);
        if opacity > 0.0 {
            paint_cover(ctx, rect, egui::Order::Tooltip, opacity);
        } else {
            t.hud.boot_cover = None;
        }
    }
    t.times.add("hud paint", started);
    t.hud.last_hud = items;
}

fn paint_cover(ctx: &egui::Context, rect: egui::Rect, order: egui::Order, opacity: f32) {
    let shapes = ctx.layer_painter(egui::LayerId::new(order, "cover".into())).with_clip_rect(rect);
    shapes.rect_filled(rect, 0.0, egui::Color32::from_black_alpha((opacity * 255.0) as u8));
}

fn paint_items(session: &mut Session, ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], contexts: &mut EguiContexts, t: &mut CanvasTargets) {
    let mut stores = CanvasStores { contexts, images: &mut t.assets.images };
    t.painter.paint(ctx, rect, items, &t.view.modern, &mut LibraryReads { session, hud: &mut t.hud }, &mut stores);
}

fn loading_frame(session: &mut Session, ctx: &egui::Context, rect: egui::Rect, progress: f32, contexts: &mut EguiContexts, t: &mut CanvasTargets) {
    t.play.set_canvas_size([rect.width(), rect.height()]);
    t.view.hide_canvas_layers();
    if session.map().is_none() {
        t.hud.boot_cover.get_or_insert_default();
        paint_cover(ctx, rect, egui::Order::Background, 1.0);
    }
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
