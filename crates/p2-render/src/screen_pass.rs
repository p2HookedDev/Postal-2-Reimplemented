//! What the modern full-screen passes share: the settings level they follow and one uniform per view, carried on the
//! camera as the pass's own component.

use std::marker::PhantomData;

use bevy::ecs::entity::Entity;
use bevy::ecs::prelude::{Commands, Component, Query, Res, ResMut, Resource, With};
use bevy::render::render_resource::encase::internal::WriteInto;
use bevy::render::render_resource::{DynamicUniformBuffer, ShaderType};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::view::ExtractedView;

use p2_host::display::Settings;

/// A modern setting's level; 0 without settings.
pub fn modern_level(settings: Option<&Settings>, name: &str) -> u8 {
    settings.map_or(0, |settings| settings.modern_level(name))
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
