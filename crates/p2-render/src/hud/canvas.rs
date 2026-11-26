use bevy::asset::{Assets, Handle};
use bevy::ecs::prelude::Resource;
use bevy::image::Image;
use bevy_egui::{EguiContexts, EguiTextureHandle, egui};
use p2_assets::Font;
use p2_vm::{CanvasItem, ERenderStyle, call_slots};
use rustc_hash::FxHashMap;
use crate::world::modern_view::ModernView;
use crate::world::view::MainView;

mod distance;
mod progress;
mod sharp;

pub use progress::ProgressBar;

pub struct CanvasImage {
    pub image: Handle<Image>,
    pub texels: egui::Vec2,
}

pub struct CanvasFont {
    pub font: Font,
    pub pages: Vec<Option<CanvasImage>>,
}

pub trait CanvasSource {
    fn texture(&mut self, material: &str, images: &mut Assets<Image>) -> Option<Option<CanvasImage>>;
    fn font(&mut self, path: &str, images: &mut Assets<Image>) -> Option<Option<CanvasFont>>;
}

pub struct CanvasStores<'a, 'w, 's> {
    pub contexts: &'a mut EguiContexts<'w, 's>,
    pub images: &'a mut Assets<Image>,
}

#[derive(Clone, PartialEq)]
pub struct Overlay {
    /// How much of the scene shows (1 = all, 0 = only the fog colour).
    pub flash_scale: f32,
    pub flash_fog: [f32; 3],
    pub titles: Vec<OverlayTitle>,
}

#[derive(Clone, PartialEq)]
pub struct OverlayTitle {
    pub material: String,
    pub color: [u8; 4],
    /// Width relative to the screen width.
    pub size: f32,
    /// Centre as fractions of the screen width and height.
    pub at: [f32; 2],
    pub alpha: f32,
}

struct PaintedFont {
    font: Font,
    pages: Vec<Option<(egui::TextureId, egui::Vec2)>>,
}

pub fn in_package(path: &str, package: &str) -> bool {
    path.split_once('.').is_some_and(|(first, _)| first.eq_ignore_ascii_case(package))
}

#[derive(Resource, Default)]
pub struct CanvasPainter {
    textures: FxHashMap<String, Option<(egui::TextureId, egui::Vec2)>>,
    fonts: FxHashMap<String, Option<PaintedFont>>,
    /// Keeps those images alive: egui holds them weakly and forgets each once it goes.
    images: Vec<Handle<Image>>,
    nearest: sharp::NearestImages,
    progress: Option<(ProgressBar, egui::TextureId)>,
}

fn registered(kept: &mut Vec<Handle<Image>>, image: Handle<Image>, contexts: &mut EguiContexts) -> egui::TextureId {
    let id = contexts.add_image(EguiTextureHandle::Weak(image.id()));
    kept.push(image);
    id
}

impl CanvasPainter {
    pub fn forget_map(&mut self, map: &str) {
        self.textures.retain(|path, painted| painted.is_some() && !in_package(path, map));
        self.fonts.retain(|path, painted| painted.is_some() && !in_package(path, map));
    }

    pub fn painted(&self) -> std::collections::HashSet<String> {
        self.textures.keys().chain(self.fonts.keys()).cloned().collect()
    }

    pub fn set_progress_bar(&mut self, bar: ProgressBar, image: Handle<Image>, contexts: &mut EguiContexts) {
        self.progress = Some((bar, contexts.add_image(EguiTextureHandle::Strong(image))));
    }

    pub fn paint_progress(&self, ctx: &egui::Context, rect: egui::Rect, progress: f32) {
        if let Some((bar, texture)) = &self.progress {
            let shapes = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, "progress".into())).with_clip_rect(rect);
            shapes.add(bar.mesh(rect, progress.min(0.99), *texture));
        }
    }

    pub fn paint_overlay(&mut self, ctx: &egui::Context, rect: egui::Rect, overlay: Overlay, source: &mut impl CanvasSource, stores: &mut CanvasStores) {
        let Overlay { flash_scale: scale, flash_fog: fog, titles } = overlay;
        let shapes = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, "matinee".into())).with_clip_rect(rect);
        let cover = (1.0 - scale).clamp(0.0, 1.0);
        if cover > 0.0 {
            let [r, g, b] = fog.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
            shapes.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(r, g, b, (cover * 255.0) as u8));
        }
        for OverlayTitle { material, color: [r, g, b, a], size, at: [x, y], alpha } in titles {
            let Some((id, texels)) = self.texture(&material, source, stores) else { continue };
            let width = size * rect.width();
            let height = width * texels.y / texels.x.max(1.0);
            let centre = rect.min + egui::vec2(x * rect.width(), y * rect.height());
            let area = egui::Rect::from_center_size(centre, egui::vec2(width, height));
            let tint = egui::Color32::from_rgba_unmultiplied(r, g, b, (f32::from(a) * alpha.clamp(0.0, 1.0)) as u8);
            shapes.image(id, area, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
    }

    pub fn set_image(&mut self, material: String, image: &Handle<Image>, texels: [f32; 2], contexts: &mut EguiContexts) {
        let id = contexts.add_image(EguiTextureHandle::Weak(image.id()));
        self.textures.insert(material, Some((id, egui::vec2(texels[0], texels[1]))));
    }

    pub fn texture(&mut self, material: &str, source: &mut impl CanvasSource, stores: &mut CanvasStores) -> Option<(egui::TextureId, egui::Vec2)> {
        if let Some(found) = self.textures.get(material) {
            return *found;
        }
        let found = source.texture(material, stores.images)?.map(|CanvasImage { image, texels }| (registered(&mut self.images, image, stores.contexts), texels));
        self.textures.insert(material.to_owned(), found);
        found
    }

    /// As retail: items before the n-th `DrawActor` call go to that slot's layer; items after the last go over everything.
    pub fn paint_canvas(&mut self, ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], view: &mut MainView, source: &mut impl CanvasSource, stores: &mut CanvasStores) {
        let is_call = |item: &CanvasItem| matches!(item, CanvasItem::Actor { .. });
        let slots = call_slots(items);
        for (n, segment) in items.split(is_call).enumerate().filter(|(_, segment)| !segment.is_empty()) {
            let slot = slots.get(n).copied();
            let layer = slot.and_then(|slot| view.canvas_layers.get(slot).copied());
            let layer = layer.and_then(|layer| stores.contexts.ctx_for_entity_mut(layer).ok()).cloned();
            match (layer, slot) {
                (Some(layer), Some(slot)) => {
                    if let Some(shown) = view.canvas_below.get_mut(slot) {
                        *shown = true;
                    }
                    self.paint(&layer, rect, segment, &view.modern, source, stores);
                }
                _ => self.paint(ctx, rect, segment, &view.modern, source, stores),
            }
        }
    }

    pub fn paint(&mut self, ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], modern: &ModernView, source: &mut impl CanvasSource, stores: &mut CanvasStores) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, "hud".into())).with_clip_rect(rect);
        let color = |c: [u8; 4]| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        let (sharp_hud, sharp_text, per_point) = (modern.sharp_hud, modern.sharp_text, ctx.pixels_per_point());
        for item in items {
            match item {
                CanvasItem::Text { text, position, scale, color: c, font } => {
                    let path = font.as_deref().unwrap_or_default();
                    if !self.fonts.contains_key(path) {
                        let read = match font.as_deref() {
                            Some(path) => source.font(path, stores.images),
                            None => Some(None),
                        };
                        let Some(read) = read else { continue };
                        let kept = &mut self.images;
                        let found = read.map(|CanvasFont { font, pages }| PaintedFont {
                            font,
                            pages: pages.into_iter().map(|page| page.map(|CanvasImage { image, texels }| (registered(kept, image, stores.contexts), texels))).collect(),
                        });
                        self.fonts.insert(path.to_owned(), found);
                    }
                    let Self { fonts, images: kept, nearest, .. } = self;
                    let mut origin = rect.min + egui::vec2(position[0], position[1]);
                    let snapped = sharp_hud && !sharp_text && scale.iter().all(|&scale| sharp::whole_per_texel(scale * per_point, 1.0));
                    if snapped {
                        origin = sharp::on_pixel(origin, per_point);
                    }
                    let magnified = scale[0].max(scale[1]) * per_point;
                    let smooth = sharp_text && magnified > 1.0;
                    match fonts.get(path).and_then(Option::as_ref) {
                        Some(painted) => {
                            let mut x = 0.0;
                            for glyph in text.chars().filter_map(|ch| painted.font.glyph(ch)) {
                                let size = egui::vec2(glyph.u_size as f32 * scale[0], glyph.v_size as f32 * scale[1]);
                                if let Some(Some((id, texels))) = painted.pages.get(usize::from(glyph.texture)) {
                                    let uv = egui::Rect::from_min_size(
                                        egui::pos2(glyph.start_u as f32 / texels.x, glyph.start_v as f32 / texels.y),
                                        egui::vec2(glyph.u_size as f32 / texels.x, glyph.v_size as f32 / texels.y),
                                    );
                                    let id = if smooth {
                                        nearest.text_page(*id, magnified, kept, stores.contexts, stores.images)
                                    } else if snapped {
                                        nearest.texture(*id, kept, stores.contexts, stores.images)
                                    } else {
                                        *id
                                    };
                                    painter.image(id, egui::Rect::from_min_size(origin + egui::vec2(x, 0.0), size), uv, color(*c));
                                }
                                x += (glyph.u_size + painted.font.kerning) as f32 * scale[0];
                            }
                        }
                        None => {
                            let font = egui::FontId::proportional(13.0 * scale[1]);
                            painter.text(origin, egui::Align2::LEFT_TOP, text, font, color(*c));
                        }
                    }
                }
                CanvasItem::Actor { .. } => {}
                CanvasItem::Tile { material, position, size, uv, color: c, style } => {
                    let Some((mut id, texels)) = self.texture(material, source, stores) else { continue };
                    let mut area = egui::Rect::from_min_size(rect.min + egui::vec2(position[0], position[1]), egui::vec2(size[0], size[1]));
                    let mut size = *size;
                    let mut whole = false;
                    if sharp_hud {
                        area = egui::Rect::from_min_max(sharp::on_pixel(area.min, per_point), sharp::on_pixel(area.max, per_point));
                        size = [area.width(), area.height()];
                        if sharp::whole_per_texel(size[0] * per_point, uv[2].abs()) && sharp::whole_per_texel(size[1] * per_point, uv[3].abs()) {
                            let copy = self.nearest.texture(id, &self.images, stores.contexts, stores.images);
                            whole = copy != id;
                            id = copy;
                        }
                    }
                    let (u, v) = if whole { ([uv[0], uv[2]], [uv[1], uv[3]]) } else { (inside_borders(uv[0], uv[2], texels.x, size[0]), inside_borders(uv[1], uv[3], texels.y, size[1])) };
                    let uv = egui::Rect::from_min_size(egui::pos2(u[0] / texels.x, v[0] / texels.y), egui::vec2(u[1] / texels.x, v[1] / texels.y));
                    let tint = if ERenderStyle::from_byte(*style) == Some(ERenderStyle::Translucent) { egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], 0) } else { color(*c) };
                    painter.image(id, area, uv, tint);
                }
            }
        }
    }
}

fn inside_borders(start: f32, length: f32, texels: f32, drawn: f32) -> [f32; 2] {
    if length <= 0.0 || drawn <= length || drawn <= 1.0 {
        return [start, length];
    }
    let inset = 0.5 * (drawn - length) / (drawn - 1.0);
    let (end, at_border) = (start + length, |edge: f32| edge == 0.0 || edge == texels);
    let (from, to) = (if at_border(start) { start + inset } else { start }, if at_border(end) { end - inset } else { end });
    [from, to - from]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_change_keeps_the_shared_paintings_only() {
        let mut painter = CanvasPainter::default();
        let painted = Some((egui::TextureId::User(1), egui::Vec2::ONE));
        for (path, found) in [("P2Misc.Hud.Ammo", painted), ("Suburbs-3.Signs.Door", painted), ("Missing.Tex", None)] {
            painter.textures.insert(path.to_owned(), found);
        }
        painter.forget_map("suburbs-3");
        assert_eq!(painter.painted(), ["P2Misc.Hud.Ammo".to_owned()].into_iter().collect());
    }

    #[test]
    fn an_enlarged_tile_reads_no_texel_across_the_border() {
        assert_eq!(inside_borders(0.0, 32.0, 32.0, 32.0), [0.0, 32.0]);
        assert_eq!(inside_borders(8.0, 16.0, 32.0, 64.0), [8.0, 16.0]);
        let [start, length] = inside_borders(0.0, 32.0, 32.0, 64.0);
        let first_pixel = start + 0.5 * length / 64.0;
        assert!((first_pixel - 0.5).abs() < 1e-4 && (start + length - (32.0 - start)).abs() < 1e-4);
    }

    struct Reads {
        asked: usize,
        ready: bool,
    }

    impl CanvasSource for Reads {
        fn texture(&mut self, _: &str, images: &mut Assets<Image>) -> Option<Option<CanvasImage>> {
            self.asked += 1;
            self.ready.then(|| Some(CanvasImage { image: images.add(Image::default()), texels: egui::vec2(32.0, 32.0) }))
        }

        fn font(&mut self, _: &str, _: &mut Assets<Image>) -> Option<Option<CanvasFont>> {
            Some(None)
        }
    }

    #[test]
    fn a_tile_is_painted_from_the_frame_its_texture_is_read_and_the_read_is_asked_once() {
        let mut world = bevy::ecs::world::World::new();
        world.init_resource::<bevy_egui::EguiUserTextures>();
        let mut state = bevy::ecs::system::SystemState::<EguiContexts>::new(&mut world);
        let mut contexts = state.get_mut(&mut world).expect("the egui stores");
        let mut images = Assets::<Image>::default();
        let mut painter = CanvasPainter::default();
        let tile = [CanvasItem::Tile { material: "t".into(), position: [0.0; 2], size: [32.0; 2], uv: [0.0, 0.0, 32.0, 32.0], color: [255; 4], style: 1 }];
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 480.0));
        let ctx = egui::Context::default();
        let mut painted = |source: &mut Reads| {
            ctx.begin_pass(egui::RawInput::default());
            let mut stores = CanvasStores { contexts: &mut contexts, images: &mut images };
            painter.paint(&ctx, rect, &tile, &ModernView::default(), source, &mut stores);
            ctx.end_pass().shapes.len()
        };
        let mut source = Reads { asked: 0, ready: false };
        assert_eq!(painted(&mut source), 0);
        source.ready = true;
        assert_eq!(painted(&mut source), 1);
        assert_eq!(painted(&mut source), 1);
        assert_eq!(source.asked, 2);
    }

    #[test]
    fn a_flash_covers_the_scene_and_a_title_waits_for_its_texture() {
        let mut world = bevy::ecs::world::World::new();
        world.init_resource::<bevy_egui::EguiUserTextures>();
        let mut state = bevy::ecs::system::SystemState::<EguiContexts>::new(&mut world);
        let mut contexts = state.get_mut(&mut world).expect("the egui stores");
        let mut images = Assets::<Image>::default();
        let mut painter = CanvasPainter::default();
        let title = OverlayTitle { material: "t".to_owned(), color: [255; 4], size: 0.5, at: [0.5; 2], alpha: 1.0 };
        let overlay = Overlay { flash_scale: 0.25, flash_fog: [1.0; 3], titles: vec![title] };
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 480.0));
        let ctx = egui::Context::default();
        let mut painted = |source: &mut Reads| {
            ctx.begin_pass(egui::RawInput::default());
            let mut stores = CanvasStores { contexts: &mut contexts, images: &mut images };
            painter.paint_overlay(&ctx, rect, overlay.clone(), source, &mut stores);
            ctx.end_pass().shapes.len()
        };
        let mut source = Reads { asked: 0, ready: false };
        assert_eq!(painted(&mut source), 1);
        source.ready = true;
        assert_eq!(painted(&mut source), 2);
    }
}
