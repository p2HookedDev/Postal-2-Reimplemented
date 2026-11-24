use super::*;
use bevy::math::Mat4;
use bevy::camera::visibility::RenderLayers;
use crate::components::{OVERLAY_LAYERS, SUN_VIEW_LAYER};
use crate::overlay_route::overlay_routes;
use bevy::camera::{CameraOutputMode, RenderTarget};

/// The Bevy camera rotation of an Unreal view rotation (pitch, yaw, roll; 65536 a turn): Unreal looks along X
/// with Z up and Y right, a Bevy camera along -Z with Y up and X right, Y and Z swapped.
pub fn view_rotation(rotation: Rotator) -> Quat {
    let matrix = rotator_matrix(rotation);
    let axis = |i: usize| {
        let v = matrix.col(i);
        Vec3::new(v.x, v.z, v.y)
    };
    let (forward, right, up) = (axis(0), axis(1), axis(2));
    Quat::from_mat3(&Mat3::from_cols(right, up, -forward)).normalize()
}


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

/// Runs after the frame is presented, so a slot's camera turns on and takes its field of view in the same frame
/// its models move to its layer (a kick's foot takes slot 0 and the weapon slot 1), not a frame later.
pub(super) struct ViewCameraPlugin;

impl Plugin for ViewCameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, place_camera.in_set(SceneViewSystems::Place))
            .add_systems(PostUpdate, show_canvas_layers.after(EguiPostUpdateSet::EndPass))
            .add_systems(PostUpdate, place_overlay_cameras.in_set(SceneViewSystems::Overlays).before(CameraUpdateSystems).before(bevy::transform::TransformSystems::Propagate));
    }
}

pub(super) fn hud_may_carry(overlay_target: &RenderTarget, viewport: Option<&Viewport>, hud_size: Option<UVec2>) -> bool {
    !matches!(overlay_target, RenderTarget::Image(_)) && viewport.is_some_and(|v| v.physical_position == UVec2::ZERO && Some(v.physical_size) == hud_size)
}

fn set_hud_output(camera: &mut Mut<Camera>, routed: &[usize]) {
    let carries = !routed.is_empty();
    if matches!(camera.clear_color, ClearColorConfig::None) == carries {
        return;
    }
    let (clear_color, output_mode) = if carries { (ClearColorConfig::None, CameraOutputMode::default()) } else { hud_alone() };
    camera.clear_color = clear_color;
    camera.output_mode = output_mode;
}

fn place_overlay_cameras(
    view: Res<MeshView>,
    hud: Res<HudView>,
    facts: Res<FrameFacts>,
    look: Res<crate::components::OverlayLook>,
    cameras: Query<&Transform, (With<SceneView>, Without<OverlayCamera>)>,
    mut overlays: Query<(&mut Camera, &mut Transform, &mut Projection, &mut RenderLayers, &OverlayCamera, &RenderTarget, Has<HudCamera>), Without<SceneView>>,
) {
    let viewport = view.bevy_viewport();
    let view_transform = look.0.or_else(|| cameras.iter().next().copied());
    let fov_of = |slot: usize| hud.overlays.get(slot).copied().filter(|&fov| fov > 0.0).unwrap_or(view.view_fov(&facts));
    let shown = hud.overlays.len().min(OVERLAY_LAYERS.len());
    let overlay_target = overlays.iter().find(|(.., is_hud)| !is_hud).map(|(.., target, _)| target.clone());
    let hud_size = overlays.iter().find(|(.., is_hud)| *is_hud).and_then(|(camera, ..)| camera.physical_target_size());
    let routes = overlay_routes(shown, overlay_target.is_some_and(|target| hud_may_carry(&target, viewport.as_ref(), hud_size)));
    for (mut camera, mut transform, mut projection, mut layers, OverlayCamera(slot), _, is_hud) in &mut overlays {
        let routed: Vec<usize> = (0..shown).filter(|&other| routes[other] == *slot).collect();
        view.set_fov(&mut projection, routed.last().map_or(view.view_fov(&facts), |&last| fov_of(last)));
        if is_hud {
            set_hud_output(&mut camera, &routed);
        } else {
            set_camera(&mut camera, viewport.is_some() && !routed.is_empty(), viewport.as_ref());
        }
        let own = OVERLAY_LAYERS.get(*slot).map_or_else(RenderLayers::none, |&layer| RenderLayers::layer(layer));
        let mut seen = routed.iter().filter_map(|&other| OVERLAY_LAYERS.get(other)).fold(own, |seen, &layer| seen.with(layer));
        if layers.intersects(&RenderLayers::layer(SUN_VIEW_LAYER)) {
            seen = seen.with(SUN_VIEW_LAYER);
        }
        layers.set_if_neq(seen);
        if let Some(placed) = view_transform
            && *transform != placed
        {
            *transform = placed;
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
        // GPU occlusion culling takes the whole depth texture as the view's: only for a view filling its target
        // (the game's full window), not one beside panels (the viewer).
        let fills = view.occlusion_culling
            && viewport.as_ref().is_none_or(|v| {
                let target = camera.physical_target_size();
                v.physical_position == UVec2::ZERO && target.is_some_and(|size| (size.as_ivec2() - v.physical_size.as_ivec2()).abs().max_element() <= 2)
            });
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
    if let (Some(eye), Some((sky_eye, turn))) = (eye, shown.sky_place) {
        let placed = Transform::from_matrix(Mat4::from_translation(eye) * Mat4::from_quat(turn.inverse()) * Mat4::from_translation(shown.origin - sky_eye));
        for mut transform in &mut sky_roots {
            transform.set_if_neq(placed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::ImageRenderTarget;
    use bevy::ecs::world::World;

    #[test]
    fn the_hud_carries_only_where_the_overlay_would_draw_the_same_pixels() {
        let size = UVec2::new(1920, 1080);
        let full = Viewport { physical_size: size, ..Default::default() };
        let window = RenderTarget::default();
        assert!(hud_may_carry(&window, Some(&full), Some(size)));
        let ramped = RenderTarget::Image(ImageRenderTarget { handle: Handle::default(), scale_factor: 1.0 });
        assert!(!hud_may_carry(&ramped, Some(&full), Some(size)));
        assert!(!hud_may_carry(&window, Some(&full), Some(UVec2::new(2560, 1080))));
        assert!(!hud_may_carry(&window, Some(&Viewport { physical_position: UVec2::new(200, 0), ..full }), Some(size)));
        assert!(!hud_may_carry(&window, None, Some(size)));
    }

    #[test]
    fn the_hud_draws_on_the_frame_only_while_it_carries() {
        let mut world = World::new();
        let (clear_color, output_mode) = hud_alone();
        let hud = world.spawn(Camera { clear_color, output_mode, ..Default::default() }).id();
        for (routed, loads) in [(&[0usize][..], true), (&[], false)] {
            if let Some(mut camera) = world.get_mut::<Camera>(hud) {
                set_hud_output(&mut camera, routed);
            }
            let camera = world.get::<Camera>(hud).expect("spawned");
            assert_eq!(matches!(camera.clear_color, ClearColorConfig::None), loads);
            assert_eq!(matches!(camera.output_mode, CameraOutputMode::Write { blend_state: None, .. }), loads);
        }
    }
}
