//! HUD natives record what they draw for the host to paint; the text measuring rule is (unverified).

use std::sync::Arc;

use p2_assets::{Font, read_font};

use p2_math::{Rotator, Vector};
use crate::native_id::Native;
use crate::Prop;
use crate::interp::{float, truthy};
use crate::{Error, LoaderRef, ObjectId, Value, Vm};

const CHAR_WIDTH: f32 = 8.0;
const CHAR_HEIGHT: f32 = 16.0;
const STY_NONE: u8 = 0;

#[derive(Debug, Clone, PartialEq)]
pub enum CanvasItem {
    Tile { material: Arc<str>, position: [f32; 2], size: [f32; 2], uv: [f32; 4], color: [u8; 4], style: u8 },
    Text { text: String, position: [f32; 2], scale: [f32; 2], color: [u8; 4], font: Option<Arc<str>> },
    /// `DrawActor` (first-person weapons): `fov` 0 is the view's. Placed as it was at the call (`scale`: `DrawScale` ×
    /// `DrawScale3D`): `MapScreen` draws one marker actor at every point of its trail, moving it between the calls.
    Actor { actor: ObjectId, clear_z: bool, fov: f32, location: Vector, rotation: Rotator, scale: Vector },
}

impl<L: LoaderRef> Vm<L> {
    pub fn render_hud(&mut self, controller: ObjectId, width: f32, height: f32) -> Result<Vec<CanvasItem>, Error> {
        let hud = self.object_of(controller, Prop::myHUD);
        let canvas = match self.media.canvas {
            Some(canvas) => canvas,
            None => {
                let class = self.find_class("Engine", "Canvas")?;
                let canvas = self.new_object(class, None);
                self.media.canvas = Some(canvas);
                canvas
            }
        };
        self.event(canvas, "Reset", Vec::new())?;
        for (property, value) in [(Prop::SizeX, Value::Int(width as i32)), (Prop::SizeY, Value::Int(height as i32))] {
            self.set(canvas, property, value);
        }
        for (property, value) in [(Prop::ClipX, width), (Prop::ClipY, height)] {
            self.set(canvas, property, Value::Float(value));
        }
        if let Some(viewport) = self.viewport() {
            self.set(canvas, Prop::Viewport, Value::Object(Some(viewport)));
        }
        self.media.canvas_items.clear();
        self.render_interactions("Process_PreRender", canvas)?;
        if let Some(hud) = hud {
            self.event(hud, "PostRender", [Value::Object(Some(canvas))])?;
        }
        self.render_interactions("Process_PostRender", canvas)?;
        let next = Vec::with_capacity(self.media.canvas_items.len());
        Ok(std::mem::replace(&mut self.media.canvas_items, next))
    }

    fn font(&mut self, font: ObjectId) -> Option<Arc<Font>> {
        if let Some(found) = self.media.fonts.get(&font) {
            return found.clone();
        }
        let path = self.object(font).path.clone();
        let found = match self.export_of(font) {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                read_font(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok().map(Arc::new)
            }
            None => self.side_export(&path).and_then(|(side, export)| read_font(&side.package, &side.data, &side.package.exports[export]).ok().map(Arc::new)),
        };
        self.media.fonts.insert(font, found.clone());
        found
    }

    fn canvas_path(&mut self, object: ObjectId) -> Arc<str> {
        let path = self.object(object).path.as_str();
        if let Some(kept) = self.media.canvas_paths.get(&object)
            && &**kept == path
        {
            return kept.clone();
        }
        let kept: Arc<str> = Arc::from(path);
        self.media.canvas_paths.insert(object, kept.clone());
        kept
    }

    fn canvas_font(&mut self, canvas: ObjectId) -> CanvasFont {
        let scale = [self.float_of(canvas, Prop::FontScaleX), self.float_of(canvas, Prop::FontScaleY)].map(|s| if s == 0.0 { 1.0 } else { s });
        let font = self.object_of(canvas, Prop::Font).and_then(|font| self.font(font).map(|found| (self.canvas_path(font), found)));
        CanvasFont { font, scale, space_x: self.float_of(canvas, Prop::SpaceX) }
    }

    #[expect(clippy::unnecessary_wraps, reason = "every native dispatcher has this shape: `natives::call` returns its result")]
    pub(crate) fn canvas_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let Some(canvas) = this else { return Ok(None) };
        let float_of = |vm: &Self, property: Prop| vm.float_of(canvas, property);
        let string = |args: &[Value]| match args.first() {
            Some(Value::Str(text)) => text.clone(),
            _ => String::new(),
        };
        let color = |vm: &mut Self| vm.color_of(canvas, Prop::DrawColor).unwrap_or([255; 4]);
        let style = |vm: &Self| match vm.get(canvas, Prop::Style) {
            Some(Value::Byte(style)) => style,
            _ => 1,
        };
        match native {
            // Sizes of a string: `StrLen` (wrapped) and `TextSize` (clipped) alike for now.
            Native::StrLen | Native::TextSize => {
                let [width, height] = self.canvas_font(canvas).size(&string(args));
                for (slot, value) in args.iter_mut().skip(1).zip([width, height]) {
                    *slot = Value::Float(value);
                }
            }
            // At the whole-pixel position; `bCenter` is ignored and the position does not move.
            // Retail cuts a glyph at the clip edge; we draw it whole (dropping it hid text in tight menus).
            Native::DrawTextClipped => {
                let text = string(args);
                let font = self.canvas_font(canvas);
                let (org, clip) = ([float_of(self, Prop::OrgX), float_of(self, Prop::OrgY)], [float_of(self, Prop::ClipX), float_of(self, Prop::ClipY)]);
                let (x, y) = (float_of(self, Prop::CurX).round(), float_of(self, Prop::CurY).round());
                let (kept, start) = clip_text(x, &font.glyphs(&text).collect::<Vec<_>>(), clip[0]);
                let height = font.size(&text)[1];
                if let Some(start) = start.filter(|_| style(self) != STY_NONE && y < clip[1] && y + height > 0.0) {
                    let color = color(self);
                    self.media.canvas_items.push(CanvasItem::Text { text: kept, position: [org[0] + start, org[1] + y], scale: font.scale, color, font: font.path() });
                }
            }
            Native::DrawText => {
                let text = string(args);
                let font = self.canvas_font(canvas);
                let [width, height] = font.size(&text);
                let position = [float_of(self, Prop::OrgX) + float_of(self, Prop::CurX), float_of(self, Prop::OrgY) + float_of(self, Prop::CurY)];
                let x = if self.flag_of(canvas, Prop::bCenter) {
                    float_of(self, Prop::OrgX) + (float_of(self, Prop::ClipX) - width) / 2.0
                } else {
                    position[0]
                };
                if style(self) != STY_NONE {
                    let color = color(self);
                    self.media.canvas_items.push(CanvasItem::Text { text, position: [x, position[1]], scale: font.scale, color, font: font.path() });
                }
                if args.get(1).is_some_and(truthy) {
                    self.set(canvas, Prop::CurX, Value::Float(0.0));
                    self.set(canvas, Prop::CurY, Value::Float(float_of(self, Prop::CurY) + height));
                } else {
                    self.set(canvas, Prop::CurX, Value::Float(float_of(self, Prop::CurX) + width));
                }
                self.set(canvas, Prop::CurYL, Value::Float(height));
            }
            Native::DrawTileClipped => {
                let material = match args.first() {
                    Some(&Value::Object(Some(material))) => self.canvas_path(material),
                    _ => return Ok(None),
                };
                let f = |n: usize| args.get(n).map_or(0.0, float);
                let (org, clip) = ([float_of(self, Prop::OrgX), float_of(self, Prop::OrgY)], [float_of(self, Prop::ClipX), float_of(self, Prop::ClipY)]);
                let at = [float_of(self, Prop::CurX), float_of(self, Prop::CurY)];
                if let Some((at, size, uv)) = clip_tile(at, [f(1), f(2)], [f(3), f(4), f(5), f(6)], clip) {
                    let style = style(self);
                    if style != STY_NONE {
                        let position = [org[0] + at[0], org[1] + at[1]];
                        let color = color(self);
                        self.media.canvas_items.push(CanvasItem::Tile { material, position, size, uv, color, style });
                    }
                    self.set(canvas, Prop::CurX, Value::Float(at[0] + size[0] + float_of(self, Prop::SpaceX)));
                    self.set(canvas, Prop::CurYL, Value::Float(float_of(self, Prop::CurYL).max(size[1])));
                }
            }
            Native::DrawTile => {
                let material = match args.first() {
                    Some(&Value::Object(Some(material))) => self.canvas_path(material),
                    _ => return Ok(None),
                };
                let f = |n: usize| args.get(n).map_or(0.0, float);
                let size = [f(1), f(2)];
                let style = style(self);
                if style != STY_NONE {
                    let position = [float_of(self, Prop::OrgX) + float_of(self, Prop::CurX), float_of(self, Prop::OrgY) + float_of(self, Prop::CurY)];
                    let color = color(self);
                    self.media.canvas_items.push(CanvasItem::Tile { material, position, size, uv: [f(3), f(4), f(5), f(6)], color, style });
                }
                self.set(canvas, Prop::CurX, Value::Float(float_of(self, Prop::CurX) + size[0]));
                self.set(canvas, Prop::CurYL, Value::Float(size[1]));
            }
            Native::DrawActor => {
                if let Some(&Value::Object(Some(actor))) = args.first() {
                    let clear_z = args.get(2).is_some_and(truthy);
                    let fov = args.get(3).map_or(0.0, float);
                    let draw_scale = self.float_of(actor, Prop::DrawScale);
                    let scale = self.vector(actor, Prop::DrawScale3D).map(|s| s * draw_scale);
                    let (location, rotation) = (self.location(actor), self.rotation(actor));
                    self.media.canvas_items.push(CanvasItem::Actor { actor, clear_z, fov, location, rotation, scale });
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

struct CanvasFont {
    font: Option<(Arc<str>, Arc<Font>)>,
    scale: [f32; 2],
    space_x: f32,
}

impl CanvasFont {
    fn path(&self) -> Option<Arc<str>> {
        self.font.as_ref().map(|(path, _)| path.clone())
    }

    fn glyphs<'a>(&'a self, text: &'a str) -> impl Iterator<Item = (char, f32)> + 'a {
        glyph_advances(self.font.as_ref().map(|(_, font)| &**font), text, self.scale[0], self.space_x)
    }

    fn size(&self, text: &str) -> [f32; 2] {
        match &self.font {
            Some((_, font)) => {
                let height = text.chars().filter_map(|c| font.glyph(c)).fold(0.0_f32, |h, g| h.max(g.v_size as f32));
                [self.glyphs(text).map(|(_, w)| w).sum::<f32>(), (height * self.scale[1]).trunc()]
            }
            None => [text.chars().count() as f32 * CHAR_WIDTH * self.scale[0], CHAR_HEIGHT * self.scale[1]],
        }
    }
}

/// Widths truncate to whole pixels; an ESC colour code (ESC and three bytes) takes no room.
fn glyph_advances<'a>(font: Option<&'a Font>, text: &'a str, scale: f32, space_x: f32) -> impl Iterator<Item = (char, f32)> + 'a {
    let spacing = font.map_or(0.0, |font| space_x + font.kerning as f32);
    let mut chars = text.chars();
    let mut glyphs = std::iter::from_fn(move || {
        loop {
            let c = chars.next()?;
            let Some(font) = font else { return Some((c, (CHAR_WIDTH * scale).trunc())) };
            if c == '\u{1b}' {
                chars.nth(2);
            } else if let Some(g) = font.glyph(c) {
                return Some((c, (g.u_size as f32 * scale).trunc() + space_x + font.kerning as f32));
            }
        }
    })
    .peekable();
    std::iter::from_fn(move || {
        let (c, advance) = glyphs.next()?;
        Some((c, if glyphs.peek().is_some() { advance } else { advance - spacing }))
    })
}

fn clip_tile(mut at: [f32; 2], mut size: [f32; 2], mut uv: [f32; 4], clip: [f32; 2]) -> Option<([f32; 2], [f32; 2], [f32; 4])> {
    for axis in 0..2 {
        if size[axis] <= 0.0 {
            return None;
        }
        if at[axis] < 0.0 {
            let cut = (-at[axis]).min(size[axis]);
            let texels = uv[axis + 2] * cut / size[axis];
            uv[axis] += texels;
            uv[axis + 2] -= texels;
            size[axis] -= cut;
            at[axis] = 0.0;
        }
        if at[axis] + size[axis] > clip[axis] {
            let keep = (clip[axis] - at[axis]).max(0.0);
            uv[axis + 2] *= keep / size.get(axis).copied().filter(|&s| s > 0.0)?;
            size[axis] = keep;
        }
    }
    (size[0] > 0.0 && size[1] > 0.0).then_some((at, size, uv))
}

fn clip_text(mut x: f32, glyphs: &[(char, f32)], clip: f32) -> (String, Option<f32>) {
    let (mut kept, mut start) = (String::new(), None);
    for &(c, width) in glyphs {
        if x + width > 0.0 && x < clip {
            start.get_or_insert(x);
            kept.push(c);
        }
        x += width;
    }
    (kept, start)
}

#[cfg(test)]
mod clip_tests {
    use super::{clip_text, clip_tile};

    #[test]
    fn clipped_tiles_are_cut_to_the_clip_box() {
        assert_eq!(clip_tile([90.0, 0.0], [20.0, 10.0], [0.0, 0.0, 64.0, 32.0], [100.0, 5.0]), Some(([90.0, 0.0], [10.0, 5.0], [0.0, 0.0, 32.0, 16.0])));
        assert_eq!(clip_tile([-5.0, 0.0], [20.0, 10.0], [0.0, 0.0, 64.0, 32.0], [100.0, 100.0]), Some(([0.0, 0.0], [15.0, 10.0], [16.0, 0.0, 48.0, 32.0])));
        assert_eq!(clip_tile([120.0, 0.0], [20.0, 10.0], [0.0, 0.0, 64.0, 32.0], [100.0, 100.0]), None);
    }

    #[test]
    fn clipped_text_keeps_the_glyphs_overlapping_the_box() {
        let glyphs = [('a', 10.0), ('b', 10.0), ('c', 10.0)];
        assert_eq!(clip_text(-5.0, &glyphs, 25.0), ("abc".to_owned(), Some(-5.0)));
        assert_eq!(clip_text(-15.0, &glyphs, 4.0), ("b".to_owned(), Some(-5.0)));
        assert_eq!(clip_text(30.0, &glyphs, 25.0), (String::new(), None));
    }
}
