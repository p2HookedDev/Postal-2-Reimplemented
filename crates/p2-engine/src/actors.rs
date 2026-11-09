use std::sync::Arc;

use p2_assets::EDrawType;
use p2_math::{Rotator, Vector};
use p2_render_cpu::{MeshLight, MeshPlacement, hsv_color};
use rustc_hash::FxHashMap;

use crate::render::actor_light::{ActorLighting, DEFAULT_MAX_LIGHTS};
use p2_particles::{EmitterKind, EmitterProperties, EmitterSettings, PropertyValue};
use p2_vm::{Kind, LoaderRef, ObjectId, Prop, Value, Vm};

pub fn antiportal_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<(String, MeshPlacement)> {
    if vm.draw_type_of(actor) != Some(EDrawType::AntiPortal) || vm.store.flag_of(actor, Prop::bHidden) || vm.store.is_deleted(actor) {
        return None;
    }
    let Some(Value::Object(Some(volume))) = vm.store.get(actor, Prop::AntiPortal) else { return None };
    let (location, rotation) = location_rotation(vm, actor);
    let placement = MeshPlacement { location, rotation, scale: actor_scale(vm, actor), pre_pivot: vm.store.vector(actor, Prop::PrePivot) };
    Some((vm.store.object(volume).path.clone(), placement))
}

pub struct SkeletalActor {
    pub object: ObjectId,
    pub mesh: String,
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
    pub skins: Vec<Option<String>>,
    pub attached_to: Option<(ObjectId, String)>,
    pub relative_location: Vector,
    pub relative_rotation: Rotator,
    pub lighting: ActorLighting,
}

pub fn actor_lighting(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> ActorLighting {
    ActorLighting {
        unlit: vm.store.flag_of(actor, Prop::bUnlit),
        glow: vm.store.byte_of(actor, Prop::AmbientGlow),
        max_lights: match vm.store.get(actor, Prop::MaxLights) {
            Some(Value::Byte(value)) => value,
            _ => DEFAULT_MAX_LIGHTS,
        },
    }
}

pub(crate) fn object_path(vm: &Vm<impl LoaderRef>, value: Option<&Value>) -> Option<String> {
    match value {
        Some(&Value::Object(Some(object))) => Some(vm.store.object(object).path.clone()),
        _ => None,
    }
}

pub(crate) fn skin_paths(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Vec<Option<String>> {
    match vm.store.get(actor, Prop::Skins) {
        Some(Value::Array(skins)) => skins.iter().map(|skin| object_path(vm, Some(skin))).collect(),
        _ => Vec::new(),
    }
}

/// Key of what `actor`'s model is built from. Retail draws whatever the actor holds each frame, so scripts change looks
/// live (`P2Pawn.SwapToBurnVictim`); the host rebuilds a model when its key changes ([`look_key`]). `AmbientGlow` is
/// left out: it changes the light, not the model.
pub fn actor_look(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> u64 {
    let skins = match vm.store.get(actor, Prop::Skins) {
        Some(Value::Array(skins)) => skins,
        _ => Vec::new(),
    };
    let skins = skins.iter().map(|skin| if let Value::Object(object) = skin { *object } else { None });
    let draw_type = vm.store.byte_of(actor, Prop::DrawType);
    let unlit = vm.store.flag_of(actor, Prop::bUnlit);
    look_key([vm.store.object_of(actor, Prop::Mesh), vm.store.object_of(actor, Prop::StaticMesh)].into_iter().chain(skins), draw_type, unlit)
}

/// Applied to the transform as it changes: scripts grow puddles and streams every frame, so not part of [`actor_look`].
pub fn actor_scale(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Vector {
    let draw_scale = match vm.store.get(actor, Prop::DrawScale) {
        Some(Value::Float(scale)) => scale,
        _ => 1.0,
    };
    vm.store.vector(actor, Prop::DrawScale3D).map(|s| s * draw_scale)
}

pub fn look_key<T: std::hash::Hash>(objects: impl IntoIterator<Item = Option<T>>, draw_type: u8, unlit: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    for object in objects {
        object.hash(&mut hasher);
    }
    (draw_type, unlit).hash(&mut hasher);
    hasher.finish()
}

pub fn skeletal_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<SkeletalActor> {
    // Hidden ones too: scripts may show them later (the followers' visibility follows `bHidden`).
    if vm.draw_type_of(actor) != Some(EDrawType::Mesh) || vm.store.is_deleted(actor) {
        return None;
    }
    let mesh = object_path(vm, vm.store.get(actor, Prop::Mesh).as_ref())?;
    let (location, rotation) = location_rotation(vm, actor);
    let skins = skin_paths(vm, actor);
    Some(SkeletalActor {
        object: actor,
        attached_to: attached_bone(vm, actor).map(|(base, bone)| (base, vm.store.name(bone).to_owned())),
        relative_location: vm.store.vector(actor, Prop::RelativeLocation),
        relative_rotation: vm.store.rotator_of(actor, Prop::RelativeRotation),
        mesh,
        location,
        rotation,
        scale: actor_scale(vm, actor),
        pre_pivot: vm.store.vector(actor, Prop::PrePivot),
        skins,
        lighting: actor_lighting(vm, actor),
    })
}

#[derive(Clone, PartialEq)]
pub struct BoneAttachment {
    pub base: ObjectId,
    pub bone: Arc<str>,
    pub bone_id: p2_vm::NameId,
    pub relative_location: Vector,
    pub relative_rotation: Rotator,
}

fn attached_bone(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<(ObjectId, p2_vm::NameId)> {
    match (vm.store.get(actor, Prop::Base), vm.store.get(actor, Prop::AttachmentBone)) {
        (Some(Value::Object(Some(base))), Some(Value::Name(bone))) if bone != p2_vm::NameId::NONE => Some((base, bone)),
        _ => None,
    }
}

pub fn bone_attachment(vm: &Vm<impl LoaderRef>, actor: ObjectId, names: &mut FxHashMap<p2_vm::NameId, Arc<str>>) -> Option<BoneAttachment> {
    let (base, bone_id) = attached_bone(vm, actor)?;
    let bone = names.entry(bone_id).or_insert_with(|| Arc::from(vm.store.name(bone_id).to_ascii_lowercase()));
    Some(BoneAttachment {
        base,
        bone: Arc::clone(bone),
        bone_id,
        relative_location: vm.store.vector(actor, Prop::RelativeLocation),
        relative_rotation: vm.store.rotator_of(actor, Prop::RelativeRotation),
    })
}

#[derive(Clone)]
pub struct MeshActor {
    pub static_mesh: String,
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
    pub instance: Option<String>,
    pub lighting: ActorLighting,
    pub blocks: bool,
    pub forced_zone_tag: Option<p2_vm::NameId>,
    pub skins: Vec<Option<String>>,
    pub object: ObjectId,
    pub movable: bool,
    pub accepts_projectors: bool,
}

pub fn mesh_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<MeshActor> {
    if vm.draw_type_of(actor) != Some(EDrawType::StaticMesh) || vm.store.is_deleted(actor) || (vm.store.flag_of(actor, Prop::bHidden) && vm.store.flag_of(actor, Prop::bStatic)) {
        return None;
    }
    let static_mesh = object_path(vm, vm.store.get(actor, Prop::StaticMesh).as_ref())?;
    let (location, rotation) = location_rotation(vm, actor);
    let skins = skin_paths(vm, actor);
    Some(MeshActor {
        static_mesh,
        location,
        rotation,
        scale: actor_scale(vm, actor),
        pre_pivot: vm.store.vector(actor, Prop::PrePivot),
        instance: object_path(vm, vm.store.get(actor, Prop::StaticMeshInstance).as_ref()),
        lighting: actor_lighting(vm, actor),
        blocks: vm.store.flag_of(actor, Prop::bCollideActors) && vm.store.flag_of(actor, Prop::bBlockActors),
        forced_zone_tag: match vm.store.get(actor, "ForcedVisibilityZoneTag") {
            Some(Value::Name(tag)) if tag != p2_vm::NameId::NONE => Some(tag),
            _ => None,
        },
        skins,
        object: actor,
        movable: !vm.store.flag_of(actor, Prop::bStatic),
        accepts_projectors: vm.store.flag_of(actor, "bAcceptsProjectors"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStyle {
    Masked,
    Translucent,
    Modulated,
    Other,
}

impl RenderStyle {
    pub fn from_byte(style: u8) -> Self {
        match style {
            2 => Self::Masked,
            3 => Self::Translucent,
            4 => Self::Modulated,
            _ => Self::Other,
        }
    }
}

/// Retail draws only `DT_Sprite` this way (`DT_VerticalSprite`, `DT_SpriteAnimOnce` and
/// `DT_RopeSprite` draw nothing), from `Texture`, never `Skins`.
pub struct SpriteActor {
    pub object: ObjectId,
    pub texture: String,
    pub location: Vector,
    pub scale: Vector,
    pub style: RenderStyle,
    pub hidden: bool,
}

/// Hidden ones that can move are kept (scripts may show them later); static hidden ones (lights, navigation points:
/// editor-only sprites retail hides in game through `bHidden`) are not.
pub fn sprite_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<SpriteActor> {
    if vm.draw_type_of(actor) != Some(EDrawType::Sprite) || vm.store.is_deleted(actor) || (vm.store.flag_of(actor, Prop::bHidden) && vm.store.flag_of(actor, Prop::bStatic)) {
        return None;
    }
    let Some(Value::Object(Some(texture))) = vm.store.get(actor, Prop::Texture) else { return None };
    Some(SpriteActor {
        object: actor,
        texture: vm.store.object(texture).path.clone(),
        location: vm.store.location(actor),
        scale: actor_scale(vm, actor),
        style: RenderStyle::from_byte(match vm.store.get(actor, Prop::Style) {
            Some(Value::Byte(style)) => style,
            _ => 1,
        }),
        hidden: vm.store.flag_of(actor, Prop::bHidden),
    })
}

pub fn location_rotation(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> (Vector, Rotator) {
    (vm.store.location(actor), vm.store.rotation(actor))
}

pub fn mesh_light(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<MeshLight> {
    let light_type = vm.store.byte_of(actor, Prop::LightType);
    if light_type == 0 || vm.store.is_deleted(actor) {
        return None;
    }
    let (location, rotation) = location_rotation(vm, actor);
    Some(MeshLight {
        location,
        rotation,
        light_type: p2_render_cpu::LightType(light_type),
        period: vm.store.byte_of(actor, Prop::LightPeriod),
        phase: vm.store.byte_of(actor, Prop::LightPhase),
        intensity: 1.0,
        effect: p2_render_cpu::LightEffect(vm.store.byte_of(actor, Prop::LightEffect)),
        brightness: vm.store.float_of(actor, Prop::LightBrightness),
        hue: vm.store.byte_of(actor, Prop::LightHue),
        saturation: vm.store.byte_of(actor, Prop::LightSaturation),
        radius: vm.store.float_of(actor, Prop::LightRadius),
        cone: vm.store.byte_of(actor, Prop::LightCone),
        dynamic: vm.store.flag_of(actor, Prop::bDynamicLight),
    })
}

pub fn zone_ambient(vm: &Vm<impl LoaderRef>, zone: ObjectId) -> [f32; 3] {
    hsv_color(vm.store.byte_of(zone, "AmbientHue"), vm.store.byte_of(zone, "AmbientSaturation"), vm.store.byte_of(zone, "AmbientBrightness"))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneFog {
    pub color: [u8; 3],
    pub start: f32,
    pub end: f32,
}

pub fn zone_fog(vm: &Vm<impl LoaderRef>, zone: ObjectId) -> Option<ZoneFog> {
    if !vm.store.flag_of(zone, "bDistanceFog") {
        return None;
    }
    let color = ["R", "G", "B"].map(|channel| match vm.store.get_path(zone, &["DistanceFogColor", channel]) {
        Some(Value::Byte(value)) => value,
        _ => 0,
    });
    Some(ZoneFog { color, start: vm.store.float_of(zone, "DistanceFogStart"), end: vm.store.float_of(zone, "DistanceFogEnd") })
}

pub struct AmbientActor {
    pub sound: ObjectId,
    pub location: Vector,
    /// `SoundRadius`; 0 = the audio default radius.
    pub radius: f32,
    pub volume: f32,
    pub pitch: f32,
}

pub fn ambient_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<AmbientActor> {
    let Some(Value::Object(Some(sound))) = vm.store.get(actor, Prop::AmbientSound) else { return None };
    if vm.store.is_deleted(actor) {
        return None;
    }
    let byte = |property: Prop, default: u8| match vm.store.get(actor, property) {
        Some(Value::Byte(value)) => value,
        _ => default,
    };
    let radius = vm.store.float_of(actor, Prop::SoundRadius);
    Some(AmbientActor {
        sound,
        location: vm.store.vector(actor, Prop::Location),
        radius,
        volume: f32::from(byte(Prop::SoundVolume, 128)) / 255.0,
        pitch: f32::from(byte(Prop::SoundPitch, 64)) / 64.0,
    })
}

pub struct EmitterActor {
    pub settings: EmitterSettings,
    pub emitters: Vec<(EmitterProperties, Option<String>)>,
    pub objects: Vec<ObjectId>,
    pub meshes: Vec<Option<String>>,
    pub name: String,
}

pub fn emitter_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<EmitterActor> {
    let emitters = emitter_objects(vm, actor);
    if emitters.is_empty() || vm.store.is_deleted(actor) {
        return None;
    }
    let objects = emitters.clone();
    let meshes = emitters
        .iter()
        .map(|&emitter| match vm.store.get(emitter, Prop::StaticMesh) {
            Some(Value::Object(Some(mesh))) => Some(vm.store.object(mesh).path.clone()),
            _ => None,
        })
        .collect();
    let emitters = emitters
        .into_iter()
        .map(|emitter| {
            let texture = match vm.store.get(emitter, Prop::Texture) {
                Some(Value::Object(Some(texture))) => Some(vm.store.object(texture).path.clone()),
                _ => None,
            };
            (emitter_properties(vm, emitter), texture)
        })
        .collect();
    let settings = vm.read_ordered(actor, "EmitterSettings", |get| {
        EmitterSettings::from_lookup(|name: &str| get(name).as_ref().map(property_value))
    });
    Some(EmitterActor { settings, emitters, objects, meshes, name: vm.store.object(actor).path.clone() })
}

pub fn emitter_properties(vm: &Vm<impl LoaderRef>, emitter: ObjectId) -> EmitterProperties {
    let kind = if vm.store.is_a(emitter, "StripEmitter") {
        EmitterKind::Strip
    } else if vm.store.is_a(emitter, "SuperSpriteEmitter") {
        EmitterKind::SuperSprite
    } else if vm.store.is_a(emitter, "MeshEmitter") {
        EmitterKind::Mesh
    } else if vm.store.is_a(emitter, "BeamEmitter") {
        EmitterKind::Beam
    } else {
        EmitterKind::Sprite
    };
    let properties = vm.read_ordered(emitter, "EmitterProperties", |get| {
        EmitterProperties::from_lookup(|name: &str| get(name).as_ref().map(property_value))
    });
    EmitterProperties { kind, ..properties }
}

pub fn emitters_disabled(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Vec<bool> {
    emitter_objects(vm, actor).into_iter().map(|emitter| vm.store.flag_of(emitter, Prop::Disabled)).collect()
}

pub fn emitter_objects(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Vec<ObjectId> {
    match vm.store.get(actor, Prop::Emitters) {
        Some(Value::Array(emitters)) => emitters.iter().filter_map(|e| if let &Value::Object(Some(e)) = e { Some(e) } else { None }).collect(),
        _ => Vec::new(),
    }
}

fn property_value(value: &Value) -> PropertyValue {
    match value {
        &Value::Byte(v) => PropertyValue::Int(i32::from(v)),
        &Value::Int(v) => PropertyValue::Int(v),
        &Value::Bool(v) => PropertyValue::Bool(v),
        &Value::Float(v) => PropertyValue::Float(v),
        Value::Struct(members) => PropertyValue::Struct(members.iter().map(property_value).collect()),
        &Value::Vector(v) => PropertyValue::Struct(<[f32; 3]>::from(v).map(PropertyValue::Float).to_vec()),
        &Value::Rotator(r) => PropertyValue::Struct(<[i32; 3]>::from(r).map(PropertyValue::Int).to_vec()),
        Value::Array(items) => PropertyValue::Array(items.iter().map(property_value).collect()),
        _ => PropertyValue::Int(0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectorBlending {
    None = 0,
    Modulate = 1,
    AlphaBlend = 2,
    Add = 3,
}

impl ProjectorBlending {
    fn from_byte(op: u8) -> Self {
        match op {
            1 => Self::Modulate,
            2 => Self::AlphaBlend,
            3 => Self::Add,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectTargets(pub u8);

pub struct ProjectorActor {
    pub texture: String,
    pub location: Vector,
    pub rotation: Rotator,
    pub draw_scale: f32,
    pub max_trace: f32,
    /// `FOV`: 0 = orthographic.
    pub fov: i32,
    pub blending: ProjectorBlending,
    pub material_blending: ProjectorBlending,
    pub targets: ProjectTargets,
    pub tagged: bool,
}

pub const PROJECT_BSP: u8 = 1;
pub const PROJECT_TERRAIN: u8 = 2;
pub const PROJECT_STATIC_MESH: u8 = 4;
pub const PROJECT_ACTOR: u8 = 8;
pub const PROJECT_ON_UNLIT: u8 = 16;

pub fn projector_actor(vm: &Vm<impl LoaderRef>, actor: ObjectId) -> Option<ProjectorActor> {
    let Some(Value::Object(Some(texture))) = vm.store.get(actor, "ProjTexture") else { return None };
    if !vm.store.is_a(actor, Kind::Projector) || vm.store.is_deleted(actor) {
        return None;
    }
    let int = |name: &str, default: i32| match vm.store.get(actor, name) {
        Some(Value::Int(value)) => value,
        _ => default,
    };
    let (location, rotation) = location_rotation(vm, actor);
    Some(ProjectorActor {
        texture: vm.store.object(texture).path.clone(),
        location,
        rotation,
        draw_scale: match vm.store.get(actor, Prop::DrawScale) {
            Some(Value::Float(scale)) => scale,
            _ => 1.0,
        },
        max_trace: int("MaxTraceDistance", 1000) as f32,
        fov: int("FOV", 0),
        blending: match vm.store.get(actor, "FrameBufferBlendingOp") {
            Some(Value::Byte(op)) => ProjectorBlending::from_byte(op),
            _ => ProjectorBlending::Modulate,
        },
        material_blending: match vm.store.get(actor, "MaterialBlendingOp") {
            Some(Value::Byte(op)) => ProjectorBlending::from_byte(op),
            _ => ProjectorBlending::None,
        },
        targets: ProjectTargets([
            ("bProjectBSP", true, PROJECT_BSP),
            ("bProjectTerrain", true, PROJECT_TERRAIN),
            ("bProjectStaticMesh", true, PROJECT_STATIC_MESH),
            ("bProjectActor", true, PROJECT_ACTOR),
            ("bProjectOnUnlit", false, PROJECT_ON_UNLIT),
        ]
        .into_iter()
        .filter(|&(name, default, _)| match vm.store.get(actor, name) {
            Some(Value::Bool(on)) => on,
            _ => default,
        })
        .fold(0, |bits, (_, _, bit)| bits | bit)),
        tagged: matches!(vm.store.get(actor, "ProjectTag"), Some(Value::Name(tag)) if tag != p2_vm::NameId::NONE),
    })
}

#[cfg(test)]
mod tests {
    use super::look_key;

    #[test]
    fn look_key_changes_with_what_the_model_is_built_from() {
        let body = [Some(1u32), None, Some(10), Some(11)];
        let key = look_key(body, 2, false);
        assert_eq!(key, look_key(body, 2, false));
        assert_ne!(key, look_key([Some(1u32), None, Some(12), Some(11)], 2, false));
        assert_ne!(key, look_key([Some(2u32), None, Some(10), Some(11)], 2, false));
        assert_ne!(key, look_key([Some(1u32), None, Some(10)], 2, false));
        assert_ne!(key, look_key(body, 2, true));
    }
}
