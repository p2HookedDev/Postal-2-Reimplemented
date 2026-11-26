use super::*;

pub(crate) fn projector_decal(
    game: &mut Game,
    t: &mut impl SceneAssets,
    projector: &ProjectorActor,
    images: &mut HashMap<String, Option<(Handle<Image>, Vec2, Option<FinalBlend>)>>,
) -> Option<(Handle<Image>, Transform, u32, bool)> {
    let (image, size, final_blend) = images.entry(projector.texture.clone()).or_insert_with(|| projector_image(game, |image| t.add_image(image), &projector.texture)).clone()?;
    Some((image, projector_transform(projector, size), projector_tag(projector, final_blend), p2_render::wounds::is_blood(&projector.texture)))
}

pub(super) const PROJECTOR_TAG: u32 = 1 << 31;

pub(super) const ALPHA_TEST_TAG: u32 = 1 << 21;
pub(super) const TAGGED_TAG: u32 = 1 << 30;

const FB_MODULATE: u8 = 1;
const FB_ALPHA_BLEND: u8 = 2;
const FB_TRANSLUCENT: u8 = 4;

pub(crate) fn projector_tag(projector: &ProjectorActor, final_blend: Option<FinalBlend>) -> u32 {
    let (blending, alpha_ref) = match final_blend {
        Some(inner) if projector.material_blending == ProjectorBlending::None => (inner.blending.byte(), inner.alpha_ref),
        _ => (match projector.blending { ProjectorBlending::AlphaBlend => FB_ALPHA_BLEND, ProjectorBlending::Add => FB_TRANSLUCENT, _ => FB_MODULATE }, None),
    };
    let tagged = if projector.tagged { TAGGED_TAG } else { 0 };
    PROJECTOR_TAG | tagged | blend_bits(blending, alpha_ref) | u32::from(projector.targets.0) << 16 | u32::from(projector.material_blending as u8) << 8
}

fn blend_bits(blending: u8, alpha_ref: Option<u8>) -> u32 {
    alpha_ref.map_or(0, |alpha_ref| ALPHA_TEST_TAG | u32::from(alpha_ref) << 22) | u32::from(blending)
}

pub(crate) fn plain_projector_blend(final_blend: Option<FinalBlend>) -> u32 {
    final_blend.map_or(u32::from(FB_MODULATE), |inner| blend_bits(inner.blending.byte(), inner.alpha_ref))
}

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

/// In Bevy space. Splats spawn at the trace's hit, so the surface lies in the projector's plane; a box
/// starting there would have the surface on its face, where Bevy's inside test is decided by float rounding
/// pixel by pixel: grainy splats that come and go as the camera turns.
pub fn projector_box(location: Vector, rotation: Rotator, side: f32, trace: f32) -> Transform {
    let axes = rotator_matrix(rotation);
    let (x, y, z) = (axes.x_axis.truncate(), axes.y_axis.truncate(), axes.z_axis.truncate());
    let depth = trace + DECAL_BACK_REACH;
    let center = Vec3::from_array(location.into()) + x * (trace - DECAL_BACK_REACH) / 2.0;
    let matrix = Mat4::from_cols((y * side).extend(0.0), (z * side).extend(0.0), (-x * depth).extend(0.0), center.extend(1.0));
    let swap = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);
    Transform::from_matrix(swap * matrix)
}
