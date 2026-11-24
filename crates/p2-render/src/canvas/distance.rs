//! Modern graphics, sharp HUD level 2: a font page's glyph shapes as a signed distance field, made on the CPU the
//! first time the page is drawn magnified, and the page drawn again from it at the pixel size the HUD asks for,
//! so magnified text keeps smooth edges without a shader of its own.

use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::image::Image;
use bevy::math::Vec2;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_egui::{EguiContexts, EguiTextureHandle, egui};
use rustc_hash::FxHashMap;

use super::sharp::source_of;

const STEPS_PER_PIXEL: f32 = 4.0;
const LARGEST_PAGE: u32 = 4096;
const INSIDE_FROM: u8 = 128;
const FAR: f32 = 1e20;

#[derive(Default)]
pub(super) struct DistancePages {
    fields: FxHashMap<egui::TextureId, Vec<f32>>,
    sized: FxHashMap<(egui::TextureId, u32), (Option<egui::TextureId>, Option<Handle<Image>>)>,
}

impl DistancePages {
    pub(super) fn page(&mut self, id: egui::TextureId, factor: f32, sources: &[Handle<Image>], contexts: &mut EguiContexts, images: &mut Assets<Image>) -> Option<egui::TextureId> {
        let steps = (factor * STEPS_PER_PIXEL).round() as u32;
        if let Some((found, _)) = self.sized.get(&(id, steps)) {
            return *found;
        }
        let made = self.redrawn(id, steps as f32 / STEPS_PER_PIXEL, source_of(id, sources, contexts), images);
        let handle = made.map(|image| images.add(image));
        let found = handle.as_ref().map(|handle| contexts.add_image(EguiTextureHandle::Weak(handle.id())));
        self.sized.insert((id, steps), (found, handle));
        found
    }

    fn redrawn(&mut self, id: egui::TextureId, factor: f32, source: Option<&Handle<Image>>, images: &Assets<Image>) -> Option<Image> {
        let image = images.get(source?)?;
        let format = image.texture_descriptor.format;
        if !matches!(format, TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb | TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb) {
            return None;
        }
        let (width, height) = (image.width(), image.height());
        let size = Extent3d { width: (width as f32 * factor).ceil() as u32, height: (height as f32 * factor).ceil() as u32, depth_or_array_layers: 1 };
        let texels = width as usize * height as usize;
        let rgba = image.data.as_deref().filter(|data| texels > 0 && data.len() >= texels * 4)?;
        if size.width > LARGEST_PAGE || size.height > LARGEST_PAGE {
            return None;
        }
        let rgba = &rgba[..texels * 4];
        let field = self.fields.entry(id).or_insert_with(|| {
            let inside: Vec<bool> = rgba.as_chunks::<4>().0.iter().map(|texel| texel[3] >= INSIDE_FROM).collect();
            signed_distance(&inside, width as usize, height as usize)
        });
        let pixels = redraw(rgba, field, [width as usize, height as usize], factor, [size.width as usize, size.height as usize]);
        let mut page = Image::new(size, TextureDimension::D2, pixels, format, RenderAssetUsages::RENDER_WORLD);
        page.sampler = image.sampler.clone();
        Some(page)
    }
}

pub(super) fn signed_distance(inside: &[bool], width: usize, height: usize) -> Vec<f32> {
    let outside: Vec<bool> = inside.iter().map(|&inside| !inside).collect();
    let to_inside = squared_distances(inside, width, height);
    let to_outside = squared_distances(&outside, width, height);
    inside.iter().zip(to_inside.iter().zip(&to_outside)).map(|(&inside, (&to_inside, &to_outside))| if inside { 0.5 - to_outside.sqrt() } else { to_inside.sqrt() - 0.5 }).collect()
}

fn squared_distances(sites: &[bool], width: usize, height: usize) -> Vec<f32> {
    let mut grid: Vec<f32> = sites.iter().map(|&site| if site { 0.0 } else { FAR }).collect();
    let longest = width.max(height);
    let (mut line, mut found) = (vec![0.0; longest], vec![0.0; longest]);
    let (mut lowest, mut bounds) = (vec![0; longest], vec![0.0; longest + 1]);
    for x in 0..width {
        for (y, value) in line[..height].iter_mut().enumerate() {
            *value = grid[y * width + x];
        }
        squared_line(&line[..height], &mut found[..height], &mut lowest, &mut bounds);
        for (y, value) in found[..height].iter().enumerate() {
            grid[y * width + x] = *value;
        }
    }
    for row in grid.chunks_exact_mut(width.max(1)) {
        line[..width].copy_from_slice(row);
        squared_line(&line[..width], &mut found[..width], &mut lowest, &mut bounds);
        row.copy_from_slice(&found[..width]);
    }
    grid
}

fn squared_line(f: &[f32], d: &mut [f32], lowest: &mut [usize], bounds: &mut [f32]) {
    if f.is_empty() {
        return;
    }
    let meet = |q: usize, p: usize| ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
    let mut k = 0;
    lowest[0] = 0;
    bounds[0] = f32::NEG_INFINITY;
    bounds[1] = f32::INFINITY;
    for q in 1..f.len() {
        let mut s = meet(q, lowest[k]);
        while s <= bounds[k] {
            k -= 1;
            s = meet(q, lowest[k]);
        }
        k += 1;
        lowest[k] = q;
        bounds[k] = s;
        bounds[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while bounds[k + 1] < q as f32 {
            k += 1;
        }
        let along = q as f32 - lowest[k] as f32;
        *out = along * along + f[lowest[k]];
    }
}

pub(super) fn redraw(rgba: &[u8], field: &[f32], [width, height]: [usize; 2], factor: f32, [out_width, out_height]: [usize; 2]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(out_width * out_height * 4);
    for y in 0..out_height {
        for x in 0..out_width {
            let at = Vec2::new((x as f32 + 0.5) / factor - 0.5, (y as f32 + 0.5) / factor - 0.5).max(Vec2::ZERO);
            let (x0, y0) = ((at.x as usize).min(width - 1), (at.y as usize).min(height - 1));
            let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
            let (tx, ty) = ((at.x - x0 as f32).clamp(0.0, 1.0), (at.y - y0 as f32).clamp(0.0, 1.0));
            let taps = [(y0 * width + x0, (1.0 - tx) * (1.0 - ty)), (y0 * width + x1, tx * (1.0 - ty)), (y1 * width + x0, (1.0 - tx) * ty), (y1 * width + x1, tx * ty)];
            let distance: f32 = taps.iter().map(|&(texel, weight)| field[texel] * weight).sum();
            let (mut colour, mut covered) = ([0.0f32; 3], 0.0);
            for &(texel, weight) in &taps {
                covered += weight * f32::from(rgba[texel * 4 + 3]);
                for (channel, total) in colour.iter_mut().enumerate() {
                    *total += weight * f32::from(rgba[texel * 4 + channel]);
                }
            }
            let alpha = ((0.5 - distance * factor).clamp(0.0, 1.0) * 255.0).round();
            pixels.extend(colour.map(|total| if covered > 0.0 { (total * alpha / covered).round() as u8 } else { 0 }));
            pixels.push(alpha as u8);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_measures_from_the_glyph_edge() {
        assert_eq!(signed_distance(&[false, false, true, false, false], 5, 1), vec![1.5, 0.5, -0.5, 0.5, 1.5]);
        assert_eq!(signed_distance(&[false, true, true, true, false], 5, 1), vec![0.5, -0.5, -1.5, -0.5, 0.5]);
    }

    #[test]
    fn distances_are_euclidean_across_the_diagonal() {
        let mut inside = [false; 9];
        inside[4] = true;
        let field = signed_distance(&inside, 3, 3);
        assert_eq!(field[4], -0.5);
        assert_eq!(field[1], 0.5);
        assert!((field[0] - (2.0f32.sqrt() - 0.5)).abs() < 1e-6, "{}", field[0]);
    }

    #[test]
    fn a_columns_distances_match_its_rows() {
        let row = signed_distance(&[true, false, false, false], 4, 1);
        assert_eq!(signed_distance(&[true, false, false, false], 1, 4), row);
    }

    #[test]
    fn a_redrawn_page_is_solid_inside_and_clear_outside() {
        let rgba = [0, 0, 0, 0, 200, 100, 50, 255, 0, 0, 0, 0];
        let field = signed_distance(&[false, true, false], 3, 1);
        let pixels = redraw(&rgba, &field, [3, 1], 2.0, [6, 2]);
        assert_eq!(pixels.len(), 6 * 2 * 4);
        assert_eq!(&pixels[2 * 4..3 * 4], &[200, 100, 50, 255]);
        assert_eq!(pixels[3], 0);
        assert_eq!(&pixels[4 * 4..5 * 4], &[0, 0, 0, 0]);
        assert_eq!(pixels[5 * 4 + 3], 0);
    }
}
