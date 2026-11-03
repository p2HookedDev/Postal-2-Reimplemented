use bevy::image::Image;
use bevy_egui::egui;
use p2_assets::{UvModifier, read_actor_location, read_actor_rotation, read_level, read_model, uv_matrix};
use p2_math::Vector;
use p2_package::PackageId;
use p2_script::Source;

use crate::library::Game;

const FOV: f32 = 85.0;

/// Most progress the engine draws: at 1 the panner would wrap back to empty.
const MAX_PROGRESS: f32 = 0.99;

const BAR_MATERIAL: &str = "ProgressBarFinal";

pub struct ProgressBar {
    corners: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    uv: Vec<UvModifier>,
    size: [f32; 2],
}

impl ProgressBar {
    pub fn read(game: &mut Game, entry: PackageId) -> Option<(Self, Image)> {
        let (view, rotation, polygon) = {
            let (pkg, data) = game.data(entry);
            let class = |i: usize| pkg.object_name(pkg.exports[i].class_index);
            let view = (0..pkg.exports.len()).find(|&i| class(i) == Some("LevelLoadingView"))?;
            let level = (0..pkg.exports.len()).find(|&i| class(i) == Some("Level"))?;
            let (source, mut objects) = (Source { package: pkg, data }, game.objects());
            let location = read_actor_location(&mut objects, source, view).ok()?;
            let rotation = read_actor_rotation(&mut objects, source, view).ok()?;
            let level = read_level(pkg, data, &pkg.exports[level]).ok()?;
            let model = pkg.exports.get(usize::try_from(level.model - 1).ok()?)?;
            let model = read_model(pkg, data, model).ok()?;
            let polygon = model.polygons().into_iter().find(|p| pkg.object_name(p.material) == Some(BAR_MATERIAL))?;
            (location, rotation, polygon)
        };
        let texture = game.ui_texture(entry, polygon.material)?;
        let size = texture.image.size().as_vec2();
        let size = [size.x.max(1.0), size.y.max(1.0)];
        let [forward, right, up] = rotation.axes();
        let corners = polygon
            .positions
            .iter()
            .map(|p| {
                let d = Vector::from(*p) - view;
                [d.dot(right), d.dot(up), d.dot(forward)]
            })
            .collect();
        let uvs = polygon.uvs.iter().map(|uv| [uv[0] / size[0], uv[1] / size[1]]).collect();
        Some((Self { corners, uvs, uv: texture.state.uv, size }, texture.image))
    }

    /// Projected like the engine's view (FOV spans a 4:3 width of the height; pixels are square), so the
    /// bar stays where it is on a 4:3 screen at any window shape.
    pub fn mesh(&self, rect: egui::Rect, progress: f32, texture: egui::TextureId) -> egui::Mesh {
        let scale = rect.height() * 4.0 / 3.0 / 2.0 / (FOV.to_radians() / 2.0).tan();
        let m = uv_matrix(&self.uv, progress.min(MAX_PROGRESS), self.size);
        let mut mesh = egui::Mesh::with_texture(texture);
        for (corner, uv) in self.corners.iter().zip(&self.uvs) {
            let z = corner[2].max(1.0);
            let pos = rect.center() + egui::vec2(corner[0] / z * scale, -corner[1] / z * scale);
            let u = m[0][0] * uv[0] + m[0][1] * uv[1] + m[0][2];
            let v = m[1][0] * uv[0] + m[1][1] * uv[1] + m[1][2];
            mesh.vertices.push(egui::epaint::Vertex { pos, uv: egui::pos2(u, v), color: egui::Color32::WHITE });
        }
        for i in 1..self.corners.len().saturating_sub(1) as u32 {
            mesh.indices.extend([0, i, i + 1]);
        }
        mesh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_of_a_quarter_turn() {
        let [forward, right, up] = p2_math::Rotator::new(0, -16384, 0).axes().map(<[f32; 3]>::from);
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(near(forward, [0.0, -1.0, 0.0]) && near(right, [1.0, 0.0, 0.0]) && near(up, [0.0, 0.0, 1.0]));
    }

    #[test]
    fn bar_scales_with_the_height() {
        let corners = vec![[-100.0, -60.0, 200.0], [100.0, -60.0, 200.0], [100.0, -80.0, 200.0], [-100.0, -80.0, 200.0]];
        let bar = ProgressBar { corners, uvs: vec![[0.0, 0.0]; 4], uv: Vec::new(), size: [1.0, 1.0] };
        let ys = |width: f32| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 600.0));
            bar.mesh(rect, 0.0, egui::TextureId::User(0)).vertices.iter().map(|v| v.pos.y).collect::<Vec<f32>>()
        };
        assert_eq!(ys(800.0), ys(2400.0));
        assert!(ys(2400.0).iter().all(|&y| y > 300.0 && y < 600.0), "{:?}", ys(2400.0));
    }

    #[test]
    fn entry_progress_bar() {
        let Some(root) = std::env::var_os("P2_GAME_DIR").map(std::path::PathBuf::from) else { return };
        let mut library = crate::library::Library::default();
        let entry = std::path::Path::new("Maps").join("Entry.fuk").display().to_string();
        let (game, id) = library.open(&root, &entry).unwrap();
        let (bar, _) = ProgressBar::read(game, id).expect("no progress bar");
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1024.0, 768.0));
        let texture = egui::TextureId::User(0);
        let (empty, full) = (bar.mesh(rect, 0.0, texture), bar.mesh(rect, 0.5, texture));
        let xs: Vec<f32> = empty.vertices.iter().map(|v| v.pos.x).collect();
        let ys: Vec<f32> = empty.vertices.iter().map(|v| v.pos.y).collect();
        eprintln!("bar x {xs:?} y {ys:?} uv {:?} -> {:?}", empty.vertices[0].uv, full.vertices[0].uv);
        assert!(xs.iter().all(|&x| (256.0..768.0).contains(&x)), "x {xs:?}");
        assert!(ys.iter().all(|&y| y > 300.0 && y < 768.0), "y {ys:?}");
        assert_ne!(empty.vertices[0].uv, full.vertices[0].uv, "the panner does not move");
    }
}
