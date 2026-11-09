use super::*;
use p2_vm::ERenderStyle;

pub(super) struct HudFont {
    font: Font,
    pages: Vec<Option<(egui::TextureId, egui::Vec2)>>,
}

type HudImage = (Handle<Image>, egui::Vec2);

#[derive(Default)]
pub(super) struct HudAssets {
    fonts: HashMap<String, (Font, Vec<Option<HudImage>>)>,
    textures: HashMap<String, HudImage>,
}

impl HudAssets {
    pub(super) fn read(game: &mut Game, vm: &Vm<Arc<Loader>>, mut add: impl FnMut(Image) -> Handle<Image>) -> Self {
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
            .collect();
        let fonts: HashSet<&str> = vm.object_ids().filter(|&id| is_font(id)).map(|id| vm.store.object(id).path.as_str()).collect();
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

fn read_hud_font(game: &mut Game, path: &str, mut add: impl FnMut(Image) -> Handle<Image>) -> Option<(Font, Vec<Option<HudImage>>)> {
    let (package, export) = game.export_by_path(path)?;
    let (pkg, data) = game.data(package);
    let font = read_font(pkg, data, &pkg.exports[export]).ok()?;
    let pages = font.textures.iter().map(|&reference| read_ui_image(game, package, reference, &mut add)).collect();
    Some((font, pages))
}

fn read_hud_texture(game: &mut Game, material: &str, add: impl FnMut(Image) -> Handle<Image>) -> Option<HudImage> {
    let (package, export) = game.export_by_path(material)?;
    read_ui_image(game, package, export_reference(export), add)
}

fn read_ui_image(game: &mut Game, package: PackageId, reference: i32, mut add: impl FnMut(Image) -> Handle<Image>) -> Option<HudImage> {
    let texture = game.ui_texture(package, reference)?;
    let size = texture.image.size().as_vec2();
    Some((add(texture.image), egui::vec2(size.x, size.y)))
}

pub(super) fn paint_overlay(
    ctx: &egui::Context,
    rect: egui::Rect,
    Overlay { flash_scale: scale, flash_fog: fog, titles }: Overlay,
    session: &mut Session,
    contexts: &mut EguiContexts,
    targets: &mut SceneTargets,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, "matinee".into())).with_clip_rect(rect);
    let cover = (1.0 - scale).clamp(0.0, 1.0);
    if cover > 0.0 {
        let [r, g, b] = fog.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(r, g, b, (cover * 255.0) as u8));
    }
    for OverlayTitle { material, color: [r, g, b, a], size, at: [x, y], alpha } in titles {
        let Some((id, texels)) = session.cached_hud_texture(&material, contexts, &mut targets.assets.images) else { continue };
        let width = size * rect.width();
        let height = width * texels.y / texels.x.max(1.0);
        let centre = rect.min + egui::vec2(x * rect.width(), y * rect.height());
        let area = egui::Rect::from_center_size(centre, egui::vec2(width, height));
        let tint = egui::Color32::from_rgba_unmultiplied(r, g, b, (f32::from(a) * alpha.clamp(0.0, 1.0)) as u8);
        painter.image(id, area, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    }
}

/// A call shares the previous call's slot when nothing was painted between them, it does not clear the depth and its
/// field of view is the same: retail draws it into that call's depth, with nothing over the first (`MapScreen`'s trail:
/// up to 10 calls).
pub(super) fn call_slots(items: &[CanvasItem]) -> Vec<usize> {
    let mut slots: Vec<usize> = Vec::new();
    let mut previous = None;
    for item in items {
        if let CanvasItem::Actor { clear_z, fov, .. } = *item {
            let slot = match slots.last() {
                Some(&last) if previous == Some(fov) && !clear_z => last,
                Some(&last) => last + 1,
                None => 0,
            };
            slots.push(slot);
            previous = Some(fov);
        } else {
            previous = None;
        }
    }
    slots
}

/// As retail: the 2D items before the n-th `DrawActor` call go to its slot's canvas layer ([`call_slots`]), over the
/// scene and the calls before it, under that call; the items after the last call go to `ctx`, over everything.
pub(super) fn paint_canvas(ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], session: &mut Session, contexts: &mut EguiContexts, targets: &mut SceneTargets) {
    let is_call = |item: &CanvasItem| matches!(item, CanvasItem::Actor { .. });
    let slots = call_slots(items);
    for (n, segment) in items.split(is_call).enumerate().filter(|(_, segment)| !segment.is_empty()) {
        let slot = slots.get(n).copied();
        let layer = slot.and_then(|slot| targets.view.canvas_layers.get(slot).copied());
        let layer = layer.and_then(|layer| contexts.ctx_for_entity_mut(layer).ok()).cloned();
        match (layer, slot) {
            (Some(layer), Some(slot)) => {
                if let Some(shown) = targets.view.canvas_below.get_mut(slot) {
                    *shown = true;
                }
                paint_hud(&layer, rect, segment, session, contexts, targets);
            }
            _ => paint_hud(ctx, rect, segment, session, contexts, targets),
        }
    }
}

pub(super) fn paint_hud(ctx: &egui::Context, rect: egui::Rect, items: &[CanvasItem], session: &mut Session, contexts: &mut EguiContexts, targets: &mut SceneTargets) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, "hud".into())).with_clip_rect(rect);
    let color = |c: [u8; 4]| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    for item in items {
        match item {
            CanvasItem::Text { text, position, scale, color: c, font } => {
                let path = font.as_deref().unwrap_or_default();
                let hud_font = match session.hud_fonts.get(path) {
                    Some(found) => found.as_ref(),
                    None => {
                        let found = font.as_deref().and_then(|path| session.hud_font(path, contexts, &mut targets.assets.images));
                        session.hud_fonts.entry(path.to_owned()).or_insert(found).as_ref()
                    }
                };
                let origin = rect.min + egui::vec2(position[0], position[1]);
                match hud_font {
                    Some(hud_font) => {
                        let mut x = 0.0;
                        for glyph in text.chars().filter_map(|ch| hud_font.font.glyph(ch)) {
                            let size = egui::vec2(glyph.u_size as f32 * scale[0], glyph.v_size as f32 * scale[1]);
                            if let Some(Some((id, texels))) = hud_font.pages.get(usize::from(glyph.texture)) {
                                let uv = egui::Rect::from_min_size(
                                    egui::pos2(glyph.start_u as f32 / texels.x, glyph.start_v as f32 / texels.y),
                                    egui::vec2(glyph.u_size as f32 / texels.x, glyph.v_size as f32 / texels.y),
                                );
                                painter.image(*id, egui::Rect::from_min_size(origin + egui::vec2(x, 0.0), size), uv, color(*c));
                            }
                            x += (glyph.u_size + hud_font.font.kerning) as f32 * scale[0];
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
                let Some((id, texels)) = session.cached_hud_texture(material, contexts, &mut targets.assets.images) else { continue };
                let area = egui::Rect::from_min_size(rect.min + egui::vec2(position[0], position[1]), egui::vec2(size[0], size[1]));
                let uv = egui::Rect::from_min_size(
                    egui::pos2(uv[0] / texels.x, uv[1] / texels.y),
                    egui::vec2(uv[2] / texels.x, uv[3] / texels.y),
                );
                let tint = if ERenderStyle::from_byte(*style) == Some(ERenderStyle::Translucent) { egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], 0) } else { color(*c) };
                painter.image(id, area, uv, tint);
            }
        }
    }
}

impl Session {
    pub(super) fn hud_font(&mut self, path: &str, contexts: &mut EguiContexts, images: &mut Assets<Image>) -> Option<HudFont> {
        let (font, pages) = match self.hud_assets.fonts.remove(path) {
            Some(read) => read,
            None => read_hud_font(open_game(&mut self.library, self.map_system.as_deref())?, path, |image| images.add(image))?,
        };
        let pages = pages.into_iter().map(|page| page.map(|(handle, size)| (self.registered_image(handle, contexts), size))).collect();
        Some(HudFont { font, pages })
    }

    pub(super) fn hud_texture(&mut self, material: &str, contexts: &mut EguiContexts, images: &mut Assets<Image>) -> Option<(egui::TextureId, egui::Vec2)> {
        let (handle, size) = match self.hud_assets.textures.remove(material) {
            Some(read) => read,
            None => read_hud_texture(open_game(&mut self.library, self.map_system.as_deref())?, material, |image| images.add(image))?,
        };
        Some((self.registered_image(handle, contexts), size))
    }

    pub(super) fn cached_hud_texture(&mut self, material: &str, contexts: &mut EguiContexts, images: &mut Assets<Image>) -> Option<(egui::TextureId, egui::Vec2)> {
        if let Some(found) = self.hud_textures.get(material) {
            return *found;
        }
        let found = self.hud_texture(material, contexts, images);
        self.hud_textures.insert(material.to_owned(), found);
        found
    }

    /// The session holds it: `bevy_egui` forgets the image when the asset goes ([`Session::clear`] drops the handles).
    fn registered_image(&mut self, handle: Handle<Image>, contexts: &mut EguiContexts) -> egui::TextureId {
        let id = contexts.add_image(EguiTextureHandle::Weak(handle.id()));
        self.hud_images.push(handle);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(clear_z: bool, fov: f32) -> CanvasItem {
        CanvasItem::Actor { actor: ObjectId::from_index(1), clear_z, fov, location: [0.0; 3].into(), rotation: [0; 3].into(), scale: [1.0; 3].into() }
    }

    fn text() -> CanvasItem {
        CanvasItem::Text { text: String::new(), position: [0.0; 2], scale: [1.0; 2], color: [0; 4], font: None }
    }

    #[test]
    fn back_to_back_calls_share_a_slot() {
        let items = [call(false, 0.0), call(false, 0.0), call(false, 0.0), text(), call(false, 0.0), call(true, 0.0), call(false, 60.0)];
        assert_eq!(call_slots(&items), [0, 0, 0, 1, 2, 3]);
    }
}
