mod actor_natives;
mod actors;
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
mod nav;
mod pawn_anim;
mod physics;
mod projectile;
pub mod projector;
mod props;
mod ragdoll;
mod save;
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

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use p2_package::{Loader, Package, PackageId, Reader};
use rustc_hash::{FxHashMap, FxHashSet};
pub use actor_natives::{ActorFx, BoneControl, BoneDirection, FluidPling, MovieState};
pub use canvas::CanvasItem;
pub use input::PlayerView;
pub use config::{RENDER_DEVICE, VIEWPORT_MANAGER, WindowRequest, ini_truthy};
use p2_script::{Layouts, Objects, ScriptObject, Source, StructLayout, Type, read_export, read_properties};

pub use anim::AnimChannel;
pub use bones::MeshAnimations;
pub use game_natives::ParticleCommand;
pub use natives::AUDIO_DEFAULT_RADIUS;
pub use matinee::{ScreenOverlay, TitleOverlay};
pub use save::{SaveError, SaveGame, save_file_name, save_map};
pub use client::VideoMode;
pub use travel::{ETravelType, TravelUrl};
pub use props::{ClassName, Kind, Prop, PropertyName, Slot};
pub use profiling::FunctionTime;
pub use memory::Mem as PropertyBlock;

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

pub trait LoaderRef: Deref<Target = Loader> + Clone + Send + Sync {}

impl<L: Deref<Target = Loader> + Clone + Send + Sync> LoaderRef for L {}

#[derive(Default)]
pub struct Diagnostics {
    pub log: Vec<String>,
    pub missing_natives: std::collections::BTreeMap<String, usize>,
    pub trace: Option<Vec<String>>,
    pub tick_profile: Option<HashMap<String, std::time::Duration>>,
    pub count_work: bool,
    pub(crate) work: profiling::Counters,
    pub(crate) function_profile: Option<profiling::FunctionProfile>,
}

#[derive(Default)]
pub struct VmOptions {
    pub player_physics: bool,
    pub data_dir: Option<std::path::PathBuf>,
}

#[derive(Default)]
pub(crate) struct Media {
    pub(crate) anims: anim::Anims,
    pub(crate) bone_data: bones::BoneData,
    pub(crate) canvas: Option<ObjectId>,
    pub(crate) canvas_items: Vec<CanvasItem>,
    pub(crate) canvas_paths: FxHashMap<ObjectId, Arc<str>>,
    pub(crate) subtitle_sounds: util_natives::SubtitleSounds,
    pub(crate) fonts: FxHashMap<ObjectId, Option<Arc<p2_assets::Font>>>,
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
    pub(crate) saved_slots: HashSet<i32>,
}

#[derive(Default)]
pub(crate) struct ObjectStore {
    pub(crate) names: Vec<String>,
    pub(crate) name_ids: FxHashMap<String, NameId>,
    pub(crate) objects: Vec<Object>,
    pub(crate) object_ids: FxHashMap<String, ObjectId>,
    pub(crate) actor_ids: std::collections::BTreeSet<ObjectId>,
    pub(crate) dynamic_ids: actors::DynamicActors,
    pub(crate) classes: Vec<Class>,
    pub(crate) class_ids: FxHashMap<(PackageId, usize), ClassId>,
    pub(crate) class_by_name: FxHashMap<NameId, ClassId>,
    pub(crate) class_exports: FxHashMap<PackageId, FxHashMap<String, usize>>,
    pub(crate) name_counters: FxHashMap<ClassId, u32>,
    pub(crate) class_redirects: Vec<(ClassId, ClassId)>,
}

#[derive(Default)]
pub(crate) struct ScriptRuntime {
    pub(crate) memory: memory::MemLayouts,
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
    side_packages: HashMap<String, Option<Arc<side_packages::SidePackage>>>,
    world: world::World,
    karma: karma::Karma,
    compiled: Vec<Option<Arc<interp::compile::Code<L>>>>,
    pub diagnostics: Diagnostics,
    pub options: VmOptions,
    media: Media,
    host: HostIo,
    store: ObjectStore,
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
            side_packages: HashMap::new(),
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

    pub fn name(&self, id: NameId) -> &str {
        &self.store.names[id.0 as usize]
    }

    pub fn actors(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.store.actor_ids.iter().copied()
    }

    pub fn object_ids(&self) -> impl Iterator<Item = ObjectId> + use<L> {
        (0..self.store.objects.len() as u32).map(ObjectId)
    }

    pub fn object(&self, id: ObjectId) -> &Object {
        &self.store.objects[id.0 as usize]
    }

    pub fn class(&self, id: ClassId) -> &Class {
        &self.store.classes[id.0 as usize]
    }

    pub fn find_class(&mut self, package: &str, name: &str) -> Result<ClassId, Error> {
        let id = self.loader.loaded_id(package).ok_or_else(|| Error::PackageNotLoaded(package.to_owned()))?;
        if !self.store.class_exports.contains_key(&id) {
            let phase = self.profile_start();
            let pkg = &self.loader.package(id).package;
            let index = class_export_index(pkg.exports.iter().map(|e| (e.class_index, pkg.name(e.object_name))));
            self.store.class_exports.insert(id, index);
            self.profile_phase("load: class index", phase);
        }
        let export = self
            .store
            .class_exports
            .get(&id)
            .and_then(|index| index.get(&name.to_ascii_lowercase()).copied())
            .ok_or_else(|| Error::ClassNotFound(format!("{package}.{name}")))?;
        self.load_class(id, export)
    }

    pub fn redirect_class(&mut self, from: (&str, &str), to: (&str, &str)) -> bool {
        let Ok(to) = self.find_class(to.0, to.1) else { return false };
        let Ok(from) = self.find_class(from.0, from.1) else { return false };
        self.store.class_redirects.push((from, to));
        true
    }

    fn redirected(&self, class: ClassId) -> ClassId {
        self.store.class_redirects.iter().find(|(from, _)| *from == class).map_or(class, |&(_, to)| to)
    }

    pub fn property_index(&self, class: ClassId, name: &str) -> Option<usize> {
        self.count_property(name);
        let class = self.class(class);
        class.index.get(name).copied().or_else(|| {
            self.count(profiling::Work::PropertyScan);
            lower_lookup(class.lower_index.get_or_init(|| lowercase_index(&class.layout)), name)
        })
    }

    pub fn new_object(&mut self, class: ClassId, name: Option<&str>) -> ObjectId {
        let (name, key) = match name {
            Some(name) => (name.to_owned(), format!("transient.{name}").to_ascii_lowercase()),
            None => loop {
                let counter = self.store.name_counters.entry(class).or_insert(0);
                let candidate = format!("{}{counter}", self.store.names[self.store.classes[class.0 as usize].name.0 as usize]);
                *counter += 1;
                let key = format!("transient.{candidate}").to_ascii_lowercase();
                if !self.store.object_ids.contains_key(&key) {
                    break (candidate, key);
                }
            },
        };
        let object = Object {
            name: self.intern(&name),
            class_name: self.class(class).name,
            path: format!("Transient.{name}"),
            class: Some(class),
            mem: self.class(class).defaults.clone(),
        };
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        self.set_header(id, class);
        id
    }

    /// Copies a template, as retail makes an instanced subobject; `template` itself for a stub,
    /// which has no class to make one of.
    pub(crate) fn duplicate_object(&mut self, template: ObjectId) -> ObjectId {
        let Some(class) = self.object(template).class else { return template };
        let copy = self.new_object(class, None);
        let mem = self.object(template).mem.clone();
        self.store.objects[copy.0 as usize].mem = mem;
        self.set_header(copy, class);
        copy
    }

    /// Written at `Object`'s own slots, the last of their name: a subclass may declare a `Name` of its own
    /// (`ACTION_IfGameState`'s `String Name`, `DAY_A`), which a lookup by name finds first.
    fn set_header(&mut self, id: ObjectId, class: ClassId) {
        if self.class(class).is_actor() {
            self.store.actor_ids.insert(id);
            self.store.dynamic_ids.added(id);
        }
        let name = self.object(id).name;
        if let Some(slot) = self.class(class).name_slot {
            self.set_property_at(id, slot, Value::Name(name));
        }
        let class_object = match self.class(class).class_object {
            Some(class_object) => class_object,
            None => {
                let class_path = format!("{}.{}", self.loader.package(self.class(class).package).name, self.name(self.class(class).name));
                let class_object = self.stub(&class_path, "Class");
                self.store.classes[class.0 as usize].class_object = Some(class_object);
                class_object
            }
        };
        if let Some(slot) = self.class(class).class_slot {
            self.set_property_at(id, slot, Value::Object(Some(class_object)));
        }
    }

    /// The key of the object at `path` of class `class_name` in the object table: its lower-case path, unless another
    /// object of a different class already holds that path and the package has exports of both classes under it
    /// (`ED_Weapons.ED_MP5_NEW` is a `SkeletalMesh` and a `MeshAnimation`); then the class is part of the key.
    fn object_key(&self, path: &str, class_name: &str) -> String {
        let key = path.to_ascii_lowercase();
        let Some(&existing) = self.store.object_ids.get(&key) else { return key };
        let existing = self.name(self.object(existing).class_name);
        if class_name.is_empty() || existing.eq_ignore_ascii_case(class_name) {
            return key;
        }
        let ours = self.loader.loaded_export_by_path_of_class(path, class_name);
        let theirs = self.loader.loaded_export_by_path_of_class(path, existing);
        match (ours, theirs) {
            (Some(ours), Some(theirs)) if ours != theirs => format!("{key}|{}", class_name.to_ascii_lowercase()),
            _ => key,
        }
    }

    pub(crate) fn export_of(&self, object: ObjectId) -> Option<(PackageId, usize)> {
        let object = self.object(object);
        let class = self.name(object.class_name);
        std::iter::once(class)
            .chain(interp::intrinsic_subclasses(class))
            .find_map(|class| self.loader.loaded_export_by_path_of_class(&object.path, class))
            .or_else(|| self.loader.loaded_export_by_path(&object.path))
    }

    pub fn stub(&mut self, path: &str, class_name: &str) -> ObjectId {
        let key = self.object_key(path, class_name);
        if let Some(&id) = self.store.object_ids.get(&key) {
            return id;
        }
        let name = path.rsplit('.').next().unwrap_or(path);
        let object = Object {
            name: self.intern(name),
            class_name: self.intern(class_name),
            path: path.to_owned(),
            class: None,
            mem: memory::Mem::default(),
        };
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        id
    }

    /// Assets stay stubs until a script uses them. A failed load leaves the stub (calls return zero, property reads read
    /// zero).
    pub(crate) fn load_stub(&mut self, object: ObjectId) {
        if self.object(object).class.is_none()
            && let Some((package, export)) = self.export_of(object)
            && self.load_object(package, export).is_err()
        {
            self.diagnostics.log.push(format!("Could not load {}", self.object(object).path));
        }
    }

    pub fn load_object(&mut self, package: PackageId, export: usize) -> Result<ObjectId, Error> {
        let loader = self.loader.clone();
        let loaded = loader.package(package);
        let pkg = &loaded.package;
        let entry = pkg.exports.get(export).ok_or(Error::BadExport(export))?;
        let id = self.object_ref(pkg, package, export as i32 + 1).ok_or(Error::BadExport(export))?;
        let Some((class_package, class_export)) = self.loader.loaded_export(package, entry.class_index) else {
            return Ok(id);
        };
        if self.object(id).class.is_some() || editor_only(pkg, export) {
            return Ok(id);
        }
        let class = self.load_class(class_package, class_export)?;
        let layout = Arc::clone(&self.class(class).layout);
        let mut properties = self.class(class).defaults.clone();
        let mut r = pkg.export_reader(&loaded.data, entry)?;
        let tags = read_properties(pkg, &mut r, &layout)?;
        self.apply(pkg, package, &layout, self.class(class).mem, &mut properties, tags.values);
        let object = &mut self.store.objects[id.0 as usize];
        object.class = Some(class);
        object.mem = properties;
        self.set_header(id, class);
        Ok(id)
    }

    pub fn property_slot(&self, class: ClassId, name: &str) -> Option<Slot> {
        self.property_index(class, name).map(Slot)
    }

    /// For a reader that asks the same names in the same order every time: later runs read the i-th name asked by the i-th
    /// recorded slot, without hashing, after checking it is that name. The values are those of [`Vm::get`].
    pub fn read_ordered<R>(&self, object: ObjectId, reader: &'static str, f: impl FnOnce(&dyn Fn(&str) -> Option<Value>) -> R) -> R {
        let Some(class) = self.object(object).class else {
            return f(&|name: &str| self.get(object, name));
        };
        let recorded = self.script.slot_lists.lock().ok().and_then(|lists| lists.get(&(class, reader)).cloned());
        let ordered = props::Ordered::new(recorded.as_deref());
        let result =
            f(&|name: &str| ordered.slot(name, |name| self.property_slot(class, name)).and_then(|slot| self.get(object, slot)));
        if let Some(list) = ordered.into_recorded()
            && let Ok(mut lists) = self.script.slot_lists.lock()
        {
            lists.insert((class, reader), list);
        }
        result
    }

    pub(crate) fn field(&self, object: ObjectId, property: impl PropertyName) -> Option<memory::Field> {
        property.field_in(self, self.object(object).class?)
    }

    pub fn get(&self, object: ObjectId, property: impl PropertyName) -> Option<Value> {
        if property.is_resolved()
            && let Some((word, shape)) = self.word(object, property)
        {
            return Some(memory::scalar(word, shape));
        }
        let field = self.field(object, property)?;
        Some(self.script.memory.read_field(&field, &self.object(object).mem))
    }

    #[inline]
    pub(crate) fn word(&self, object: ObjectId, property: impl PropertyName) -> Option<(u32, memory::Shape)> {
        let object_ref = self.object(object);
        let (word, shape) = property.word_in(self, object_ref.class?)?;
        match shape {
            memory::Shape::Byte | memory::Shape::Int | memory::Shape::Bool | memory::Shape::Float | memory::Shape::Object | memory::Shape::Name => {
                Some((*object_ref.mem.words.get(word as usize)?, shape))
            }
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn member_words<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[(u32, memory::Shape); N]> {
        const { assert!(N <= 3, "member_words reaches a struct's first three members (`MemLayout::head`)") };
        let object_ref = self.object(object);
        let (base, memory::Shape::Struct(id)) = property.word_in(self, object_ref.class?)? else { return None };
        let head = &self.script.memory.get(id).head;
        let words = &object_ref.mem.words;
        Some(std::array::from_fn(|i| match head.get(i) {
            Some(&(word, shape)) if shape != memory::Shape::Map => (words.get((base + word) as usize).copied().unwrap_or(0), shape),
            _ => (0, memory::Shape::Map),
        }))
    }

    /// R, G, B, A (`Object.uc` declares B, G, R, A), read in place. All four: [`Vm::member_words`] reaches only the first
    /// three.
    pub(crate) fn color_of(&self, object: ObjectId, property: impl PropertyName) -> Option<[u8; 4]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return None };
        let (members, words) = (&self.script.memory.get(id).fields, &self.object(object).mem.words);
        let byte = |i: usize| members.get(i).and_then(|m| words.get((field.word + m.word) as usize)).map(|&word| word as u8);
        Some([byte(2)?, byte(1)?, byte(0)?, byte(3)?])
    }

    pub(crate) fn members<const N: usize>(&self, object: ObjectId, property: impl PropertyName) -> Option<[Value; N]> {
        Some(self.member_words::<N>(object, property)?.map(|(word, shape)| memory::scalar(word, shape)))
    }

    pub(crate) fn set_members<const N: usize>(&mut self, object: ObjectId, property: impl PropertyName, values: [Value; N]) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        let (memory::Shape::Struct(id), 1) = (field.shape, field.dim) else { return false };
        let members = &self.script.memory.get(id).fields;
        let mem = &mut self.store.objects[object.0 as usize].mem;
        for (m, value) in members.iter().zip(values) {
            self.script.memory.write(m.shape, m.dim, mem, (field.word + m.word) as usize, (field.heap + m.heap) as usize, value);
        }
        true
    }

    pub(crate) fn array_words(&self, object: ObjectId, property: impl PropertyName) -> Option<&[u32]> {
        let field = self.field(object, property)?;
        let (memory::Shape::Array(elem), 1) = (field.shape, field.dim) else { return None };
        if self.script.memory.element_size(elem) != (1, 0) {
            return None;
        }
        match self.object(object).mem.heap.get(field.heap as usize)? {
            memory::Heap::Array(array) => array.mem.words.get(..array.len as usize),
            memory::Heap::Str(_) => None,
        }
    }

    pub(crate) fn element_of(&self, object: ObjectId, property: impl PropertyName, index: usize) -> Option<Value> {
        let field = self.field(object, property)?;
        let mem = &self.object(object).mem;
        if field.dim > 1 {
            if index >= field.dim as usize {
                return None;
            }
            let (word, heap) = (field.word as usize + index * field.words as usize, field.heap as usize + index * field.heaps as usize);
            return Some(self.script.memory.read(field.shape, 1, mem, word, heap));
        }
        let memory::Shape::Array(elem) = field.shape else { return None };
        let memory::Heap::Array(array) = mem.heap.get(field.heap as usize)? else { return None };
        if index >= array.len as usize {
            return None;
        }
        let f = self.script.memory.get(elem).fields[0];
        let (w, h) = self.script.memory.element_size(elem);
        Some(self.script.memory.read(f.shape, f.dim, &array.mem, index * w as usize, index * h as usize))
    }

    pub(crate) fn object_list(&self, object: ObjectId, property: impl PropertyName) -> Vec<ObjectId> {
        self.array_words(object, property).map_or_else(Vec::new, |words| words.iter().filter_map(|&w| memory::object(w)).collect())
    }

    pub fn property_block(&self, object: ObjectId) -> &PropertyBlock {
        &self.object(object).mem
    }

    pub fn properties(&self, object: ObjectId) -> Vec<Value> {
        let object = self.object(object);
        let Some(class) = object.class else { return Vec::new() };
        self.script.memory.get(self.class(class).mem).fields.iter().map(|field| self.script.memory.read_field(field, &object.mem)).collect()
    }

    pub fn property_at(&self, object: ObjectId, index: usize) -> Option<Value> {
        let object_ref = self.object(object);
        let field = *self.script.memory.get(self.class(object_ref.class?).mem).fields.get(index)?;
        Some(self.script.memory.read_field(&field, &object_ref.mem))
    }

    pub(crate) fn set_property_at(&mut self, object: ObjectId, index: usize, value: Value) -> bool {
        let Some(class) = self.object(object).class else { return false };
        let Some(&field) = self.script.memory.get(self.class(class).mem).fields.get(index) else { return false };
        self.script.memory.write_field(&field, &mut self.store.objects[object.0 as usize].mem, value);
        true
    }

    fn member_place(&self, class: ClassId, first: &str, members: &[&str]) -> Option<(memory::Shape, u32, usize, usize)> {
        let index = self.property_index(class, first)?;
        let field = *self.script.memory.get(self.class(class).mem).fields.get(index)?;
        let mut ty = &self.class(class).layout.properties[index].ty;
        let (mut shape, mut dim, mut word, mut heap) = (field.shape, field.dim, field.word as usize, field.heap as usize);
        for member in members {
            let (Type::Struct(layout), memory::Shape::Struct(id), 1) = (ty, shape, dim) else { return None };
            let index = layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(member))?;
            let f = *self.script.memory.get(id).fields.get(index)?;
            ty = &layout.properties[index].ty;
            (shape, dim, word, heap) = (f.shape, f.dim, word + f.word as usize, heap + f.heap as usize);
        }
        Some((shape, dim, word, heap))
    }

    pub fn get_path(&self, object: ObjectId, path: &[&str]) -> Option<Value> {
        let (first, members) = path.split_first()?;
        let (shape, dim, word, heap) = self.member_place(self.object(object).class?, first, members)?;
        Some(self.script.memory.read(shape, dim, &self.object(object).mem, word, heap))
    }

    pub fn set_path(&mut self, object: ObjectId, path: &[&str], value: Value) -> bool {
        let Some((first, members)) = path.split_first() else { return false };
        let Some(class) = self.object(object).class else { return false };
        let Some((shape, dim, word, heap)) = self.member_place(class, first, members) else { return false };
        self.script.memory.write(shape, dim, &mut self.store.objects[object.0 as usize].mem, word, heap, value);
        true
    }

    pub fn set(&mut self, object: ObjectId, property: impl PropertyName, value: Value) -> bool {
        let Some(field) = self.field(object, property) else { return false };
        self.script.memory.write_field(&field, &mut self.store.objects[object.0 as usize].mem, value);
        true
    }

    pub fn export_class(&self, path: &str, wanted: &str) -> Option<Option<String>> {
        let package = path.split('.').next()?;
        self.loader.loaded_id(package)?;
        let by_class = (!wanted.is_empty())
            .then(|| {
                std::iter::once(wanted)
                    .chain(interp::intrinsic_subclasses(wanted))
                    .find_map(|class| self.loader.loaded_export_by_path_of_class(path, class))
            })
            .flatten();
        Some(by_class.or_else(|| self.loader.loaded_export_by_path(path)).map(|(id, export)| {
            let pkg = &self.loader.package(id).package;
            pkg.object_name(pkg.exports[export].class_index).unwrap_or("Class").to_owned()
        }))
    }

    pub fn call_native(&mut self, name: &str, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        natives::call(self, name, native_id::Native::from_name(name), this, args)
    }

    pub fn texture_size(&mut self, texture: ObjectId) -> [i32; 2] {
        if let Some(&size) = self.media.texture_sizes.get(&texture) {
            return size;
        }
        let phase = self.profile_start();
        let path = self.object(texture).path.clone();
        let size_of = |loader: &Loader, package: &Package, data: &[u8], export: usize| {
            let texture = p2_assets::read_texture(&mut Objects::new(loader), Source { package, data }, export).ok()?;
            texture.mips.first().map(|mip| [mip.width as i32, mip.height as i32])
        };
        let size = match self.export_of(texture) {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                size_of(&self.loader, &loaded.package, &loaded.data, export)
            }
            None => self.side_export(&path).and_then(|(side, export)| size_of(&self.loader, &side.package, &side.data, export)),
        }
        .unwrap_or([0, 0]);
        self.media.texture_sizes.insert(texture, size);
        self.profile_phase("load: texture size", phase);
        size
    }

    pub fn sound_file(&mut self, path: &str) -> Option<p2_assets::Sound> {
        if let Some((package, export)) = self.loader.loaded_export_by_path(path) {
            let loaded = self.loader.package(package);
            return p2_assets::read_sound(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok();
        }
        let (side, export) = self.side_export(path)?;
        p2_assets::read_sound(&side.package, &side.data, &side.package.exports[export]).ok()
    }

    pub fn sound_duration(&mut self, sound: ObjectId) -> Option<f32> {
        if let Some(&duration) = self.media.sound_durations.get(&sound) {
            return duration;
        }
        let phase = self.profile_start();
        let path = self.object(sound).path.clone();
        let duration = |pkg: &Package, data: &[u8], export: usize| p2_assets::read_sound_duration(pkg, data, &pkg.exports[export]).ok()?;
        let found = match self.export_of(sound) {
            Some((package, export)) => {
                let loaded = self.loader.package(package);
                duration(&loaded.package, &loaded.data, export)
            }
            None => self.side_sound_duration(&path),
        };
        self.media.sound_durations.insert(sound, found);
        self.profile_phase("load: sound duration", phase);
        found
    }

    pub fn class_package(&self, name: &str) -> Option<String> {
        self.loader.package_names().filter_map(|package| self.loader.loaded_id(package)).find_map(|id| {
            let loaded = self.loader.package(id);
            let pkg = &loaded.package;
            pkg.exports
                .iter()
                .any(|e| e.class_index == 0 && pkg.name(e.object_name).eq_ignore_ascii_case(name))
                .then(|| loaded.name.clone())
        })
    }

    pub fn default_value(&self, class: ClassId, property: impl PropertyName) -> Option<Value> {
        let field = property.field_in(self, class)?;
        Some(self.script.memory.read_field(&field, &self.class(class).defaults))
    }

    fn load_class(&mut self, package: PackageId, export: usize) -> Result<ClassId, Error> {
        self.load_class_at(package, export, 0)
    }

    fn load_class_at(&mut self, package: PackageId, export: usize, depth: u32) -> Result<ClassId, Error> {
        if let Some(&id) = self.store.class_ids.get(&(package, export)) {
            return Ok(id);
        }
        let loader = self.loader.clone();
        let loaded = loader.package(package);
        if depth >= MAX_CLASS_DEPTH {
            return Err(Error::ClassNotFound(loaded.package.name(loaded.package.exports[export].object_name).to_owned()));
        }
        let source = Source { package: &loaded.package, data: &loaded.data };
        let Some(ScriptObject::Class(class)) = read_export(source.package, source.data, export)? else {
            return Err(Error::ClassNotFound(loaded.package.name(loaded.package.exports[export].object_name).to_owned()));
        };
        let super_class = match self.loader.loaded_export(package, class.base.base.field.super_field) {
            Some((super_package, super_export)) => Some(self.load_class_at(super_package, super_export, depth + 1)?),
            None => None,
        };
        let phase = self.profile_start();
        let layout = self.layouts.layout(source, export)?;

        // The layout lists the class's own properties first, then the superclass's (whose defaults apply). In memory
        // the superclass's come first, at the superclass's offsets, as in the engine.
        let base = super_class.map(|id| (self.class(id).mem, self.class(id).layout.properties.len()));
        let mem_layout = self.script.memory.of_class(&layout, base);
        let mut defaults = self.script.memory.zero(mem_layout);
        if let Some(id) = super_class {
            let inherited = &self.class(id).defaults;
            defaults.words[..inherited.words.len()].copy_from_slice(&inherited.words);
            defaults.heap[..inherited.heap.len()].clone_from_slice(&inherited.heap);
        }
        let entry = &source.package.exports[export];
        let end = (entry.serial_offset + entry.serial_size).min(source.data.len());
        let mut r = Reader::new(&source.data[..end]);
        r.seek(class.defaults_offset)?;
        let tags = read_properties(source.package, &mut r, &layout)?;
        self.apply(source.package, package, &layout, mem_layout, &mut defaults, tags.values);
        let class_path = format!("{}.{}", loaded.name, source.package.name(entry.object_name));
        let config_file = match source.package.name(class.config_name) {
            "" | "None" => "System".to_owned(),
            name => name.to_owned(),
        };
        self.load_config(class.flags, &config_file, &class_path, super_class, &layout, mem_layout, &mut defaults);

        let name = self.intern(source.package.name(entry.object_name));
        // The default object's native `Class` is the class (`Default.Class` in static functions, e.g.
        // `P2GameInfoSingle.GetStartURL`).
        let class_object = self.stub(&class_path, "Class");
        let id = ClassId(self.store.classes.len() as u32);
        let mut index = FxHashMap::default();
        for (i, field) in layout.properties.iter().enumerate() {
            index.entry(field.name.to_string()).or_insert(i);
        }
        let lower = lowercase_index(&layout);
        let slot_of = |p: &str| index.get(p).copied().or_else(|| lower_lookup(&lower, p));
        let fields = &self.script.memory.get(mem_layout).fields;
        let resolved = props::Resolved::new(slot_of, fields, self.name(name), super_class.map_or(0, |s| self.class(s).resolved.kinds()));
        let lower_index = std::sync::OnceLock::from(lower);
        let mem = mem_layout;
        let last_named = |name: &str| layout.properties.iter().rposition(|p| p.name.eq_ignore_ascii_case(name));
        let (name_slot, class_slot) = (last_named("Name"), last_named("Class"));
        self.store.classes.push(Class {
            name,
            package,
            export,
            super_class,
            layout,
            mem,
            defaults,
            index,
            lower_index,
            resolved,
            class_object: Some(class_object),
            name_slot,
            class_slot,
        });
        self.store.class_ids.insert((package, export), id);
        self.store.class_by_name.entry(name).or_insert(id);
        if let Some(index) = self.property_index(id, "Class") {
            let field = self.script.memory.get(mem).fields[index];
            self.script.memory.write_field(&field, &mut self.store.classes[id.0 as usize].defaults, Value::Object(Some(class_object)));
        }
        self.profile_phase("load: class", phase);
        Ok(id)
    }

    fn apply(
        &mut self,
        pkg: &Package,
        package: PackageId,
        layout: &StructLayout,
        mem_layout: memory::LayoutId,
        mem: &mut memory::Mem,
        tags: Vec<p2_script::PropertyValue>,
    ) {
        let indices = Arc::clone(self.script.lookups.tag_indices.entry(mem_layout).or_insert_with(|| Arc::new(lowercase_index(layout))));
        for tag in tags {
            let Some(index) = lower_lookup(&indices, &tag.name) else {
                continue;
            };
            let value = self.convert(pkg, package, &layout.properties[index].ty, tag.value);
            let field = self.script.memory.get(mem_layout).fields[index];
            self.script.memory.write_element(&field, mem, tag.array_index as usize, value);
        }
    }

    fn convert(&mut self, pkg: &Package, package: PackageId, ty: &Type, value: p2_script::StoredValue) -> Value {
        use p2_script::StoredValue as V;
        if let Type::Array(inner) = ty
            && !matches!(value, V::Array(_))
        {
            return self.convert(pkg, package, inner, value);
        }
        match value {
            V::Byte(v) => Value::Byte(v),
            V::Int(v) => Value::Int(v),
            V::Bool(v) => Value::Bool(v),
            V::Float(v) => Value::Float(v),
            V::Object(reference) if reference > 0 && editor_only(pkg, reference as usize - 1) => Value::Object(None),
            V::Object(reference) => Value::Object(self.object_ref(pkg, package, reference)),
            V::Name(name) => Value::Name(self.intern(pkg.name(name))),
            V::Str(v) => Value::Str(v),
            V::Array(values) => {
                let inner = match ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => inner.as_ref(),
                    other => other,
                };
                Value::Array(values.into_iter().map(|v| self.convert(pkg, package, inner, v)).collect())
            }
            V::Struct(members) => {
                let Type::Struct(layout) = ty else { return Value::Struct(Vec::new()) };
                let mut slots: Vec<Value> = layout.properties.iter().map(|f| zero_slot(&f.ty, f.array_dim)).collect();
                for member in members.values {
                    let Some(index) = layout.properties.iter().position(|p| p.name.eq_ignore_ascii_case(&member.name))
                    else {
                        continue;
                    };
                    let field = &layout.properties[index];
                    let value = self.convert(pkg, package, &field.ty, member.value);
                    set_element(&mut slots[index], &field.ty, field.array_dim, member.array_index, value);
                }
                Value::of_struct(layout, slots)
            }
            V::Delegate { object, function } => {
                Value::Delegate { object: self.object_ref(pkg, package, object), function: self.intern(pkg.name(function)) }
            }
            V::Map => Value::Map,
        }
    }

    fn object_ref(&mut self, pkg: &Package, package: PackageId, reference: i32) -> Option<ObjectId> {
        if reference == 0 {
            return None;
        }
        let mut parts = Vec::new();
        let mut current = reference;
        for _ in 0..=pkg.imports.len() + pkg.exports.len() {
            if current == 0 {
                break;
            }
            parts.push(pkg.object_name(current)?);
            current = match current {
                ..0 => pkg.imports.get(current.unsigned_abs() as usize - 1)?.package_index,
                _ => pkg.exports.get(current as usize - 1)?.package_index,
            };
        }
        let loader = self.loader.clone();
        if reference > 0 {
            parts.push(&loader.package(package).name);
        }
        parts.reverse();
        let path = parts.join(".");
        let class_name = match reference {
            ..0 => pkg.name(pkg.imports.get(reference.unsigned_abs() as usize - 1)?.class_name),
            _ => match pkg.exports.get(reference as usize - 1)?.class_index {
                0 => "Class",
                class => pkg.object_name(class)?,
            },
        };
        let key = self.object_key(&path, class_name);
        if let Some(&id) = self.store.object_ids.get(&key) {
            return Some(id);
        }
        let object = Object {
            name: self.intern(parts.last()?),
            class_name: self.intern(class_name),
            path,
            class: None,
            mem: memory::Mem::default(),
        };
        let id = ObjectId(self.store.objects.len() as u32);
        self.store.objects.push(object);
        self.store.object_ids.insert(key, id);
        if let Some(found) = loader.resolve_loaded(package, reference)
            && let Some(export) = found.export
            && self.loads_on_reference(found.package, export)
        {
            return self.load_object(found.package, export).ok();
        }
        Some(id)
    }

    fn loads_on_reference(&mut self, package: PackageId, export: usize) -> bool {
        let class_index = self.loader.package(package).package.exports[export].class_index;
        let Some((class_package, class_export)) = self.loader.loaded_export(package, class_index) else { return false };
        let Ok(class) = self.load_class(class_package, class_export) else { return false };
        !["Actor", "Material", "Sound", "Font", "Palette", "MeshAnimation"].iter().any(|skip| self.class_is_a(class, skip))
    }
}

const MAX_CLASS_DEPTH: u32 = 256;

const RF_LOAD_FOR_ANY: u32 = 0x7_0000;
const RF_LOAD_FOR_GAME: u32 = 0x3_0000;

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
