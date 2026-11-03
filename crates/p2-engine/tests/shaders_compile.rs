//! The engine's WESL shaders composed as `bevy_shader` 0.20 composes them at runtime, then validated by naga for the
//! game's feature sets. Bindless sets skip naga's binding-uniqueness check only: Bevy declares a bindless material's
//! arrays from its `AsBindGroup` layout at runtime, which this composer does not. Skips when Bevy's sources are not in
//! the registry.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use wesl::syntax::{ModulePath, PathOrigin};
use wesl::{CompileOptions, Feature};

const BEVY: &str = "0.20.0";

const ENTRIES: [(&str, &[&str]); 8] = [
    ("actor_light", &[]),
    ("lightmapped", &[]),
    ("lightmapped", &["LIGHTMAP"]),
    ("mesh_light", &[]),
    ("terrain", &[]),
    ("terrain_layer", &["TERRAIN_LAYER"]),
    ("blended", &[]),
    ("particle", &[]),
];

const CONSTANTS: &str = "const MATERIAL_BIND_GROUP = 3;
const MAX_CASCADES_PER_LIGHT = 4;
const MAX_DIRECTIONAL_LIGHTS = 10;
const MAX_RECT_LIGHTS = 8;
const PER_OBJECT_BUFFER_BATCH_SIZE = 128;
const SAMPLES_PER_SLICE_SIDE = 3;
const SLICE_COUNT = 4;
const SORTED_FRAGMENT_MAX_COUNT = 8;
const SCREEN_SPACE_SPECULAR_TRANSMISSION_BLUR_TAPS = 8;
const TONEMAPPING_LUT_TEXTURE_BINDING_INDEX = 20;
const TONEMAPPING_LUT_SAMPLER_BINDING_INDEX = 21;
const AVAILABLE_STORAGE_BUFFER_BINDINGS = 8;
";

fn registry() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").map(|home| Path::new(&home).join(".cargo")))?;
    let sources = std::fs::read_dir(home.join("registry").join("src")).ok()?;
    sources.flatten().map(|entry| entry.path()).find(|index| index.join(format!("bevy_pbr-{BEVY}")).is_dir())
}

struct Sources {
    registry: PathBuf,
    ours: PathBuf,
}

impl wesl::Resolver for Sources {
    fn resolve_source<'a>(&'a self, path: &ModulePath) -> Result<Cow<'a, str>, wesl::error::ResolveError> {
        let missing = |why: &str| wesl::error::ResolveError::ModuleNotFound(path.clone(), why.to_string());
        let file = match &path.origin {
            PathOrigin::Package(package) => {
                let package = package.rsplit('/').next().unwrap_or(package);
                if package == "constants" && path.components.is_empty() {
                    return Ok(Cow::Borrowed(CONSTANTS));
                }
                self.registry.join(format!("{package}-{BEVY}")).join("src").join(path.components.join("/"))
            }
            PathOrigin::Absolute => match path.components.split_first() {
                Some((first, rest)) if first == "p2_engine" => self.ours.join("render").join(rest.join("/")),
                _ => return Err(missing("not ours")),
            },
            _ => return Err(missing("relative path")),
        };
        std::fs::read_to_string(file.with_extension("wesl")).map(Cow::Owned).map_err(|error| missing(&error.to_string()))
    }
}

fn options(extra: &[&str], entry: &[&str]) -> CompileOptions {
    let mut options = CompileOptions { imports: true, condcomp: true, visibility: false, ..Default::default() };
    let base = [
        "VERTEX_POSITIONS",
        "VERTEX_NORMALS",
        "VERTEX_UVS",
        "VERTEX_UVS_A",
        "VERTEX_UVS_B",
        "VERTEX_COLORS",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
        "PER_OBJECT_BUFFER_BATCH_SIZE",
        "MAX_CASCADES_PER_LIGHT",
        "MAX_DIRECTIONAL_LIGHTS",
        "MAX_RECT_LIGHTS",
        "AVAILABLE_STORAGE_BUFFER_BINDINGS",
        "CLUSTERED_FORWARD_DEBUG_Z_SLICES",
        "SIXTEEN_BYTE_ALIGNMENT",
        "DISTANCE_FOG",
    ];
    for name in base.iter().chain(extra).chain(entry) {
        options.features.flags.insert((*name).to_string(), Feature::Enable);
    }
    options
}

#[test]
fn engine_shaders_compose_and_validate() {
    let Some(registry) = registry() else {
        eprintln!("skipped: bevy_pbr-{BEVY} not in the cargo registry");
        return;
    };
    let sources = Sources { registry, ours: Path::new(env!("CARGO_MANIFEST_DIR")).join("src") };
    let feature_sets: [&[&str]; 7] = [
        &[],
        &["BINDLESS"],
        &["LIGHTMAP"],
        &["BINDLESS", "LIGHTMAP", "MULTIPLE_LIGHTMAPS_IN_ARRAY"],
        &["DEPTH_PREPASS", "NORMAL_PREPASS", "SCREEN_SPACE_AMBIENT_OCCLUSION", "CONTACT_SHADOWS"],
        &["BINDLESS", "DEPTH_PREPASS", "NORMAL_PREPASS", "SCREEN_SPACE_AMBIENT_OCCLUSION", "CONTACT_SHADOWS"],
        &["CLUSTERED_DECALS_ARE_USABLE"],
    ];
    let mut failures = Vec::new();
    for (entry, defs) in ENTRIES {
        for features in feature_sets {
            let mut components = vec!["p2_engine".to_string()];
            components.extend(entry.split('/').map(str::to_string));
            let main = ModulePath { origin: PathOrigin::Absolute, components };
            let wgsl = match wesl::compile(&main, &options(features, defs), &sources) {
                Ok(compiled) => compiled.to_string(),
                Err(error) => {
                    failures.push(format!("{entry} {features:?}: compose: {}", error.diagnostic().render_plain()));
                    continue;
                }
            };
            let module = match naga::front::wgsl::parse_str(&wgsl) {
                Ok(module) => module,
                Err(error) => {
                    failures.push(format!("{entry} {features:?}: parse: {}", error.emit_to_string(&wgsl)));
                    continue;
                }
            };
            let mut flags = naga::valid::ValidationFlags::all();
            if features.contains(&"BINDLESS") {
                flags.remove(naga::valid::ValidationFlags::BINDINGS);
            }
            let mut validator = naga::valid::Validator::new(flags, naga::valid::Capabilities::all());
            if let Err(error) = validator.validate(&module) {
                failures.push(format!("{entry} {features:?}: validate: {}", error.emit_to_string(&wgsl)));
            }
        }
    }
    assert!(failures.is_empty(), "{} shader failures:\n{}", failures.len(), failures.join("\n\n"));
}
