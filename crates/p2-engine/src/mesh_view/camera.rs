use super::*;

/// The Bevy camera rotation of an Unreal view rotation (pitch, yaw, roll; 65536 a turn): Unreal looks
/// along X with Z up and Y right, a Bevy camera along −Z with Y up and X right, Y and Z swapped.
pub fn view_rotation(rotation: Rotator) -> Quat {
    let matrix = rotator_matrix(rotation);
    let axis = |i: usize| {
        let v = matrix.col(i);
        Vec3::new(v.x, v.z, v.y)
    };
    let (forward, right, up) = (axis(0), axis(1), axis(2));
    Quat::from_mat3(&Mat3::from_cols(right, up, -forward)).normalize()
}

/// Unreal units. Retail uses 5 for every scene: the view, the sky and each `DrawActor`.
pub const NEAR_CLIPPING_PLANE: f32 = 5.0;

/// `DefaultTexMipBias` as `Postal2.ini` ships it.
const DEFAULT_MIP_BIAS: f32 = -0.5;

pub(super) fn plain_output() -> (Msaa, Tonemapping, DebandDither, MipBias) {
    (Msaa::Off, Tonemapping::None, DebandDither::Disabled, MipBias(DEFAULT_MIP_BIAS))
}

#[derive(SystemParam)]
pub(super) struct SceneCameras<'w, 's> {
    pub(super) views: Query<'w, 's, &'static mut SceneView>,
    pub(super) skies: Query<'w, 's, &'static mut SkyCamera>,
}

fn set_camera(camera: &mut Mut<Camera>, is_active: bool, viewport: Option<&Viewport>) {
    if camera.is_active != is_active {
        camera.is_active = is_active;
    }
    let same = match (camera.viewport.as_ref(), viewport) {
        (None, None) => true,
        (Some(a), Some(b)) => a.physical_position == b.physical_position && a.physical_size == b.physical_size && a.depth == b.depth,
        _ => false,
    };
    if !same {
        camera.viewport = viewport.cloned();
    }
}

/// After the frame is presented: a slot's camera turns on and takes its field of view in the frame its models move to its
/// layer (a kick's foot takes slot 0 and the weapon moves to slot 1), not a frame later.
pub(super) fn place_overlay_cameras(
    view: Res<MeshView>,
    cameras: Query<&Transform, (With<SceneView>, Without<OverlayCamera>)>,
    mut overlays: Query<(&mut Camera, &mut Transform, &mut Projection, &OverlayCamera), Without<SceneView>>,
) {
    let viewport = view.bevy_viewport();
    let view_transform = cameras.iter().next().copied();
    for (mut camera, mut transform, mut projection, OverlayCamera(slot)) in &mut overlays {
        let fov = view.overlays.get(*slot).copied();
        view.set_fov(&mut projection, fov.filter(|&fov| fov > 0.0).unwrap_or(view.view_fov()));
        set_camera(&mut camera, viewport.is_some() && fov.is_some(), viewport.as_ref());
        if let Some(view) = view_transform
            && *transform != view
        {
            *transform = view;
        }
    }
}

pub(super) fn show_canvas_layers(view: Res<MeshView>, mut layers: Query<(&mut Camera, &CanvasLayer)>) {
    for (mut camera, CanvasLayer(slot)) in &mut layers {
        let shown = view.canvas_below.get(*slot).copied().unwrap_or(false);
        if camera.is_active != shown {
            camera.is_active = shown;
        }
    }
}

pub(super) fn place_camera(
    mut commands: Commands,
    view: Res<MeshView>,
    mut cameras: Query<
        (Entity, &mut Camera, &mut Transform, &SceneView, &mut Projection, Has<OcclusionCulling>, Has<ScreenSpaceAmbientOcclusion>),
        Without<SkyCamera>,
    >,
    mut skies: Query<(&mut Camera, &mut Transform, &SkyCamera, &mut Projection), Without<SceneView>>,
) {
    let view_fov = view.view_fov();
    let viewport = view.bevy_viewport();
    let has_sky = skies.iter().any(|(_, _, sky, _)| sky.position.is_some());
    let mut rotation = Quat::IDENTITY;
    for (entity, mut camera, mut transform, scene_view, mut projection, culled, ssao) in &mut cameras {
        view.set_fov(&mut projection, view_fov);
        set_camera(&mut camera, viewport.is_some(), viewport.as_ref());
        // GPU occlusion culling (Bevy 0.20) takes the whole depth texture for the view's: only for a view filling its
        // target (the game's full window; its size is rounded from egui points), not one beside panels (the viewer).
        let fills = view.occlusion_culling
            && viewport.as_ref().is_none_or(|v| {
                let target = camera.physical_target_size();
                v.physical_position == UVec2::ZERO && target.is_some_and(|size| (size.as_ivec2() - v.physical_size.as_ivec2()).abs().max_element() <= 2)
            });
        // It needs the depth prepass, which nothing else uses: both together.
        if fills && !culled {
            commands.entity(entity).insert((DepthPrepass, OcclusionCulling));
        } else if !fills && culled {
            if ssao {
                commands.entity(entity).remove::<OcclusionCulling>();
            } else {
                commands.entity(entity).remove::<(DepthPrepass, OcclusionCulling)>();
            }
        }
        let clear_color = if has_sky { ClearColorConfig::None } else { ClearColorConfig::Default };
        if std::mem::discriminant(&camera.clear_color) != std::mem::discriminant(&clear_color) {
            camera.clear_color = clear_color;
        }
        rotation = scene_view.rotation;
        let placed = Transform::from_translation(scene_view.position).with_rotation(rotation);
        if *transform != placed {
            *transform = placed;
        }
    }
    for (mut camera, mut transform, sky, mut projection) in &mut skies {
        view.set_fov(&mut projection, view_fov);
        set_camera(&mut camera, viewport.is_some() && sky.position.is_some(), viewport.as_ref());
        // The sky's world turns by the inverse of the `SkyZoneInfo`'s rotation before the view's, so its
        // camera looks along the zone's rotation applied to the view's.
        let (position, turn) = sky.position.unwrap_or((Vec3::ZERO, Quat::IDENTITY));
        let placed = Transform::from_translation(position).with_rotation(turn * rotation);
        if *transform != placed {
            *transform = placed;
        }
    }
}
