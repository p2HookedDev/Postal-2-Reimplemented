use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use p2_assets::{
    BaseTexture, Model, RenderState, TextureFormat, base_texture, fan_triangles, read_convex_volume, read_level, read_model, read_palette, read_polys,
    read_karma_assets, read_kmesh_props, read_mesh_actor, read_mesh_animation, read_primitive_sphere, read_skeletal_mesh, read_static_mesh, read_static_mesh_instance, read_static_mesh_materials, read_sound,
    read_terrain, read_texture, texture_heights, DecoMaps, TexelMap, Texture, texture_alpha, texture_bgra,
};
use p2_package::{Export, LoadedPackage, Loader, Package, PackageId};
use p2_script::{Layouts, Objects, Source};

fn game_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("P2_GAME_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("P2_GAME_DIR unset, skipping");
    }
    dir
}

const EXTENSIONS: &[&str] = &["u", "utx", "usx", "ukx", "fuk"];

fn collect_packages(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_packages(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
}

fn mip_size(format: TextureFormat, width: u32, height: u32) -> Option<usize> {
    let (w, h) = (width as usize, height as usize);
    let blocks = w.div_ceil(4).max(1) * h.div_ceil(4).max(1);
    match format {
        TextureFormat::P8 | TextureFormat::L8 => Some(w * h),
        TextureFormat::G16 => Some(w * h * 2),
        TextureFormat::Rgba8 => Some(w * h * 4),
        TextureFormat::Dxt1 => Some(blocks * 8),
        TextureFormat::Dxt3 | TextureFormat::Dxt5 => Some(blocks * 16),
        _ => None,
    }
}

fn install_packages() -> Option<Vec<PathBuf>> {
    let dir = game_dir()?;
    let mut paths = Vec::new();
    collect_packages(&dir, &mut paths);
    Some(paths)
}

fn script_loader() -> Loader {
    let dir = game_dir().unwrap();
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    loader
}

fn games_with_maps(paths: &[PathBuf]) -> Option<Vec<(Loader, Vec<PackageId>)>> {
    let dir = game_dir()?;
    let mut games = Vec::new();
    let mut untested: Vec<&PathBuf> = paths.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk"))).collect();
    for system in [dir.join("System"), dir.join("Paradise Lost").join("System")] {
        if !system.is_dir() {
            continue;
        }
        let mut loader = Loader::open(&system).unwrap();
        loader.load_scripts().unwrap();
        let mut maps = Vec::new();
        untested.retain(|&path| {
            let name = path.file_stem().unwrap().to_str().unwrap();
            let found = loader.find(name).is_some_and(|found| found.canonicalize().ok() == path.canonicalize().ok());
            if found {
                maps.push(loader.load(name).unwrap());
            }
            !found
        });
        games.push((loader, maps));
    }
    Some(games)
}

#[test]
fn every_static_mesh() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut meshes, mut strips) = (0, 0);
    #[derive(Debug, Default)]
    struct Collision {
        triangles: usize,
        drawn: usize,
        fewer: usize,
        none: usize,
        no_nodes: usize,
        no_box: usize,
        line: usize,
        models: usize,
        models_without_nodes: usize,
    }
    let mut collision = Collision::default();
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("StaticMesh")) {
            meshes += 1;
            let name = pkg.name(export.object_name);
            let mesh = match read_static_mesh(&pkg, &data, export) {
                Ok(mesh) => mesh,
                Err(e) => {
                    failures.push(format!("{}: {name}: {e}", path.display()));
                    continue;
                }
            };
            strips += mesh.sections.iter().filter(|s| s.is_strip).count();
            let vertices = mesh.positions.len();
            let bad_uvs = mesh.uvs.iter().any(|uvs| uvs.len() != vertices);
            let bad_index = mesh.indices.iter().any(|&i| usize::from(i) >= vertices);
            let bad_section = mesh.sections.iter().any(|s| {
                let count =
                    if s.is_strip { usize::from(s.num_primitives) + 2 } else { usize::from(s.num_primitives) * 3 };
                usize::from(s.first_index) + count > mesh.indices.len()
            });
            if bad_uvs || bad_index || bad_section {
                failures.push(format!(
                    "{}: {name}: uvs {bad_uvs} indices {bad_index} sections {bad_section}",
                    path.display()
                ));
            }
            let drawn: std::collections::HashSet<[u32; 3]> = mesh
                .sections
                .iter()
                .flat_map(|s| {
                    let start = usize::from(s.first_index);
                    mesh.indices[start..start + usize::from(s.num_primitives) * 3].chunks(3).map(|t| [0, 1, 2].map(|i| u32::from(t[i])))
                })
                .collect();
            let drawn_count = mesh.sections.iter().map(|s| usize::from(s.num_primitives)).sum::<usize>();
            if mesh.collision_triangles.iter().any(|t| !drawn.contains(t)) {
                failures.push(format!("{}: {name}: collision triangle not drawn", path.display()));
            }
            collision.triangles += mesh.collision_triangles.len();
            collision.drawn += drawn_count;
            collision.fewer += usize::from(mesh.collision_triangles.len() < drawn_count);
            collision.none += usize::from(mesh.collision_triangles.is_empty());
            collision.no_nodes += usize::from(mesh.collision_nodes == 0 && !mesh.collision_triangles.is_empty());
            collision.no_box += usize::from(!mesh.use_simple_box_collision);
            collision.line += usize::from(mesh.use_simple_line_collision);
            if mesh.collision_model != 0 {
                collision.models += 1;
                let model = (mesh.collision_model > 0).then(|| &pkg.exports[mesh.collision_model as usize - 1]);
                match model.filter(|e| pkg.object_name(e.class_index) == Some("Model")).map(|e| read_model(&pkg, &data, e)) {
                    Some(Ok(model)) => collision.models_without_nodes += usize::from(model.nodes.is_empty()),
                    other => failures.push(format!("{}: {name}: collision model {} {:?}", path.display(), mesh.collision_model, other.map(Result::err))),
                }
            }
        }
    }
    eprintln!("{meshes} static meshes checked, {strips} strip sections; collision: {collision:?}");
    assert!(meshes > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_kmesh_props() {
    const IDENTITY: [[f32; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let Some(paths) = install_packages() else { return };
    let (mut failures, mut outliers) = (Vec::new(), Vec::new());
    let (mut props, mut referenced, mut counts, mut checked, mut moved_hulls) = (0, 0, [0usize; 4], 0, 0);
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("KMeshProps")) {
            props += 1;
            match read_kmesh_props(&pkg, &data, export) {
                Ok(p) => {
                    let g = &p.geom;
                    for (count, n) in counts.iter_mut().zip([g.spheres.len(), g.boxes.len(), g.cylinders.len(), g.convexes.len()]) {
                        *count += n;
                    }
                }
                Err(e) => failures.push(format!("{}: {}: {e}", path.display(), pkg.name(export.object_name))),
            }
        }
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("StaticMesh")) {
            let name = pkg.name(export.object_name);
            let reference = read_static_mesh(&pkg, &data, export).unwrap().physics_props;
            if reference == 0 {
                continue;
            }
            referenced += 1;
            let Some(target) = usize::try_from(reference - 1).ok().and_then(|i| pkg.exports.get(i)) else {
                failures.push(format!("{}: {name}: KPhysicsProps {reference} is not an export", path.display()));
                continue;
            };
            if pkg.object_name(target.class_index) != Some("KMeshProps") {
                failures.push(format!("{}: {name}: KPhysicsProps is a {:?}", path.display(), pkg.object_name(target.class_index)));
                continue;
            }
            let (Ok(mesh), Ok(p)) = (read_static_mesh(&pkg, &data, export), read_kmesh_props(&pkg, &data, target)) else { continue };
            let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
            for p in &mesh.positions {
                for i in 0..3 {
                    min[i] = min[i].min(p[i]);
                    max[i] = max[i].max(p[i]);
                }
            }
            // Shapes are in Karma units: Unreal units / 50.
            let inside = |p: [f32; 3]| {
                (0..3).all(|i| {
                    let slack = (max[i] - min[i]) * 0.25 + 2.0;
                    (min[i] - slack..=max[i] + slack).contains(&(p[i] * 50.0))
                })
            };
            let extent = (0..3).map(|i| max[i] - min[i]).fold(0.0, f32::max);
            for b in &p.geom.boxes {
                checked += 1;
                let largest = b.size.iter().copied().fold(0.0, f32::max) * 50.0;
                let origin = [b.tm[3][0], b.tm[3][1], b.tm[3][2]];
                if largest > extent * 1.5 + 2.0 || !inside(origin) {
                    outliers.push(format!("{}: {name}: box {:?} at {origin:?}, mesh {min:?}..{max:?}", path.display(), b.size));
                }
            }
            for c in &p.geom.convexes {
                checked += 1;
                if c.tm != IDENTITY {
                    moved_hulls += 1;
                }
                let m = c.tm;
                let placed = |v: [f32; 3]| [0, 1, 2].map(|i| v[0] * m[0][i] + v[1] * m[1][i] + v[2] * m[2][i] + m[3][i]);
                if let Some(v) = c.vertices.iter().find(|&&v| !inside(placed(v))) {
                    outliers.push(format!("{}: {name}: hull vertex {v:?}, mesh {min:?}..{max:?}", path.display()));
                }
            }
        }
    }
    eprintln!(
        "{props} KMeshProps ({referenced} referenced): spheres, boxes, cylinders, convexes {counts:?}; {moved_hulls} hulls \
         with a non-identity TM; {} of {checked} shapes well outside their mesh:\n{}",
        outliers.len(),
        outliers.join("\n")
    );
    assert!(props > 0 && checked > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(outliers.len() * 100 < checked, "{} outliers", outliers.len());
}

#[test]
fn every_karma_asset() {
    let Some(dir) = game_dir() else { return };
    let mut files = Vec::new();
    for folder in [dir.join("KarmaData"), dir.join("Paradise Lost").join("KarmaData")] {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        files.extend(entries.map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ka"))));
    }
    assert!(files.len() >= 10, "{files:?}");
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let assets = read_karma_assets(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for asset in &assets {
            let part = |name: &str| asset.parts.iter().any(|p| p.id == name);
            for joint in &asset.joints {
                assert!(joint.parts.iter().all(|p| part(p)), "{}: {}: joint {:?}", path.display(), asset.id, joint.parts);
            }
            for p in &asset.parts {
                assert!(asset.models.iter().any(|m| m.id == p.model), "{}: {}: part {} without model", path.display(), asset.id, p.id);
            }
        }
    }
}

#[test]
fn karma_parts_are_bone_frames() {
    let Some(dir) = game_dir() else { return };
    let data = std::fs::read(dir.join("Animations").join("Characters.ukx")).unwrap();
    let pkg = Package::parse(&data).unwrap();
    for (file, mesh_name) in [("Avg_Fem.ka", "Fem_LS_Pants"), ("Avg_Dude.ka", "Avg_Dude"), ("Avg_Fat.ka", "Fat_M_SS_Pants"), ("Avg_Big.ka", "Big_M_LS_Pants")] {
        let text = std::fs::read_to_string(dir.join("KarmaData").join(file)).unwrap();
        let asset = read_karma_assets(&text).unwrap().remove(0);
        let export = pkg.exports.iter().find(|e| pkg.name(e.object_name) == mesh_name).unwrap();
        let mesh = read_skeletal_mesh(&pkg, &data, export).unwrap();
        let names: Vec<String> = mesh.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
        let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
        let bind: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
        let frames = p2_assets::pose::bone_frames(&parents, &bind, |_| 1.0);
        let bone = |part: &str| names.iter().position(|n| *n == part).unwrap_or_else(|| panic!("{file}: no bone {part}"));
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let (mut worst_position, mut worst_axis) = (0.0_f32, 1.0_f32);
        for joint in &asset.joints {
            let (child, parent) = (&frames[bone(&joint.parts[0])], &frames[bone(&joint.parts[1])]);
            let offset = [0, 1, 2].map(|i| child.origin[i] - parent.origin[i]);
            let local = parent.axes.map(|axis| dot(offset, axis) * asset.scale);
            let error = (0..3).map(|i| (local[i] - joint.positions[1][i]).powi(2)).sum::<f32>().sqrt();
            let primary = |frame: &p2_assets::pose::Frame, axis: [f32; 3]| frame.transform_vector(axis);
            let agree = dot(primary(child, joint.primary_axes[0]), primary(parent, joint.primary_axes[1]));
            worst_position = worst_position.max(error);
            worst_axis = worst_axis.min(agree);
        }
        // The meshes' proportions differ a little from the skeleton the asset was made on (up to ~7 Unreal units).
        assert!(worst_position < 0.2, "{file}: joint positions do not fit the bones ({worst_position})");
        assert!(worst_axis > 0.95, "{file}: joint axes do not fit the bones ({worst_axis})");
    }
}

#[test]
fn every_skeletal_mesh() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut meshes, mut faces, mut bones, mut sections_past_materials) = (0, 0, 0, 0);
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("SkeletalMesh")) {
            meshes += 1;
            let name = pkg.name(export.object_name);
            let mesh = match read_skeletal_mesh(&pkg, &data, export) {
                Ok(mesh) => mesh,
                Err(e) => {
                    failures.push(format!("{}: {name}: {e}", path.display()));
                    continue;
                }
            };
            let radius = read_primitive_sphere(&pkg, &data, export).unwrap()[3];
            assert!(radius.is_finite() && radius >= 0.0, "{name}: sphere radius {radius}");
            faces += mesh.indices.len() / 3;
            bones += mesh.bones.len();
            let bad_wedge = mesh.wedges.iter().any(|w| w.point as usize >= mesh.positions.len());
            let bad_index = mesh.indices.len() % 3 != 0 || mesh.indices.iter().any(|&i| usize::from(i) >= mesh.wedges.len());
            // Material indices past `materials` exist (they rely on the actor's `Skins` or fall back to material 0), so they are
            // only counted.
            let mut next_face = 0;
            let bad_section = mesh.sections.iter().any(|s| {
                let ok = usize::from(s.first_face) == next_face;
                next_face += usize::from(s.num_faces);
                !ok
            }) || next_face * 3 != mesh.indices.len();
            sections_past_materials += mesh.sections.iter().filter(|s| usize::from(s.material) >= mesh.materials.len()).count();
            let bad_bone = mesh.bones.iter().enumerate().any(|(i, b)| {
                let length = b.orientation.iter().map(|c| c * c).sum::<f32>().sqrt();
                b.parent > i || (i > 0 && b.parent == i) || (length - 1.0).abs() > 1e-3
            });
            if mesh.indices.is_empty() || bad_wedge || bad_index || bad_section || bad_bone {
                failures.push(format!(
                    "{}: {name}: indices {} wedges {bad_wedge} indices {bad_index} sections {bad_section} bones {bad_bone}",
                    path.display(),
                    mesh.indices.len()
                ));
            }
        }
    }
    eprintln!(
        "{meshes} skeletal meshes checked, {faces} LOD 0 faces, {bones} bones, \
         {sections_past_materials} sections with a material index past the mesh's materials"
    );
    assert!(meshes > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_skeletal_mesh_skin_weights() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut meshes, mut points, mut influences, mut max_influences) = (0, 0, 0, 0);
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("SkeletalMesh")) {
            meshes += 1;
            let name = pkg.name(export.object_name);
            let mesh = read_skeletal_mesh(&pkg, &data, export).unwrap();
            points += mesh.positions.len();
            influences += mesh.influences.len();
            max_influences = max_influences.max(mesh.point_influences().map(<[_]>::len).max().unwrap_or(0));
            let bad = mesh.influence_starts.len() != mesh.positions.len() + 1
                || mesh.point_influences().any(|point| {
                    let sum: f32 = point.iter().map(|i| i.weight).sum();
                    point.is_empty()
                        || (sum - 1.0).abs() > 1e-4
                        || point.iter().any(|i| usize::from(i.bone) >= mesh.bones.len() || i.weight < 0.0)
                });
            if bad {
                failures.push(format!("{}: {name}", path.display()));
            }
        }
    }
    eprintln!("{meshes} skeletal meshes, {points} LOD 0 points, {influences} influences, at most {max_influences} per point");
    assert!(meshes > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

const NON_UNIT_SEQUENCES: &[(&str, &str)] = &[("animFat", "sb_base"), ("animKrotchy", "cd_base")];

#[test]
fn every_mesh_animation() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut animations, mut sequences, mut tracks, mut keys, mut constant_positions) = (0, 0, 0, 0, 0);
    let (mut notifies, mut extra_tracks, mut non_unit) = (0, 0, Vec::new());
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("MeshAnimation")) {
            animations += 1;
            let name = pkg.name(export.object_name);
            let anim = match read_mesh_animation(&pkg, &data, export) {
                Ok(anim) => anim,
                Err(e) => {
                    failures.push(format!("{}: {name}: {e}", path.display()));
                    continue;
                }
            };
            if anim.version != 0 {
                failures.push(format!("{}: {name}: version {}", path.display(), anim.version));
            }
            let bad_bones = anim.bones.iter().enumerate().any(|(i, b)| usize::try_from(b.parent).map_or(true, |p| p > i));
            for seq in &anim.sequences {
                let seq_name = pkg.name(seq.name);
                sequences += 1;
                tracks += seq.tracks.len();
                notifies += seq.notifies.len();
                extra_tracks += usize::from(seq.tracks.len() > anim.bones.len());
                let mut problems = Vec::new();
                if seq.tracks.len() < anim.bones.len() {
                    problems.push(format!("{} tracks for {} bones", seq.tracks.len(), anim.bones.len()));
                }
                let identity = seq.bone_indices.iter().enumerate().all(|(i, &b)| usize::try_from(b) == Ok(i));
                if !(seq.bone_indices.is_empty() || (identity && seq.bone_indices.len() == seq.tracks.len())) {
                    problems.push(format!("bone indices {:?}", &seq.bone_indices[..seq.bone_indices.len().min(8)]));
                }
                if seq.track_time != seq.num_frames as f32 || !seq.root_track.times.is_empty() {
                    problems.push(format!("track time {} for {} frames", seq.track_time, seq.num_frames));
                }
                if seq.start_bone != 0 || seq.flags != 0 || seq.start_frame != 0 {
                    problems.push(format!("start bone {} flags {} start frame {}", seq.start_bone, seq.flags, seq.start_frame));
                }
                let mut unit = true;
                for track in &seq.tracks {
                    let n = track.times.len();
                    keys += n;
                    constant_positions += usize::from(n > 1 && track.positions.len() == 1);
                    if track.rotations.len() != n || (track.positions.len() != n && track.positions.len() != 1) {
                        problems.push(format!("{} rotations, {} positions for {n} times", track.rotations.len(), track.positions.len()));
                    }
                    let increasing = track.times.windows(2).all(|w| w[0] < w[1]);
                    if n == 0 || !increasing || track.times[0] != 0.0 || track.times[n - 1] >= seq.track_time {
                        problems.push(format!("key times {:?}", &track.times[..n.min(8)]));
                    }
                    unit &= track.rotations.iter().all(|q| (q.iter().map(|c| c * c).sum::<f32>().sqrt() - 1.0).abs() < 1e-3);
                }
                if !unit {
                    non_unit.push((name.to_owned(), seq_name.to_owned()));
                }
                if !problems.is_empty() || bad_bones {
                    problems.dedup();
                    failures.push(format!("{}: {name}.{seq_name}: bones {bad_bones} {problems:?}", path.display()));
                }
            }
        }
    }
    eprintln!(
        "{animations} mesh animations, {sequences} sequences, {tracks} tracks, {keys} keys, {notifies} notifies, \
         {constant_positions} tracks with one position key, {extra_tracks} sequences with more tracks than bones, \
         non-unit quaternions in {non_unit:?}"
    );
    assert!(animations > 0);
    assert_eq!(non_unit.iter().map(|(a, s)| (a.as_str(), s.as_str())).collect::<Vec<_>>(), NON_UNIT_SEQUENCES);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let ([ax, ay, az, aw], [bx, by, bz, bw]) = (a, b);
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn conjugate(q: [f32; 4]) -> [f32; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let r = quat_mul(quat_mul(q, [v[0], v[1], v[2], 0.0]), conjugate(q));
    [r[0], r[1], r[2]]
}

fn bone_to_mesh(locals: &[([f32; 4], [f32; 3])], parents: &[usize], conjugate_children: bool) -> Vec<([f32; 4], [f32; 3])> {
    let mut out: Vec<([f32; 4], [f32; 3])> = Vec::new();
    for (i, &(q, p)) in locals.iter().enumerate() {
        if i == 0 {
            out.push((q, p));
            continue;
        }
        let q = if conjugate_children { conjugate(q) } else { q };
        let (parent_q, parent_p) = out[parents[i]];
        let offset = rotate(parent_q, p);
        out.push((quat_mul(parent_q, q), [0, 1, 2].map(|k| parent_p[k] + offset[k])));
    }
    out
}

/// Skinning a mid-walk pose of `animAvg_PL` onto `Avg_M_SS_Shorts_PB_D` keeps the mesh together only when non-root
/// quaternions are conjugated.
#[test]
fn animation_pose_convention() {
    let Some(dir) = game_dir() else { return };
    let data = std::fs::read(dir.join("Animations").join("Characters.ukx")).unwrap();
    let pkg = Package::parse(&data).unwrap();
    let export = |name: &str| pkg.exports.iter().find(|e| pkg.name(e.object_name) == name).unwrap();
    let mesh = read_skeletal_mesh(&pkg, &data, export("Avg_M_SS_Shorts_PB_D")).unwrap();
    let anim = read_mesh_animation(&pkg, &data, export("animAvg_PL")).unwrap();
    let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
    let bind: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
    let tracks: Vec<usize> = mesh
        .bones
        .iter()
        .map(|b| anim.bones.iter().position(|a| pkg.name(a.name).eq_ignore_ascii_case(pkg.name(b.name))).unwrap())
        .collect();
    let walk = anim.sequences.iter().position(|s| pkg.name(s.name) == "pl_smoking_walk").unwrap();
    let pose = anim.pose(walk, anim.sequences[walk].num_frames as f32 * 0.4);
    let posed: Vec<_> = tracks.iter().map(|&t| pose[t]).collect();

    let stretch = |conjugate_children: bool| {
        let (from, to) = (bone_to_mesh(&bind, &parents, conjugate_children), bone_to_mesh(&posed, &parents, conjugate_children));
        let skinned: Vec<[f32; 3]> = mesh
            .positions
            .iter()
            .zip(mesh.point_influences())
            .map(|(v, influences)| {
                let mut out = [0.0; 3];
                for i in influences {
                    let ((bind_q, bind_p), (pose_q, pose_p)) = (from[usize::from(i.bone)], to[usize::from(i.bone)]);
                    let r = rotate(pose_q, rotate(conjugate(bind_q), [0, 1, 2].map(|k| v[k] - bind_p[k])));
                    (0..3).for_each(|k| out[k] += i.weight * (pose_p[k] + r[k]));
                }
                out
            })
            .collect();
        let length = |a: [f32; 3], b: [f32; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>().sqrt();
        let (mut total, mut edges) = (0.0, 0);
        for tri in mesh.indices.chunks(3) {
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                let (a, b) = (mesh.wedges[usize::from(a)].point as usize, mesh.wedges[usize::from(b)].point as usize);
                let bind_length = length(mesh.positions[a], mesh.positions[b]);
                if bind_length > 0.1 {
                    total += (length(skinned[a], skinned[b]) / bind_length - 1.0).abs();
                    edges += 1;
                }
            }
        }
        total / edges as f32
    };
    let (conjugated, stored) = (stretch(true), stretch(false));
    assert!(conjugated < 0.05 && stored > 0.3);
}

const MATERIAL_CLASSES: &[&str] = &[
    "Texture",
    "Cubemap",
    "MovieTexture",
    "WetTexture",
    "Shader",
    "Combiner",
    "FinalBlend",
    "ColorModifier",
    "OpacityModifier",
    "MaterialSwitch",
    "TexPanner",
    "TexScaler",
    "TexRotator",
    "TexOscillator",
    "TexCoordSource",
    "TexEnvMap",
    "ConstantColor",
    "VertexColor",
];

#[derive(Debug, Default)]
struct Walks {
    texture: usize,
    color: usize,
    import: BTreeMap<String, usize>,
    none: BTreeMap<String, usize>,
}

impl Walks {
    fn add(&mut self, pkg: &Package, start: i32, result: BaseTexture) {
        let class = |index: i32| match index {
            ..0 => pkg.name(pkg.imports[index.unsigned_abs() as usize - 1].class_name).to_string(),
            _ => pkg.object_name(pkg.exports[index as usize - 1].class_index).unwrap_or_default().to_string(),
        };
        match result {
            BaseTexture::Texture(_) => self.texture += 1,
            BaseTexture::Color => self.color += 1,
            BaseTexture::Import(import) => *self.import.entry(class(import)).or_default() += 1,
            BaseTexture::None | BaseTexture::Cubemap(_) => *self.none.entry(class(start)).or_default() += 1,
        }
    }
}

#[test]
fn every_static_mesh_materials() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut meshes, mut materials, mut empty) = (0, 0, 0);
    let mut walks = Walks::default();
    let loader = script_loader();
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("StaticMesh")) {
            meshes += 1;
            let name = pkg.name(export.object_name);
            let (mesh, refs) =
                match read_static_mesh(&pkg, &data, export).and_then(|m| Ok((m, read_static_mesh_materials(&pkg, &data, export)?))) {
                    Ok(result) => result,
                    Err(e) => {
                        failures.push(format!("{}: {name}: {e}", path.display()));
                        continue;
                    }
                };
            if refs.len() != mesh.sections.len() {
                failures.push(format!("{}: {name}: {} materials, {} sections", path.display(), refs.len(), mesh.sections.len()));
            }
            materials += refs.len();
            for &material in &refs {
                if material == 0 {
                    empty += 1;
                    continue;
                }
                let source = Source { package: &pkg, data: &data };
                match base_texture(&mut Objects::new(&loader), source, material, &mut RenderState::default()) {
                    Ok(result) => walks.add(&pkg, material, result),
                    Err(e) => failures.push(format!("{}: {name}: {e}", path.display())),
                }
            }
        }
    }
    eprintln!("{meshes} static meshes, {materials} section materials ({empty} none); base texture walks: {walks:?}");
    assert!(meshes > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_material_base_texture() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let mut walks = Walks::default();
    let (mut blendings, mut two_sided, mut animated) = (BTreeMap::new(), 0, 0);
    let loader = script_loader();
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let mut objects = Objects::new(&loader);
        for (i, export) in pkg.exports.iter().enumerate() {
            if !pkg.object_name(export.class_index).is_some_and(|c| MATERIAL_CLASSES.contains(&c)) {
                continue;
            }
            let material = i as i32 + 1;
            let mut state = RenderState::default();
            match base_texture(&mut objects, Source { package: &pkg, data: &data }, material, &mut state) {
                Ok(result) => walks.add(&pkg, material, result),
                Err(e) => failures.push(format!("{}: {}: {e}", path.display(), pkg.name(export.object_name))),
            }
            *blendings.entry(format!("{:?}", state.blending)).or_insert(0) += 1;
            animated += usize::from(!state.uv.is_empty());
            two_sided += usize::from(state.two_sided);
        }
    }
    eprintln!("material exports, base texture walks: {walks:?}; blending {blendings:?}, {two_sided} two-sided, {animated} with UV animation");
    assert!(walks.texture > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_model_polys_level() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut models, mut nodes, mut surfaces, mut polygons, mut triangles) = (0, 0, 0, 0, 0);
    let (mut polys, mut levels, mut maps) = (0, 0, 0);
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let mut is_map = false;
        for export in &pkg.exports {
            let name = pkg.name(export.object_name);
            let fail = |e: &dyn std::fmt::Display| format!("{}: {name}: {e}", path.display());
            match pkg.object_name(export.class_index) {
                Some("Model") => {
                    models += 1;
                    match read_model(&pkg, &data, export) {
                        Ok(model) => {
                            nodes += model.nodes.len();
                            surfaces += model.surfaces.len();
                            let polys = model.polygons();
                            polygons += polys.len();
                            triangles += polys.iter().map(|p| fan_triangles(p.positions.len()).count()).sum::<usize>();
                            if let Some(e) = section_mismatch(&model).or_else(|| light_map_mismatch(&model)) {
                                failures.push(fail(&e));
                            }
                        }
                        Err(e) => failures.push(fail(&e)),
                    }
                }
                Some("Polys") => {
                    polys += 1;
                    if let Err(e) = read_polys(&pkg, &data, export) {
                        failures.push(fail(&e));
                    }
                }
                Some("Level") => {
                    levels += 1;
                    is_map = true;
                    match read_level(&pkg, &data, export) {
                        Ok(level) if level.model > 0 => {
                            if pkg.object_name(pkg.exports[level.model as usize - 1].class_index) != Some("Model") {
                                failures.push(fail(&"level model is not a Model"));
                            }
                        }
                        Ok(_) => failures.push(fail(&"level without model")),
                        Err(e) => failures.push(fail(&e)),
                    }
                }
                _ => {}
            }
        }
        maps += usize::from(is_map);
    }
    eprintln!(
        "{maps} maps: {levels} levels, {models} models ({nodes} nodes, {surfaces} surfaces, \
         {polygons} polygons, {triangles} triangles), {polys} polys"
    );
    assert!(models > 0 && levels > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_convex_volume() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut volumes, mut faces) = (0, 0);
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("ConvexVolume")) {
            let fail = |e: &dyn std::fmt::Display| format!("{}: {}: {e}", path.display(), pkg.name(export.object_name));
            match read_convex_volume(&pkg, &data, export) {
                Ok(volume) => {
                    volumes += 1;
                    faces += volume.faces.len();
                    let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 0.5);
                    let shared = volume.edges.iter().all(|e| {
                        let first = volume.faces[e.faces[0]].vertices[e.vertices[0]];
                        volume.faces[e.faces[1]].vertices.iter().any(|&v| near(v, first))
                    });
                    let sheet = volume.faces.len() == 2
                        && volume.faces[0].vertices.iter().all(|&a| volume.faces[1].vertices.iter().any(|&b| near(a, b)));
                    if (volume.faces.len() < 4 && !sheet) || !shared {
                        failures.push(fail(&format!("{} faces, edges shared: {shared}", volume.faces.len())));
                    }
                }
                Err(e) => failures.push(fail(&e)),
            }
        }
    }
    eprintln!("{volumes} convex volumes, {faces} faces");
    assert!(volumes > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

fn light_map_mismatch(model: &Model) -> Option<String> {
    for (i, lm) in model.light_maps.iter().enumerate() {
        let Some(texture) = usize::try_from(lm.texture).ok().and_then(|t| model.light_map_textures.get(t)) else {
            return Some(format!("light map {i}: texture {} out of range", lm.texture));
        };
        let surface = usize::try_from(lm.surface).ok().filter(|&s| s < model.surfaces.len());
        let nodes_ok = model.nodes.iter().filter(|n| n.light_map == Some(i)).all(|n| Some(n.surface) == surface);
        let fits = texture.data[0].is_empty()
            || (lm.offset[0] + lm.size[0] <= texture.width
                && lm.offset[1] + lm.size[1] <= texture.height
                && texture.light_maps.contains(&(i as i32)));
        if surface.is_none() || !nodes_ok || !fits {
            return Some(format!("light map {i}: {lm:?}"));
        }
    }
    None
}

fn section_mismatch(model: &Model) -> Option<String> {
    for (i, section) in model.sections.iter().enumerate() {
        let nodes = model.nodes.iter().filter(|n| n.section == Some(i)).count();
        if usize::try_from(section.num_nodes) != Ok(nodes) {
            return Some(format!("section {i}: NumNodes {} but {nodes} nodes", section.num_nodes));
        }
    }
    for (i, node) in model.nodes.iter().enumerate().filter(|(_, n)| n.num_vertices > 0) {
        let Some(section) = node.section else { continue };
        let count = usize::from(node.num_vertices);
        let vertices = usize::try_from(node.first_vertex)
            .ok()
            .and_then(|first| model.sections[section].vertices.get(first..first + count));
        let Some(vertices) = vertices else { return Some(format!("node {i}: section vertices out of range")) };
        let pool = &model.verts[node.vert_pool..node.vert_pool + count];
        let same = pool.iter().zip(vertices).all(|(v, sv)| {
            let p = model.points[v.point];
            (0..3).all(|k| (p[k] - sv.position[k]).abs() < 0.01)
        });
        if !same {
            return Some(format!("node {i}: section positions differ from vertex pool"));
        }
    }
    None
}

/// Known exception: `PL-suburbs-2.fuk`, whose `Layers[3]` tag size is too small (the `SoundThing` problem); it needs
/// declaration-driven property reading.
#[test]
fn every_terrain() {
    let Some(paths) = install_packages() else { return };
    let Some(games) = games_with_maps(&paths) else { return };
    let mut failures = Vec::new();
    let (mut terrains, mut layers, mut triangles, mut rebuilt, mut stale) = (0, 0, 0, 0, 0);
    for (loader, maps) in &games {
        let mut layouts = Layouts::new(loader);
        for &id in maps {
        let LoadedPackage { path, package: pkg, data, .. } = loader.package(id);
        let source = Source { package: pkg, data };
        for (index, export) in pkg.exports.iter().enumerate() {
            if pkg.object_name(export.class_index) != Some("TerrainInfo") {
                continue;
            }
            terrains += 1;
            let terrain = match layouts.class_layout(source, index).map_err(p2_assets::Error::from).and_then(|layout| read_terrain(pkg, data, export, &layout)) {
                Ok(terrain) => terrain,
                Err(e) => {
                    failures.push(format!("{}: {}: {e}", path.display(), pkg.name(export.object_name)));
                    continue;
                }
            };
            layers += terrain.layers.iter().filter(|l| l.texture != 0).count();
            triangles += terrain.triangles().len();
            stale += usize::from(terrain.vertices.is_empty());
            let heights = (terrain.heightmap > 0)
                .then(|| read_texture(&mut Objects::new(loader), source, terrain.heightmap as usize - 1).ok())
                .flatten()
                .and_then(|texture| texture_heights(&texture));
            if let Some((_, _, heights)) = heights.filter(|_| !terrain.vertices.is_empty()) {
                rebuilt += 1;
                let same = terrain.vertices_from_heights(&heights).iter().zip(&terrain.vertices).all(|(a, b)| {
                    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
                });
                if !same {
                    failures.push(format!("{}: rebuilt vertices differ from saved", path.display()));
                }
            }
        }
        }
    }
    eprintln!(
        "{terrains} terrains, {layers} textured layers, {triangles} visible triangles; \
         {rebuilt} rebuilt from embedded heightmaps match, {stale} with stale saved vertices"
    );
    assert!(terrains > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

fn load_texture(loader: &mut Loader, id: PackageId, reference: i32) -> Option<(Texture, Option<Vec<[u8; 4]>>)> {
    let resolved = loader.resolve(id, reference).ok()??;
    let LoadedPackage { package, data, .. } = loader.package(resolved.package);
    let texture = read_texture(&mut Objects::new(&*loader), Source { package, data }, resolved.export?).ok()?;
    let palette = texture.palette.and_then(|palette| {
        let resolved = loader.resolve(resolved.package, palette).ok()??;
        let LoadedPackage { package: pkg, data, .. } = loader.package(resolved.package);
        read_palette(pkg, data, &pkg.exports[resolved.export?]).ok()
    });
    Some((texture, palette))
}

#[test]
fn every_terrain_decorations() {
    let Some(paths) = install_packages() else { return };
    let Some(mut games) = games_with_maps(&paths) else { return };
    let mut failures = Vec::new();
    let (mut terrains, mut layers, mut placed, mut reported) = (0, 0, 0, 0);
    for (loader, maps) in &mut games {
        // Terrains first: the layouts borrow the loader, which texture loading needs mutably.
        let mut found = Vec::new();
        {
            let mut layouts = Layouts::new(&*loader);
            for &id in maps.iter() {
                let LoadedPackage { package: pkg, data, .. } = loader.package(id);
                let source = Source { package: pkg, data };
                for (index, export) in pkg.exports.iter().enumerate() {
                    if pkg.object_name(export.class_index) == Some("TerrainInfo") {
                        let layout = layouts.class_layout(source, index).unwrap();
                        found.push((id, read_terrain(pkg, data, export, &layout).unwrap()));
                    }
                }
            }
        }
        for (id, terrain) in found {
            terrains += 1;
            if terrain.deco_layers.is_empty() {
                continue;
            }
            let map = loader.package(id).name.clone();
            let vertices = if terrain.vertices.is_empty() {
                let Some((_, _, heights)) = load_texture(loader, id, terrain.heightmap).and_then(|(t, _)| texture_heights(&t)) else {
                    failures.push(format!("{map}: heightmap missing"));
                    continue;
                };
                terrain.vertices_from_heights(&heights)
            } else {
                terrain.vertices.clone()
            };
            let mut counts = Vec::new();
            for layer in &terrain.deco_layers {
                layers += 1;
                let density = load_texture(loader, id, layer.density_map).and_then(|(t, p)| texture_alpha(&t, p.as_deref()));
                let scale = load_texture(loader, id, layer.scale_map).and_then(|(t, _)| texture_bgra(&t));
                let color = load_texture(loader, id, layer.color_map).and_then(|(t, _)| texture_bgra(&t));
                let maps = DecoMaps {
                    density: density.as_ref().map(|(w, h, t)| TexelMap { width: *w as usize, height: *h as usize, texels: t }),
                    scale: scale.as_ref().map(|(w, h, t)| TexelMap { width: *w as usize, height: *h as usize, texels: t }),
                    color: color.as_ref().map(|(w, h, t)| TexelMap { width: *w as usize, height: *h as usize, texels: t }),
                };
                let decorations = terrain.decorations(layer, &vertices, &maps);
                counts.push(decorations.len());
                placed += decorations.len();
                for d in &decorations {
                    if let Some(problem) = off_surface(&terrain, &vertices, d.location.into()) {
                        failures.push(format!("{map}: {problem}: {d:?}"));
                        break;
                    }
                    let in_range = (0..3).all(|i| {
                        let [a, b] = layer.scale_multiplier[i];
                        d.scale[i] <= a.max(b) + 1e-4 && (layer.scale_map != 0 || d.scale[i] >= a.min(b) - 1e-4)
                    });
                    if !in_range || (!layer.align_to_terrain && d.rotation.pitch != -1) {
                        failures.push(format!("{map}: scale or pitch: {layer:?} {d:?}"));
                        break;
                    }
                }
            }
            if reported < 12 {
                reported += 1;
                eprintln!("{map}: {} deco layers, placements {counts:?}", terrain.deco_layers.len());
            }
        }
    }
    eprintln!("{terrains} terrains, {layers} decoration layers, {placed} placements");
    assert!(terrains > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

fn off_surface(terrain: &p2_assets::Terrain, vertices: &[[f32; 3]], p: [f32; 3]) -> Option<String> {
    let (w, h) = (terrain.width, terrain.height);
    let cell = |i: usize| ((p[i] - terrain.location[i]) / terrain.scale[i]) + ((if i == 0 { w } else { h }) / 2) as f32;
    let (fx, fy) = (cell(0), cell(1));
    if fx < -0.01 || fy < -0.01 || fx > (w - 1) as f32 + 0.01 || fy > (h - 1) as f32 + 0.01 {
        return Some(format!("outside the heightmap at ({fx}, {fy})"));
    }
    let (x, y) = ((fx.max(0.0) as usize).min(w - 2), (fy.max(0.0) as usize).min(h - 2));
    let corners = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)].map(|(x, y)| vertices[y * w + x][2]);
    let (low, high) = corners.iter().fold((f32::MAX, f32::MIN), |(l, h), &z| (l.min(z), h.max(z)));
    (p[2] < low - 0.05 || p[2] > high + 0.05).then(|| format!("z {} outside quad heights {low}..{high}", p[2]))
}

#[test]
fn every_mesh_actor() {
    let Some(paths) = install_packages() else { return };
    let mut failures = Vec::new();
    let (mut maps, mut actors, mut lit, mut stale) = (0, 0, 0, 0);
    let games = games_with_maps(&paths).unwrap();
    for (loader, path, pkg, data) in games.iter().flat_map(|(loader, ids)| {
        ids.iter().map(move |&id| {
            let loaded = loader.package(id);
            (loader, &loaded.path, &loaded.package, &loaded.data[..])
        })
    }) {
        maps += 1;
        let mut objects = Objects::new(loader);
        for (index, export) in pkg.exports.iter().enumerate() {
            if export.serial_size == 0 || pkg.object_name(export.class_index) == Some("SoundThing") {
                continue;
            }
            // Not actors, and no class to read them by: the classes `Core` and `Engine` define natively, with no script
            // class in their packages; `MPDancers`, from a package that does not ship (`Strippers`); classes themselves.
            const NOT_READABLE: [&str; 13] = [
                "Level", "Model", "Polys", "ConvexVolume", "StaticMeshInstance", "TerrainSector", "Package", "TextBuffer", "StaticMesh",
                "Sound", "KMeshProps", "Function", "MPDancers",
            ];
            if pkg.object_name(export.class_index).is_none_or(|class| NOT_READABLE.contains(&class)) {
                continue;
            }
            match read_mesh_actor(&mut objects, Source { package: pkg, data }, index) {
                Ok(Some(actor)) => {
                    actors += 1;
                    if actor.instance > 0 {
                        let instance = &pkg.exports[actor.instance as usize - 1];
                        match read_static_mesh_instance(pkg, data, instance) {
                            Ok(p2_assets::StaticMeshInstance { colors, .. }) => {
                                lit += 1;
                                let in_map = std::ptr::eq(actor.mesh_package, pkg) && actor.static_mesh > 0;
                                let mesh = in_map.then(|| &pkg.exports[actor.static_mesh as usize - 1]);
                                if let Some(mesh) = mesh.and_then(|mesh| read_static_mesh(pkg, data, mesh).ok())
                                    && !colors.is_empty()
                                    && colors.len() != mesh.positions.len()
                                {
                                    stale += 1;
                                }
                            }
                            Err(e) => failures.push(format!("{}: instance: {e}", path.display())),
                        }
                    }
                    let mesh_pkg = actor.mesh_package;
                    let class = match actor.static_mesh {
                        ..0 => mesh_pkg.imports.get((-actor.static_mesh - 1) as usize).map(|i| mesh_pkg.name(i.class_name)),
                        _ => mesh_pkg.exports.get(actor.static_mesh as usize - 1).and_then(|e| mesh_pkg.object_name(e.class_index)),
                    };
                    if class != Some("StaticMesh") {
                        failures.push(format!("{}: {}: mesh class {class:?}", path.display(), pkg.name(export.object_name)));
                    }
                }
                Ok(None) => {}
                Err(e) => failures.push(format!("{}: {} of class {:?}: {e}", path.display(), pkg.name(export.object_name), pkg.object_name(export.class_index))),
            }
        }
    }
    eprintln!("{maps} maps, {actors} static mesh actors, {lit} with baked lighting ({stale} stale)");
    assert!(actors > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_texture() {
    let Some(paths) = install_packages() else { return };

    let mut failures = Vec::new();
    let mut textures = 0;
    let loader = script_loader();
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let mut objects = Objects::new(&loader);
        let is_texture =
            |e: &&Export| matches!(pkg.object_name(e.class_index), Some("Texture" | "Cubemap" | "MovieTexture" | "WetTexture"));
        for (index, export) in pkg.exports.iter().enumerate().filter(|(_, e)| is_texture(e)) {
            textures += 1;
            let name = pkg.name(export.object_name);
            let texture = match read_texture(&mut objects, Source { package: &pkg, data: &data }, index) {
                Ok(texture) => texture,
                Err(e) => {
                    failures.push(format!("{}: {name}: {e}", path.display()));
                    continue;
                }
            };
            for (i, mip) in texture.mips.iter().enumerate() {
                let expected = mip_size(texture.format, mip.width, mip.height);
                if !mip.data.is_empty() && expected.is_some_and(|n| n != mip.data.len()) {
                    failures.push(format!(
                        "{}: {name} mip {i}: {:?} {}x{} has {} bytes, expected {expected:?}",
                        path.display(),
                        texture.format,
                        mip.width,
                        mip.height,
                        mip.data.len()
                    ));
                }
            }
            if let Some(palette) = texture.palette.filter(|&p| p > 0) {
                let export = &pkg.exports[palette as usize - 1];
                match read_palette(&pkg, &data, export) {
                    Ok(colors) if colors.len() == 256 => {}
                    Ok(colors) => failures.push(format!("{name}: palette has {} colors", colors.len())),
                    Err(e) => failures.push(format!("{}: {name} palette: {e}", path.display())),
                }
            }
        }
    }
    assert!(textures > 0);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_sound() {
    let Some(dir) = game_dir() else { return };
    let mut paths = Vec::new();
    let mut dirs = vec![dir];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("uax")) {
                paths.push(path);
            }
        }
    }
    let mut failures = Vec::new();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    let mut magic: BTreeMap<String, usize> = BTreeMap::new();
    let mut longest = 0.0_f32;
    for path in &paths {
        let data = std::fs::read(path).unwrap();
        let pkg = Package::parse(&data).unwrap();
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("Sound")) {
            match read_sound(&pkg, &data, export) {
                Ok(sound) => {
                    *types.entry(sound.file_type.clone()).or_insert(0) += 1;
                    let head = String::from_utf8_lossy(sound.data.get(..4).unwrap_or_default()).into_owned();
                    *magic.entry(head).or_insert(0) += 1;
                    match sound.duration() {
                        Some(seconds) if seconds > 0.0 && seconds < 3600.0 => longest = longest.max(seconds),
                        other => failures.push(format!("{}: {}: duration {other:?}", path.display(), pkg.name(export.object_name))),
                    }
                }
                Err(e) => failures.push(format!("{}: {}: {e}", path.display(), pkg.name(export.object_name))),
            }
        }
    }
    eprintln!("sounds by type {types:?}, by file magic {magic:?}; longest {longest} s");
    assert!(!types.is_empty());
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn every_font() {
    let Some(dir) = game_dir() else { return };
    let mut paths = Vec::new();
    for sub in ["System", "Textures"] {
        for entry in std::fs::read_dir(dir.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u") || e.eq_ignore_ascii_case("utx")) {
                paths.push(path);
            }
        }
    }
    let (mut fonts, mut failures) = (0, Vec::new());
    for path in paths {
        let data = std::fs::read(&path).unwrap();
        let Ok(pkg) = Package::parse(&data) else { continue };
        for export in pkg.exports.iter().filter(|e| pkg.object_name(e.class_index) == Some("Font")) {
            match p2_assets::read_font(&pkg, &data, export) {
                Ok(font) => {
                    fonts += 1;
                    let a = font.glyph('A');
                    if a.is_none_or(|g| usize::from(g.texture) >= font.textures.len()) {
                        failures.push(format!("{}: {}: no 'A' glyph", path.display(), pkg.name(export.object_name)));
                    }
                }
                Err(e) => failures.push(format!("{}: {}: {e}", path.display(), pkg.name(export.object_name))),
            }
        }
    }
    assert!(fonts > 0);
    assert!(failures.is_empty(), "{}", failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}
