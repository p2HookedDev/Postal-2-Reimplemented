mod actor_natives;
mod actors;
mod bolton;
mod bullet_hole;
mod bones;
mod ai_move;
mod ai_natives;
mod canvas;
mod client;
mod collision;
mod config;
mod damage;
mod enums;
mod anim;
mod game_natives;
mod hash;
mod input;
pub mod interp;
mod matinee;
mod karma;
mod limbs;
mod memory;
mod mover;
mod movement_natives;
mod native_id;
mod operators;
mod natives;
mod object_store;
mod nav;
mod pawn_anim;
mod physics;
mod projectile;
pub mod projector;
mod props;
mod ragdoll;
mod save;
mod script_hooks;
mod senses;
mod side_packages;
mod find_spot;
mod states;
mod travel;
mod util_natives;
mod volumes;
pub use volumes::{warp_clip_plane, warp_view};
mod profiling;
mod world;
mod class_load;
mod host_api;
mod objects;

use std::ops::Deref;
use std::sync::Arc;

use p2_package::flags::{RF_LOAD_FOR_ANY, RF_LOAD_FOR_GAME};
use p2_package::{Loader, Package, PackageId};
use rustc_hash::{FxHashMap, FxHashSet};
pub use actor_natives::{ActorFx, BoneControl, BoneDirection, FluidPling, MovieState, NEAR_CLIPPING_PLANE};
pub use canvas::CanvasItem;
pub use enums::ERenderStyle;
pub use input::PlayerView;
pub use config::{RENDER_DEVICE, VIEWPORT_MANAGER, WindowRequest, ini_truthy};
use p2_script::{Layouts, StructLayout, Type};

pub use anim::AnimChannel;
pub use bones::MeshAnimations;
pub use game_natives::ParticleCommand;
pub use natives::AUDIO_DEFAULT_RADIUS;
pub use matinee::{ScreenOverlay, TitleOverlay};
pub use save::{SaveError, SaveGame, save_file_name, save_map};
pub use actors::Encroachment;
pub use client::{InteractionOrder, VideoMode};
pub use travel::{ETravelType, TravelUrl};
pub use props::{ClassName, Kind, Prop, PropertyName, Slot};
pub use profiling::{FunctionProfiling, FunctionTime};
pub use memory::Mem as PropertyBlock;
pub use object_store::ObjectStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Script(#[from] p2_script::Error),
    #[error(transparent)]
    Package(#[from] p2_package::Error),
    #[error("package {0} is not loaded")]
    PackageNotLoaded(String),
    #[error("class {0} not found")]
    ClassNotFound(String),
    #[error("export {0} is missing")]
    BadExport(usize),
    #[error("not supported yet: {0}")]
    Unsupported(String),
    #[error("native function {0} is not implemented")]
    UnknownNative(String),
    #[error("assertion failed in {function}, line {line}")]
    AssertFailed { function: String, line: u16 },
    #[error("jump to {0:#x} is not a statement")]
    BadJump(u16),
    #[error("not supported yet: Infinite script recursion ({calls} calls) detected in {}", .chain.join(" <- "))]
    Recursion { calls: u32, chain: Vec<String> },
}

/// Equal names, ignoring ASCII case, share one id. The default is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NameId(u32);

impl NameId {
    pub const NONE: Self = Self(0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(u32);

impl ObjectId {
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassId(u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Object(Option<ObjectId>),
    Name(NameId),
    Str(String),
    Array(Vec<Self>),
    /// Members in struct layout order (every struct but `Vector` and `Rotator`).
    Struct(Vec<Self>),
    Vector(p2_math::Vector),
    Rotator(p2_math::Rotator),
    Delegate { object: Option<ObjectId>, function: NameId },
    /// No value: a map property (never serialized, not used by script), and, inside the VM, an omitted argument, a
    /// parameter that is not `out`, an iterator's step that leaves the loop variables.
    Map,
}

impl Value {
    pub(crate) fn of_struct(layout: &StructLayout, members: Vec<Self>) -> Self {
        let float = |i: usize| members.get(i).map_or(0.0, interp::float);
        let int = |i: usize| members.get(i).map_or(0, interp::int);
        match memory::StructKind::of(layout) {
            memory::StructKind::Vector => Self::Vector(p2_math::Vector::new(float(0), float(1), float(2))),
            memory::StructKind::Rotator => Self::Rotator(p2_math::Rotator::new(int(0), int(1), int(2))),
            memory::StructKind::Other => Self::Struct(members),
        }
    }
}

/// An object. Objects that script data references but the VM has not loaded or created (textures, classes,
/// sounds, ...) are stubs: no `class`, no properties.
#[derive(Debug, Clone)]
pub struct Object {
    pub name: NameId,
    pub class_name: NameId,
    pub path: String,
    pub class: Option<ClassId>,
    pub(crate) mem: memory::Mem,
}

#[derive(Debug)]
pub struct Class {
    pub name: NameId,
    pub package: PackageId,
    pub export: usize,
    pub super_class: Option<ClassId>,
    pub layout: Arc<StructLayout>,
    pub(crate) mem: memory::LayoutId,
    pub(crate) defaults: memory::Mem,
    index: FxHashMap<String, usize>,
    lower_index: std::sync::OnceLock<FxHashMap<String, usize>>,
    resolved: props::Resolved,
    class_object: Option<ObjectId>,
    /// `Object`'s native `Name` and `Class` properties in the layout: the last of their name.
    name_slot: Option<usize>,
    class_slot: Option<usize>,
}

impl Class {
    pub(crate) fn is_actor(&self) -> bool {
        self.resolved.kinds() & (1 << Kind::Actor as u32) != 0
    }
}

pub trait LoaderRef: Deref<Target = Loader> + Clone + Send + Sync {
    /// This loader with package `name` and its imports added; existing ids stay valid. `None` where it cannot grow.
    fn with_package(&self, name: &str) -> Option<Result<Self, p2_package::ResolveError>>;
}

impl LoaderRef for Arc<Loader> {
    fn with_package(&self, name: &str) -> Option<Result<Self, p2_package::ResolveError>> {
        let mut loader = Loader::clone(self);
        Some(loader.load(name).and_then(|id| loader.load_imports(id)).map(|()| Self::new(loader)))
    }
}

impl LoaderRef for &Loader {
    fn with_package(&self, _: &str) -> Option<Result<Self, p2_package::ResolveError>> {
        None
    }
}

#[derive(Default)]
pub struct Diagnostics {
    pub log: Vec<String>,
    pub missing_natives: std::collections::BTreeMap<String, usize>,
    pub trace: Option<Vec<String>>,
    pub tick_profile: Option<FxHashMap<String, std::time::Duration>>,
    pub count_work: bool,
    pub(crate) function_profile: Option<profiling::FunctionProfile>,
}

#[derive(Default)]
pub struct VmOptions {
    pub player_physics: bool,
    pub data_dir: Option<std::path::PathBuf>,
    pub steam_apps: Vec<u32>,
}

#[derive(Default)]
pub(crate) struct Media {
    pub(crate) anims: anim::Anims,
    pub(crate) bone_data: bones::BoneData,
    pub(crate) canvas: canvas::Canvas,
    pub(crate) subtitle_sounds: util_natives::SubtitleSounds,
    pub(crate) texture_sizes: FxHashMap<ObjectId, [i32; 2]>,
    pub(crate) sound_durations: FxHashMap<ObjectId, Option<f32>>,
}

#[derive(Default)]
pub(crate) struct HostIo {
    pub(crate) commands: Vec<HostCommand>,
    pub(crate) input: input::Input,
    pub(crate) logs: FxHashMap<ObjectId, std::path::PathBuf>,
    pub(crate) client: client::Client,
    pub(crate) configs: config::Configs,
    pub(crate) fullscreen: Option<bool>,
    pub(crate) music_handle: i32,
    pub(crate) save_dir: Option<std::path::PathBuf>,
    pub(crate) saved_slots: FxHashSet<i32>,
}

#[derive(Default)]
pub(crate) struct ScriptRuntime {
    pub(crate) frame_pool: Vec<memory::Mem>,
    pub(crate) arg_pool: Vec<Vec<Value>>,
    pub(crate) functions: FxHashMap<(PackageId, usize), Arc<interp::Function>>,
    pub(crate) lookups: Lookups,
    pub(crate) children: FxHashMap<(PackageId, usize), FxHashMap<(String, bool), (PackageId, usize)>>,
    pub(crate) script_depth: u32,
    pub(crate) in_singular: FxHashSet<ObjectId>,
    pub(crate) slot_lists: std::sync::Mutex<FxHashMap<(ClassId, &'static str), props::SlotList>>,
    pub(crate) states: states::States,
    pub(crate) latent: states::Latent,
}

/// `loader` must have the script packages loaded ([`Loader::load_scripts`]).
pub struct Vm<L> {
    loader: L,
    layouts: Layouts<L>,
    nav_graph: Option<nav::Graph>,
    hash: hash::CollisionHash,
    level: Option<ObjectId>,
    rng: p2_math::Rng,
    travel_items: bool,
    travelling: Vec<travel::TravelActor>,
    map_name: String,
    url_options: Vec<String>,
    pub actor_fx: ActorFx,
    side_packages: FxHashMap<String, Option<Arc<side_packages::SidePackage>>>,
    world: world::World,
    karma: karma::Karma,
    compiled: Vec<Option<Arc<interp::compile::Code<L>>>>,
    pub diagnostics: Diagnostics,
    pub options: VmOptions,
    media: Media,
    host: HostIo,
    pub store: ObjectStore,
    script: ScriptRuntime,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostCommand {
    Sound(PlayedSound),
    Music(MusicCommand),
    Projector(ProjectorEvent),
    Particle(ParticleCommand),
    Travel(String),
    Window(WindowRequest),
    Quit,
    SetMouse([i32; 2]),
    SaveGame { slot: i32, save: Box<SaveGame> },
    OpenUrl(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectorEvent {
    Attach(ObjectId),
    Detach(ObjectId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MusicCommand {
    Play {
        handle: i32,
        song: String,
        actor: Option<ObjectId>,
        fade_in: f32,
        volume: f32,
        radius: f32,
        pitch: f32,
    },
    Stop { handle: i32, fade_out: f32 },
    StopAll { fade_out: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayedSound {
    pub actor: Option<ObjectId>,
    pub sound: String,
    pub slot: u8,
    pub volume: f32,
    pub radius: f32,
    pub pitch: f32,
    pub attenuate: bool,
    pub no_override: bool,
    pub flashbang: Option<f32>,
}

#[derive(Default)]
pub(crate) struct Lookups {
    pub(crate) members: FxHashMap<(ClassId, PackageId, i32), Option<usize>>,
    pub(crate) names: FxHashMap<(PackageId, usize), NameId>,
    pub(crate) object_consts: FxHashMap<(PackageId, i32), ObjectId>,
    pub(crate) class_objects: FxHashMap<ObjectId, Option<ClassId>>,
    pub(crate) hole_splats: FxHashMap<u32, Option<ClassId>>,
    pub(crate) casts: FxHashMap<(ClassId, PackageId, i32, bool), bool>,
    /// Ancestry is then tested by `NameId`: interning is case-insensitive, so this is `UnrealScript`'s by-name test without
    /// strings, and stays right while classes load lazily.
    pub(crate) cast_targets: FxHashMap<(PackageId, i32), NameId>,
    pub(crate) class_tests: FxHashMap<NameId, FxHashMap<ClassId, bool>>,
    pub(crate) functions: FxHashMap<(ClassId, Option<(PackageId, usize)>, NameId), Option<Arc<interp::Function>>>,
    pub(crate) function_table: Vec<Arc<interp::Function>>,
    pub(crate) state_keys: FxHashMap<(PackageId, usize), u32>,
    pub(crate) super_states: FxHashMap<(PackageId, usize), Option<(PackageId, usize)>>,
    pub(crate) class_states: FxHashMap<(ClassId, NameId), Option<(PackageId, usize)>>,
    pub(crate) auto_states: FxHashMap<ClassId, Option<((PackageId, usize), NameId)>>,
    pub(crate) event_names: FxHashMap<String, (NameId, Option<u32>)>,
    pub(crate) events: Vec<(NameId, Option<u32>)>,
    pub(crate) native_declarations: FxHashMap<u16, (PackageId, usize)>,
    pub(crate) stub_calls: FxHashMap<(NameId, PackageId, usize), String>,
    pub(crate) tag_indices: FxHashMap<memory::LayoutId, Arc<FxHashMap<String, usize>>>,
    pub(crate) class_names: ClassNames,
}

#[derive(Default)]
pub(crate) struct ClassNames {
    pub(crate) object: NameId,
    pub(crate) actor: NameId,
    pub(crate) anim_notify_scripted: NameId,
    pub(crate) anim_notify_script: NameId,
    pub(crate) anim_notify_sound: NameId,
    pub(crate) anim_notify_destroy_effect: NameId,
    pub(crate) anim_notify_effect: NameId,
    pub(crate) particle_emitter: NameId,
    pub(crate) fluid_surface_info: NameId,
    pub(crate) canvas: NameId,
    pub(crate) hud: NameId,
    pub(crate) limb: NameId,
    pub(crate) bolton_part: NameId,
    pub(crate) path_node: NameId,
    pub(crate) home_node: NameId,
    pub(crate) class: NameId,
    pub(crate) auto: NameId,
    pub(crate) begin: NameId,
}

impl ClassNames {
    fn new(mut intern: impl FnMut(&str) -> NameId) -> Self {
        Self {
            object: intern("Object"),
            actor: intern("Actor"),
            anim_notify_scripted: intern("AnimNotify_Scripted"),
            anim_notify_script: intern("AnimNotify_Script"),
            anim_notify_sound: intern("AnimNotify_Sound"),
            anim_notify_destroy_effect: intern("AnimNotify_DestroyEffect"),
            anim_notify_effect: intern("AnimNotify_Effect"),
            particle_emitter: intern("ParticleEmitter"),
            fluid_surface_info: intern("FluidSurfaceInfo"),
            canvas: intern("Canvas"),
            hud: intern("HUD"),
            limb: intern("Limb"),
            bolton_part: intern("BoltonPart"),
            path_node: intern("PathNode"),
            home_node: intern("HomeNode"),
            class: intern("Class"),
            auto: intern("Auto"),
            begin: intern("Begin"),
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn new(loader: L) -> Self {
        let mut vm = Self {
            layouts: Layouts::new(loader.clone()),
            loader,
            travel_items: false,
            travelling: Vec::new(),
            map_name: String::new(),
            url_options: Vec::new(),
            nav_graph: None,
            hash: hash::CollisionHash::default(),
            level: None,
            rng: p2_math::Rng::new(1),
            actor_fx: ActorFx::default(),
            world: world::World::default(),
            karma: karma::Karma::default(),
            compiled: Vec::new(),
            side_packages: FxHashMap::default(),
            diagnostics: Diagnostics::default(),
            options: VmOptions::default(),
            media: Media::default(),
            host: HostIo::default(),
            store: ObjectStore::default(),
            script: ScriptRuntime::default(),
        };
        vm.intern("None");
        let class_names = ClassNames::new(|name| vm.intern(name));
        vm.script.lookups.class_names = class_names;
        let events = states::EVENT_NAMES.iter().map(|name| (vm.intern(name), states::probe_bit(name))).collect();
        vm.script.lookups.events = events;
        vm
    }

    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    pub fn take_commands(&mut self, mut pick: impl FnMut(&HostCommand) -> bool) -> impl Iterator<Item = HostCommand> {
        self.host.commands.extract_if(.., move |command| pick(command))
    }

    /// `rand()` / 32767, in `0..=1` (deterministic per VM).
    pub fn random(&mut self) -> f32 {
        self.rng.frand()
    }

    pub(crate) fn rand(&mut self) -> i32 {
        self.rng.rand()
    }

    pub fn intern(&mut self, name: &str) -> NameId {
        let mut lower = [0u8; 64];
        let found = match lower.get_mut(..name.len()) {
            Some(lower) => {
                lower.copy_from_slice(name.as_bytes());
                lower.make_ascii_lowercase();
                std::str::from_utf8(lower).ok().and_then(|lower| self.store.name_ids.get(lower))
            }
            None => self.store.name_ids.get(&name.to_ascii_lowercase()),
        };
        if let Some(&id) = found {
            return id;
        }
        let id = NameId(self.store.names.len() as u32);
        self.store.names.push(name.to_owned());
        self.store.name_ids.insert(name.to_ascii_lowercase(), id);
        id
    }

    pub fn actors(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.store.actor_ids.iter().copied()
    }

    pub fn object_ids(&self) -> impl Iterator<Item = ObjectId> + use<L> {
        (0..self.store.objects.len() as u32).map(ObjectId)
    }
}

const MAX_CLASS_DEPTH: u32 = 256;

/// `RF_LoadForEdit` without `RF_LoadForClient` / `RF_LoadForServer` (the editor's viewport `Camera`s saved in maps): the
/// game's linker does not create it, and references to it are None.
fn editor_only(pkg: &Package, export: usize) -> bool {
    pkg.exports.get(export).is_some_and(|e| e.object_flags & RF_LOAD_FOR_ANY != 0 && e.object_flags & RF_LOAD_FOR_GAME == 0)
}

fn lowercase_index(layout: &StructLayout) -> FxHashMap<String, usize> {
    let mut index = FxHashMap::default();
    for (i, field) in layout.properties.iter().enumerate() {
        index.entry(field.name.to_ascii_lowercase()).or_insert(i);
    }
    index
}

/// Avoids allocating for names up to 64 bytes.
fn lower_lookup(index: &FxHashMap<String, usize>, name: &str) -> Option<usize> {
    let mut lower = [0u8; 64];
    match lower.get_mut(..name.len()) {
        Some(lower) => {
            lower.copy_from_slice(name.as_bytes());
            lower.make_ascii_lowercase();
            index.get(std::str::from_utf8(lower).ok()?).copied()
        }
        None => index.get(&name.to_ascii_lowercase()).copied(),
    }
}

fn class_export_index<'a>(exports: impl Iterator<Item = (i32, &'a str)>) -> FxHashMap<String, usize> {
    let mut index = FxHashMap::default();
    for (i, (class_index, name)) in exports.enumerate() {
        if class_index == 0 {
            index.entry(name.to_ascii_lowercase()).or_insert(i);
        }
    }
    index
}

fn zero_slot(ty: &Type, array_dim: u32) -> Value {
    if array_dim > 1 {
        return Value::Array((0..array_dim).map(|_| zero(ty)).collect());
    }
    zero(ty)
}

fn zero(ty: &Type) -> Value {
    match ty {
        Type::Byte => Value::Byte(0),
        Type::Int => Value::Int(0),
        Type::Bool => Value::Bool(false),
        Type::Float => Value::Float(0.0),
        Type::Object | Type::Class => Value::Object(None),
        Type::Name => Value::Name(NameId::NONE),
        Type::Delegate => Value::Delegate { object: None, function: NameId::NONE },
        Type::Str | Type::String => Value::Str(String::new()),
        Type::Array(_) => Value::Array(Vec::new()),
        Type::FixedArray(inner, count) => Value::Array((0..*count).map(|_| zero(inner)).collect()),
        Type::Struct(layout) => Value::of_struct(layout, layout.properties.iter().map(|f| zero_slot(&f.ty, f.array_dim)).collect()),
        Type::Map => Value::Map,
    }
}

fn set_element(slot: &mut Value, ty: &Type, array_dim: u32, index: u32, value: Value) {
    let index = index as usize;
    match slot {
        Value::Array(elements) if array_dim > 1 => {
            if let Some(element) = elements.get_mut(index) {
                *element = value;
            }
        }
        Value::Array(elements) if !matches!(value, Value::Array(_)) => {
            if index >= memory::MAX_TAGGED_ELEMENTS {
                return;
            }
            if elements.len() <= index {
                let element = if let Type::Array(inner) = ty { zero(inner) } else { Value::Map };
                elements.resize(index + 1, element);
            }
            elements[index] = value;
        }
        _ => *slot = value,
    }
}

#[cfg(test)]
mod class_export_index_tests {
    use super::class_export_index;

    #[test]
    fn finds_classes_by_any_case_first_wins() {
        let exports = [(-3, "Shotgun"), (0, "ShotgunWeapon"), (0, "Shotgun"), (0, "SHOTGUN"), (5, "Default__x")];
        let index = class_export_index(exports.into_iter());
        assert_eq!(index.get("shotgun"), Some(&2));
        assert_eq!(index.get("shotgunweapon"), Some(&1));
        assert_eq!(index.get("default__x"), None);
        assert_eq!(index.len(), 2);
    }
}
