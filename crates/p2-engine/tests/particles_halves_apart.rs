use std::path::{Path, PathBuf};

const DRAWING: [&str; 5] = ["../../p2-render/src/effects/particle_frame.rs", "../../p2-render/src/effects/particle_draw.rs", "../../p2-render/src/effects/particle_draw", "../../p2-render/src/effects/fire_volume.rs", "../../p2-render/src/post/reflections.rs"];
const SIMULATION: [&str; 2] = ["map/particles.rs", "map/particles"];
const SIMULATION_NAMES: [&str; 1] = ["crate::map"];
const DRAWING_NAMES: [&str; 6] = ["ParticleMaterial", "ShaderBuffer", "RECORD_WORDS", "sprite_light", "lamp_sum", "DynamicLightList"];

fn rust_files(path: &Path, found: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            rust_files(&entry.unwrap().path(), found);
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        found.push(path.to_owned());
    }
}

fn naming(paths: &[&str], names: &[&str]) -> Vec<String> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for path in paths {
        let path = source.join(path);
        assert!(path.exists(), "{} is gone", path.display());
        rust_files(&path, &mut files);
    }
    let mut named = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        for (index, line) in text.lines().enumerate() {
            if names.iter().any(|name| line.contains(name)) {
                named.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    }
    named
}

#[test]
fn particle_drawing_never_names_the_simulation() {
    let named = naming(&DRAWING, &SIMULATION_NAMES);
    assert!(named.is_empty(), "particle drawing names the map:\n{}", named.join("\n"));
}

#[test]
fn the_particle_simulation_never_names_the_drawing() {
    let named = naming(&SIMULATION, &DRAWING_NAMES);
    assert!(named.is_empty(), "the particle simulation names the drawing:\n{}", named.join("\n"));
}
