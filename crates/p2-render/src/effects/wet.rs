use bevy::app::{App, Plugin, Update};
use bevy::asset::{AssetId, Assets};
use bevy::ecs::prelude::{Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::time::Time;
use p2_bevy_assets::library::WetSource;
use p2_render_cpu::WetSim;

/// Simulation steps per second; retail's rate is (unverified).
const STEPS_PER_SECOND: f32 = 30.0;

pub(crate) struct WetPlugin;

impl Plugin for WetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WetTextures>().add_systems(Update, animate_wet.in_set(crate::RenderFollowSystems));
    }
}

struct Wet {
    image: AssetId<Image>,
    sim: WetSim,
    colors: [[u8; 4]; 256],
    pending: f32,
}

#[derive(Resource, Default)]
pub struct WetTextures(Vec<Wet>);

impl WetTextures {
    pub fn add(&mut self, image: AssetId<Image>, source: WetSource) {
        let mut colors = [[0; 4]; 256];
        for (slot, &color) in colors.iter_mut().zip(&source.palette) {
            *slot = color;
        }
        if source.texture.masked {
            colors[0] = [0; 4];
        }
        self.0.push(Wet { image, sim: source.sim, colors, pending: 0.0 });
    }
}

pub fn animate_wet(time: Res<Time>, facts: Res<crate::world::frame_facts::FrameFacts>, mut wet: ResMut<WetTextures>, mut images: ResMut<Assets<Image>>) {
    let delta = if facts.paused { 0.0 } else { time.delta_secs() };
    let interval = 1.0 / STEPS_PER_SECOND;
    wet.0.retain_mut(|wet| {
        wet.pending = (wet.pending + delta).min(interval * 2.0);
        if wet.pending < interval {
            return images.contains(wet.image);
        }
        let Some(mut image) = images.get_mut(wet.image) else { return false };
        wet.pending -= interval;
        wet.sim.step();
        let indices = wet.sim.indices();
        if let Some(data) = image.data.as_mut().filter(|data| data.len() >= indices.len() * 4) {
            for (texel, &index) in data.as_chunks_mut::<4>().0.iter_mut().zip(indices) {
                texel.copy_from_slice(&wet.colors[usize::from(index)]);
            }
        }
        true
    });
}
