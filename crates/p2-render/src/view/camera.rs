use super::*;
use bevy::math::Mat4;

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


/// `DefaultTexMipBias` as `Postal2.ini` ships it.
const DEFAULT_MIP_BIAS: f32 = -0.5;

pub(super) fn plain_output() -> (Msaa, Tonemapping, DebandDither, MipBias) {
    (Msaa::Off, Tonemapping::None, DebandDither::Disabled, MipBias(DEFAULT_MIP_BIAS))
}

#[derive(SystemParam)]
pub(super) struct SceneCameras<'w, 's> {
    pub(super) views: Query<'w, 's, &'static mut SceneView>,
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
pub(super) struct ViewCameraPlugin;

impl Plugin for ViewCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, place_camera.in_set(SceneViewSystems::Place))
            // After the HUD paint (egui's pass), so a layer shows in the frame it was painted.
            .add_systems(PostUpdate, show_canvas_layers.after(EguiPostUpdateSet::EndPass))
            .add_systems(PostUpdate, place_overlay_cameras.in_set(SceneViewSystems::Overlays).before(CameraUpdateSystems).before(bevy::transform::TransformSystems::Propagate));
    }
}

fn place_overlay_cameras(
    view: Res<MeshView>,
    hud: Res<HudView>,
    facts: Res<FrameFacts>,
    look: Res<crate::components::OverlayLook>,
    cameras: Query<&Transform, (With<SceneView>, Without<OverlayCamera>)>,
    mut overlays: Query<(&mut Camera, &mut Transform, &mut Projection, &OverlayCamera), Without<SceneView>>,
) {
    let viewport = view.bevy_viewport();
    let view_transform = look.0.or_else(|| cameras.iter().next().copied());
    for (mut camera, mut transform, mut projection, OverlayCamera(slot)) in &mut overlays {
        let fov = hud.overlays.get(*slot).copied();
        view.set_fov(&mut projection, fov.filter(|&fov| fov > 0.0).unwrap_or(view.view_fov(&facts)));
        set_camera(&mut camera, viewport.is_some() && fov.is_some(), viewport.as_ref());
        if let Some(view) = view_transform
            && *transform != view
        {
            *transform = view;
        }
    }
}

fn show_canvas_layers(view: Res<MeshView>, mut layers: Query<(&mut Camera, &CanvasLayer)>) {
    for (mut camera, CanvasLayer(slot)) in &mut layers {
        let shown = view.canvas_below.get(*slot).copied().unwrap_or(false);
        if camera.is_active != shown {
            camera.is_active = shown;
        }
    }
}

fn place_camera(
    mut commands: Commands,
    view: Res<MeshView>,
    facts: Res<FrameFacts>,
    mut cameras: Query<
        (Entity, &mut Camera, &mut Transform, &SceneView, &mut Projection, Has<OcclusionCulling>, Has<ScreenSpaceAmbientOcclusion>),
        Without<SkyRoot>,
    >,
    mut sky_roots: Query<&mut Transform, (With<SkyRoot>, Without<SceneView>)>,
    shown: Res<ShownScene>,
) {
    let view_fov = view.view_fov(&facts);
    let viewport = view.bevy_viewport();
    let mut eye = None;
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
        eye = Some(scene_view.position);
        let placed = Transform::from_translation(scene_view.position).with_rotation(scene_view.rotation);
        if *transform != placed {
            *transform = placed;
        }
    }
    // The sky zone's geometry at p shows at eye + turn^-1 (p - sky eye): no parallax, turned by the zone's rotation.
    if let (Some(eye), Some((sky_eye, turn))) = (eye, shown.sky_place) {
        let placed = Transform::from_matrix(Mat4::from_translation(eye) * Mat4::from_quat(turn.inverse()) * Mat4::from_translation(shown.origin - sky_eye));
        for mut transform in &mut sky_roots {
            transform.set_if_neq(placed);
        }
    }
}
