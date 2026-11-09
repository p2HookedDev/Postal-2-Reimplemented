use super::*;

pub(crate) fn projector_decal(game: &mut Game, t: &mut impl SceneAssets, projector: &ProjectorActor) -> Option<(Handle<Image>, Transform, u32)> {
    let (image, size, final_blend) = projector_image(game, t, &projector.texture)?;
    Some((image, projector_transform(projector, size), projector_tag(projector, final_blend)))
}

pub(super) const PROJECTOR_TAG: u32 = 1 << 31;

pub(super) const ALPHA_TEST_TAG: u32 = 1 << 21;
pub(super) const TAGGED_TAG: u32 = 1 << 30;

const FB_MODULATE: u8 = 1;
const FB_ALPHA_BLEND: u8 = 2;
const FB_TRANSLUCENT: u8 = 4;

/// [`PROJECTOR_TAG`], targets in bits 16..21, `MaterialBlendingOp` in bits 8..16, frame buffer blend
/// in bits 0..8, and the alpha test. With `MaterialBlendingOp` none the material's `FinalBlend` wins.
pub(crate) fn projector_tag(projector: &ProjectorActor, final_blend: Option<FinalBlend>) -> u32 {
    let (blending, alpha_ref) = match final_blend {
        Some(inner) if projector.material_blending == ProjectorBlending::None => (inner.blending.byte(), inner.alpha_ref),
        _ => (match projector.blending { ProjectorBlending::AlphaBlend => FB_ALPHA_BLEND, ProjectorBlending::Add => FB_TRANSLUCENT, _ => FB_MODULATE }, None),
    };
    let alpha_test = alpha_ref.map_or(0, |alpha_ref| ALPHA_TEST_TAG | u32::from(alpha_ref) << 22);
    let tagged = if projector.tagged { TAGGED_TAG } else { 0 };
    PROJECTOR_TAG | tagged | alpha_test | u32::from(projector.targets.0) << 16 | u32::from(projector.material_blending as u8) << 8 | u32::from(blending)
}

pub(crate) fn projector_image(game: &mut Game, t: &mut impl SceneAssets, texture: &str) -> Option<(Handle<Image>, Vec2, Option<FinalBlend>)> {
    let (package, export) = game.export_by_path(texture)?;
    let texture = game.material_texture(package, export_reference(export))?;
    let size = texture.image.size().as_vec2();
    Some((t.add_image(texture.image), size, texture.state.final_blend))
}

/// The box holds retail's frustum. Orthographic: the near square (side ½·√2·√(w² + h²)) and
/// `MaxTraceDistance`. Perspective: the far end of a frustum with apex ½·w / tan(½·FOV) behind it.
pub(crate) fn projector_transform(projector: &ProjectorActor, texels: Vec2) -> Transform {
    let frustum = p2_vm::projector::frustum(&p2_vm::projector::ProjectorShape {
        location: projector.location,
        rotation: projector.rotation,
        texels: texels.to_array(),
        draw_scale: projector.draw_scale,
        fov: projector.fov,
        max_trace_distance: projector.max_trace as i32,
        rotating: false,
    });
    let [x, y, z] = projector.rotation.axes();
    let offsets = frustum.vertices.map(|v| v - projector.location);
    let side = offsets.iter().map(|o| o.dot(y).abs().max(o.dot(z).abs())).fold(0.0, f32::max) * 2.0;
    let depth = offsets.iter().map(|o| o.dot(x)).fold(0.0, f32::max);
    projector_box(projector.location, projector.rotation, side, depth)
}

const DECAL_BACK_REACH: f32 = 2.0;

/// In Bevy space. Splats are spawned at the trace's hit: the surface lies in the plane of the projector. A box starting
/// there would have it on its face, where Bevy's inside test (`clustered_decal_iterator_next`, |p| ≤ 0.5) is decided by
/// float rounding pixel by pixel: grainy splats that come and go as the camera turns.
pub fn projector_box(location: Vector, rotation: Rotator, side: f32, trace: f32) -> Transform {
    let axes = rotator_matrix(rotation);
    let (x, y, z) = (axes.x_axis.truncate(), axes.y_axis.truncate(), axes.z_axis.truncate());
    let depth = trace + DECAL_BACK_REACH;
    let center = Vec3::from_array(location.into()) + x * (trace - DECAL_BACK_REACH) / 2.0;
    // The decal's local axes are Bevy's (it projects along local −Z), so the Unreal → Bevy axis swap applies on the
    // world side only. Local X along the texture's U (Y), local Y along Z, local −Z along the projection (X); this
    // basis is mirrored in Unreal terms, which the swap undoes.
    let matrix = Mat4::from_cols((y * side).extend(0.0), (z * side).extend(0.0), (-x * depth).extend(0.0), center.extend(1.0));
    let swap = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);
    Transform::from_matrix(swap * matrix)
}
