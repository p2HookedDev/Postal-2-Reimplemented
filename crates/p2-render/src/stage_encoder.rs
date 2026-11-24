//! Our own passes of one stage record into one command encoder, handed on after them in the stage's place: the frame's
//! submit checks each command buffer against every live texture, and each system with its own encoder made one more.

use std::marker::PhantomData;

use bevy::app::App;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::prelude::{ResMut, Resource};
use bevy::ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy::render::render_resource::{CommandEncoder, CommandEncoderDescriptor};
use bevy::render::renderer::{PendingCommandBuffers, RenderContext, RenderDevice};
use bevy::render::{Render, RenderApp, RenderSystems};

use crate::MainPassExtraSystems;

/// The lamp atlas, the movers' tiles and the liquid layer, drawn before the cameras' passes.
pub(crate) struct Prepared;
/// The decal bins and the lamp shadow mask, between the depth prepass and the main pass.
pub(crate) struct BeforeMain;
/// The screen passes between the opaque and the transparent pass.
pub(crate) struct AfterOpaque;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BeforeMainPasses;

#[derive(Resource)]
pub(crate) struct StageEncoder<S: Send + Sync + 'static> {
    encoder: Option<CommandEncoder>,
    stage: PhantomData<S>,
}

impl<S: Send + Sync + 'static> Default for StageEncoder<S> {
    fn default() -> Self {
        Self { encoder: None, stage: PhantomData }
    }
}

impl<S: Send + Sync + 'static> StageEncoder<S> {
    pub(crate) fn encoder(&mut self, device: &RenderDevice) -> &mut CommandEncoder {
        self.encoder.get_or_insert_with(|| device.create_command_encoder(&CommandEncoderDescriptor { label: Some(std::any::type_name::<S>()) }))
    }
}

/// Called by each plugin with a pass of a stage; registers the stages once.
pub(crate) fn build(app: &mut App) {
    let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
    if render.world().contains_resource::<StageEncoder<Prepared>>() {
        return;
    }
    render
        .init_resource::<StageEncoder<Prepared>>()
        .init_resource::<StageEncoder<BeforeMain>>()
        .init_resource::<StageEncoder<AfterOpaque>>()
        .configure_sets(Core3d, BeforeMainPasses.after(Core3dSystems::Prepass).before(Core3dSystems::MainPass))
        .add_systems(Render, submit_prepared.in_set(RenderSystems::PrepareBindGroups))
        .add_systems(
            Core3d,
            (
                submit_stage::<BeforeMain>.after(BeforeMainPasses).before(Core3dSystems::MainPass),
                submit_stage::<AfterOpaque>
                    .in_set(Core3dSystems::MainPass)
                    .after(main_opaque_pass_3d)
                    .after(MainPassExtraSystems::Bounce)
                    .after(MainPassExtraSystems::FireVolume)
                    .after(MainPassExtraSystems::LampFog)
                    .after(MainPassExtraSystems::GpuParticles)
                    .after(MainPassExtraSystems::Liquids)
                    .before(main_transparent_pass_3d),
            ),
        );
}

/// Handed on unfinished, so Bevy encodes it beside the frame's other buffers off the render thread.
fn submit_stage<S: Send + Sync + 'static>(mut stage: ResMut<StageEncoder<S>>, mut ctx: RenderContext) {
    if let Some(mut encoder) = stage.encoder.take() {
        std::mem::swap(ctx.command_encoder(), &mut encoder);
    }
}

fn submit_prepared(mut stage: ResMut<StageEncoder<Prepared>>, mut pending: ResMut<PendingCommandBuffers>) {
    if let Some(encoder) = stage.encoder.take() {
        pending.push_encoder(encoder, "prepared passes");
    }
}
