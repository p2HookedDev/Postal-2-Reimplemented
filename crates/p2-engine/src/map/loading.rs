use bevy::image::Image;
use p2_assets::{read_actor_location, read_actor_rotation, read_level, read_model};
use p2_math::Vector;
use p2_package::PackageId;
use p2_script::Source;

use p2_bevy_assets::library::Game;
use p2_render::canvas::ProgressBar;

const BAR_MATERIAL: &str = "ProgressBarFinal";

pub fn read_progress_bar(game: &mut Game, entry: PackageId) -> Option<(ProgressBar, Image)> {
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
    Some((ProgressBar { corners, uvs, uv: texture.state.uv, size }, texture.image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_egui::egui;

    #[test]
    fn axes_of_a_quarter_turn() {
        let [forward, right, up] = p2_math::Rotator::new(0, -16384, 0).axes().map(<[f32; 3]>::from);
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(near(forward, [0.0, -1.0, 0.0]) && near(right, [1.0, 0.0, 0.0]) && near(up, [0.0, 0.0, 1.0]));
    }

    #[test]
    fn entry_progress_bar() {
        let Some(root) = std::env::var_os("P2_GAME_DIR").map(std::path::PathBuf::from) else { return };
        let mut library = p2_bevy_assets::library::Library::new(p2_host::game_dir::config_folder());
        let entry = std::path::Path::new("Maps").join("Entry.fuk").display().to_string();
        let (game, id) = library.open(&root, &entry).unwrap();
        let (bar, _) = read_progress_bar(game, id).expect("no progress bar");
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
