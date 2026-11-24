//! What the modern full-screen passes share: the views they draw in, the settings level they follow and one uniform
//! per view, carried on the camera as the pass's own component.

use std::marker::PhantomData;

use bevy::app::{App, Plugin, PostUpdate};
use bevy::camera::Camera;
use bevy::color::LinearRgba;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::ecs::entity::{Entity, EntityHashMap};
use bevy::ecs::prelude::{Commands, Component, Has, Query, Res, ResMut, Resource, With};
use bevy::ecs::system::EntityCommands;
use bevy::math::UVec2;
use bevy::render::camera::ExtractedCamera;
use bevy::render::diagnostic::RecordDiagnostics;
use bevy::render::render_resource::encase;
use bevy::render::render_resource::encase::internal::WriteInto;
use bevy::render::render_resource::binding_types;
use bevy::render::render_resource::{BindGroupLayoutEntryBuilder, SamplerBindingType, TextureSampleType};
use bevy::render::render_resource::{
    BindGroup, CommandEncoder, DynamicUniformBuffer, Extent3d, Operations, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, ShaderType, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView,
};
use bevy::render::render_phase::TrackedRenderPass;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget};
use bevy::render::RenderApp;

use crate::components::SceneView;
use p2_host::display::Settings;

/// A modern setting's level; 0 without settings.
pub fn modern_level(settings: Option<&Settings>, name: &str) -> u8 {
    settings.map_or(0, |settings| settings.modern_level(name))
}

/// On a scene view the passes draw in: active, single-sampled (they bind the prepass textures as such) and with a
/// depth prepass.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct PassView;

struct PassViewPlugin;

impl Plugin for PassViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, tag_pass_views);
    }
}

/// Extracts the pass's `T` from the views; the views are tagged once however many passes ask.
pub fn add_view_pass<T: ExtractComponent<RenderApp>>(app: &mut App) {
    if !app.is_plugin_added::<PassViewPlugin>() {
        app.add_plugins(PassViewPlugin);
    }
    app.add_plugins(ExtractComponentPlugin::<T>::default());
}

pub fn tag_pass_views(mut commands: Commands, views: Query<(Entity, &Camera, &Msaa, Has<DepthPrepass>, Option<&PassView>), With<SceneView>>) {
    for (entity, camera, msaa, depth, tagged) in &views {
        let drawn_in = camera.is_active && matches!(msaa, Msaa::Off) && depth;
        follow_view_pass(commands.entity(entity), tagged, drawn_in.then_some(PassView));
    }
}

/// Puts the pass's `wanted` on the view only when it differs from what is there, or takes the pass off.
pub fn follow_view_pass<T: Component + PartialEq>(mut view: EntityCommands, shown: Option<&T>, wanted: Option<T>) {
    match wanted {
        Some(wanted) if shown != Some(&wanted) => {
            view.insert(wanted);
        }
        None if shown.is_some() => {
            view.remove::<T>();
        }
        _ => {}
    }
}

/// `value` laid out as WGSL lays it out; empty only when memory runs out.
pub(crate) fn shader_bytes<T: ShaderType + WriteInto + ?Sized>(value: &T) -> Vec<u8> {
    let mut bytes = encase::StorageBuffer::new(Vec::new());
    bytes.write(value).map(|()| bytes.into_inner()).unwrap_or_default()
}

#[derive(Resource)]
pub struct PassUniforms<T: ShaderType + WriteInto + Send + Sync + 'static>(pub DynamicUniformBuffer<T>);

impl<T: ShaderType + WriteInto + Send + Sync + 'static> Default for PassUniforms<T> {
    fn default() -> Self {
        Self(DynamicUniformBuffer::default())
    }
}

/// Where a view's `T` sits in its `PassUniforms<T>`.
#[derive(Component)]
pub struct PassUniformOffset<T: Send + Sync + 'static>(pub u32, PhantomData<fn() -> T>);

pub fn prepare_pass_uniforms<T: Component + ShaderType + WriteInto>(
    mut commands: Commands,
    views: Query<(Entity, &T), With<ExtractedView>>,
    mut buffer: ResMut<PassUniforms<T>>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
) {
    let Some(mut writer) = buffer.0.get_writer(views.iter().len(), &render_device, &render_queue) else {
        return;
    };
    for (view, uniform) in &views {
        let offset = writer.write(uniform);
        commands.entity(view).insert(PassUniformOffset::<T>(offset, PhantomData));
    }
}

/// A pass's bind group for each view, made again only when `K`, the ids of what it holds, changes. The depth prepass
/// goes in `K` by its texture's id, as Bevy makes a new view of it every frame.
pub struct ViewBindGroups<K>(EntityHashMap<Option<(K, BindGroup)>>);

impl<K> Default for ViewBindGroups<K> {
    fn default() -> Self {
        Self(EntityHashMap::default())
    }
}

impl<K: PartialEq> ViewBindGroups<K> {
    pub fn get_or_make(&mut self, view: Entity, key: K, make: impl FnOnce() -> BindGroup) -> &BindGroup {
        let cached = self.0.entry(view).or_default();
        if cached.as_ref().is_some_and(|(held, _)| *held != key) {
            *cached = None;
        }
        &cached.get_or_insert_with(|| (key, make())).1
    }

}

fn half_size(full: UVec2) -> UVec2 {
    (full + UVec2::ONE) / 2
}

/// The size of a half-size texture over the view's viewport, `None` while the viewport is empty.
pub(crate) fn half_viewport(view: &ExtractedView) -> Option<UVec2> {
    let size = UVec2::new(view.viewport.z, view.viewport.w);
    (size.min_element() > 0).then(|| half_size(size))
}

/// A texture to march into, from Bevy's texture cache, which hands out a new one when `size` changes.
pub fn half_texture(cache: &mut TextureCache, render_device: &RenderDevice, label: &'static str, format: TextureFormat, size: UVec2) -> CachedTexture {
    cache.get(
        render_device,
        TextureDescriptor {
            label: Some(label),
            size: Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
    )
}

pub struct ScreenStep<'a> {
    pub label: &'static str,
    pub pipeline: &'a RenderPipeline,
    pub bind_group: &'a BindGroup,
}

/// A half-size march into `half`, then a full-size composite onto the frame within the camera's viewport, both bound
/// at the same dynamic `offset` and timed as one span.
pub struct HalfThenFull<'a> {
    pub span: &'static str,
    pub half: &'a TextureView,
    pub march: ScreenStep<'a>,
    pub march_blend_constant: Option<LinearRgba>,
    pub composite: ScreenStep<'a>,
    pub offset: u32,
}

impl HalfThenFull<'_> {
    pub fn draw(self, ctx: &RenderContext<'_, '_>, encoder: &mut CommandEncoder, target: &ViewTarget, camera: &ExtractedCamera) {
        let diagnostics = ctx.diagnostic_recorder();
        let diagnostics = diagnostics.as_deref();
        let span = diagnostics.time_span(encoder, self.span);
        {
            let mut pass = TrackedRenderPass::new(ctx.render_device(), encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some(self.march.label),
                color_attachments: &[Some(RenderPassColorAttachment { view: self.half, depth_slice: None, resolve_target: None, ops: Operations::default() })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            }));
            pass.set_render_pipeline(self.march.pipeline);
            pass.set_bind_group(0, self.march.bind_group, &[self.offset]);
            if let Some(constant) = self.march_blend_constant {
                pass.set_blend_constant(constant);
            }
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = TrackedRenderPass::new(ctx.render_device(), encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some(self.composite.label),
                color_attachments: &[Some(target.get_color_attachment())],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            }));
            if let Some(viewport) = camera.viewport.as_ref() {
                pass.set_camera_viewport(viewport);
            }
            pass.set_render_pipeline(self.composite.pipeline);
            pass.set_bind_group(0, self.composite.bind_group, &[self.offset]);
            pass.draw(0..3, 0..1);
        }
        span.end(encoder);
    }
}

/// A filtered colour texture and the sampler that filters it, as a pass's bind group reads them.
pub(crate) fn filtered_texture_and_sampler() -> (BindGroupLayoutEntryBuilder, BindGroupLayoutEntryBuilder) {
    (binding_types::texture_2d(TextureSampleType::Float { filterable: true }), binding_types::sampler(SamplerBindingType::Filtering))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::change_detection::DetectChanges;
    use bevy::ecs::world::World;

    #[test]
    fn half_size_covers_odd_sides() {
        assert_eq!(half_size(UVec2::new(1921, 1080)), UVec2::new(961, 540));
        assert_eq!(half_size(UVec2::ONE), UVec2::ONE);
    }

    #[derive(Component, Debug, Clone, Copy, PartialEq)]
    struct Pass(u8);

    /// Whether the follow wrote the pass this step.
    fn follow(world: &mut World, view: Entity, wanted: Option<Pass>) -> bool {
        world.clear_trackers();
        let shown = world.get::<Pass>(view).copied();
        follow_view_pass(world.commands().entity(view), shown.as_ref(), wanted);
        world.flush();
        world.entity(view).get_ref::<Pass>().is_some_and(|pass| pass.is_changed())
    }

    #[test]
    fn a_view_pass_is_written_only_when_it_changes_and_taken_off_when_unwanted() {
        let mut world = World::new();
        let view = world.spawn_empty().id();
        assert!(follow(&mut world, view, Some(Pass(1))));
        assert!(!follow(&mut world, view, Some(Pass(1))), "the same pass is not written again");
        assert!(follow(&mut world, view, Some(Pass(2))));
        assert_eq!(world.get::<Pass>(view), Some(&Pass(2)));
        follow(&mut world, view, None);
        assert_eq!(world.get::<Pass>(view), None);
        follow(&mut world, view, None);
        assert_eq!(world.get::<Pass>(view), None);
    }
}
