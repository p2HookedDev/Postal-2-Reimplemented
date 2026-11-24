use bevy_egui::egui;
use p2_assets::{UvModifier, uv_matrix};

const FOV: f32 = 85.0;

/// Most progress the engine draws: at 1 the panner would wrap back to empty.
const MAX_PROGRESS: f32 = 0.99;

pub struct ProgressBar {
    pub corners: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub uv: Vec<UvModifier>,
    pub size: [f32; 2],
}

impl ProgressBar {
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
}
