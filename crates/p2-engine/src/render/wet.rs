use bevy::asset::{AssetId, Assets};
use bevy::ecs::prelude::{Res, ResMut, Resource};
use bevy::image::Image;
use bevy::time::Time;
use p2_render_cpu::WetSim;

use crate::library::WetSource;

/// Simulation steps per second; retail's rate is `(unverified)`.
const STEPS_PER_SECOND: f32 = 30.0;

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

pub fn animate_wet(time: Res<Time>, mut wet: ResMut<WetTextures>, mut images: ResMut<Assets<Image>>) {
    let delta = time.delta_secs();
    let interval = 1.0 / STEPS_PER_SECOND;
    wet.0.retain_mut(|wet| {
        // Only a step touches the image: `get_mut` marks it changed, and a changed image is uploaded again.
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
