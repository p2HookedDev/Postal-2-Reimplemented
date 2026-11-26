use super::*;
use p2_render::world::frame_facts::{Glow, GlowSource, ScriptLight};

pub(in crate::play) fn glows(vm: &Vm<Arc<Loader>>, actor: ObjectId) -> bool {
    [Kind::FireEmitter, Kind::XMuzzleFlashEmitter, Kind::MuzzleFlashAttachment].into_iter().any(|kind| vm.store.is_a(actor, kind))
}

fn glow(vm: &Vm<Arc<Loader>>, actor: ObjectId) -> Option<GlowSource> {
    let hidden = vm.store.flag_of(actor, Prop::bHidden);
    if vm.store.is_a(actor, Kind::FireEmitter) {
        let burning = vm.store.objects_of(actor, Prop::Emitters).flatten().next().is_some_and(|emitter| !vm.store.flag_of(emitter, Prop::Disabled));
        return (burning && !hidden).then(|| GlowSource { location: vm.store.location(actor), kind: Glow::Fire { life_left: vm.store.float_of(actor, Prop::LifeSpan) } });
    }
    let values = crate::actors::light_values(vm, actor);
    let light = ScriptLight {
        rotation: values.rotation,
        effect: values.effect,
        brightness: values.brightness,
        hue: values.hue,
        saturation: values.saturation,
        radius: values.radius,
        cone: values.cone,
    };
    if vm.store.is_a(actor, Kind::MuzzleFlashAttachment) {
        return (!hidden && light.brightness > 0.0).then_some(GlowSource { location: values.location, kind: Glow::ShotFlash(light) });
    }
    let Some(p2_vm::Value::Float(whole)) = vm.store.default_value(vm.store.object(actor).class?, Prop::LifeSpan) else { return None };
    let location = vm.store.object_of(actor, Prop::Owner).map_or(values.location, |holder| vm.store.location(holder));
    Some(GlowSource { location, kind: Glow::Flash { light, age: whole - vm.store.float_of(actor, Prop::LifeSpan) } })
}

impl Simulation {
    pub(super) fn read_glows(&self, frame: &mut VmFrame) {
        let vm = &self.vm;
        let unlit = |actor: &&ObjectId| vm.store.byte_of(**actor, Prop::LightType) == 0 && !vm.store.is_deleted(**actor);
        frame.glows.extend(self.glows.iter().filter(unlit).filter_map(|&actor| glow(vm, actor)));
    }
}
