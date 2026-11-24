use std::collections::{HashMap, HashSet};

use bevy::asset::{Assets, Handle};
use bevy::ecs::prelude::{Commands, Resource};
use bevy::image::Image;
use p2_bevy_assets::library::{Game, SourceLayers, TextureDecode, export_reference};
use p2_package::PackageId;
use rustc_hash::FxHashMap;

use super::SwitchImage;
use crate::scene::{MaterialImages, SectionView};
use crate::view::{SwitchedLook, rebind_materials, restyle_switched};

#[derive(Resource, Default)]
pub struct MaterialSwitches {
    images: HashMap<(PackageId, usize), Vec<SwitchImage>>,
    targets: FxHashMap<(PackageId, usize), Option<SwitchTarget>>,
}

impl Extend<((PackageId, usize), SwitchImage)> for MaterialSwitches {
    fn extend<I: IntoIterator<Item = ((PackageId, usize), SwitchImage)>>(&mut self, switches: I) {
        for (switch, image) in switches {
            self.images.entry(switch).or_default().push(image);
        }
    }
}

struct SwitchTarget {
    image: Image,
    now: MaterialImages,
    size: [f32; 2],
    specular_size: [f32; 2],
}

pub fn switch_materials(game: &mut Game, switches: &mut MaterialSwitches, images: &mut Assets<Image>, commands: &mut Commands, changes: Vec<(String, Option<String>)>) {
    if switches.images.is_empty() {
        return;
    }
    let (mut switched, mut looks) = (HashSet::new(), HashMap::new());
    for (switch, material) in changes {
        let Some(key) = game.export_by_path(&switch) else { continue };
        let Some(surfaces) = switches.images.get_mut(&key) else { continue };
        let Some((package, export)) = material.and_then(|path| game.export_by_path(&path)) else { continue };
        let target = switches.targets.entry((package, export)).or_insert_with(|| switch_target(game, images, package, export));
        let Some(SwitchTarget { image, now, size, specular_size }) = target else { continue };
        for surface in surfaces {
            let Some(mut drawn) = images.get_mut(surface.image) else { continue };
            *drawn = image.clone();
            switched.insert(surface.image);
            if surface.whole {
                let was = MaterialImages { state: std::mem::replace(&mut surface.shown, now.state.clone()), ..now.clone() };
                let (was, now) = (SectionView::textured(Handle::default(), Some(was)), SectionView::textured(Handle::default(), Some(now.clone())));
                looks.insert(surface.image, SwitchedLook { was, now, size: *size, specular_size: *specular_size });
            }
        }
    }
    if !switched.is_empty() {
        commands.queue(move |world: &mut bevy::ecs::world::World| {
            rebind_materials(world, &switched);
            restyle_switched(world, &looks);
        });
    }
}

fn switch_target(game: &mut Game, images: &mut Assets<Image>, package: PackageId, export: usize) -> Option<SwitchTarget> {
    let (source, state) = game.material_source(package, export_reference(export))?;
    let SourceLayers { mut image, glow, .. } = game.source_layers(source, TextureDecode::Keep)?;
    // Uploaded as a new texture with all its mips: Bevy rewrites a texture of the same descriptor with
    // `COPY_DST` in place, mip 0 only, so the smaller mips kept the old picture past some distance from the
    // camera. Without `COPY_DST` the descriptor differs; wgpu adds it for the upload itself.
    image.texture_descriptor.usage.remove(bevy::render::render_resource::TextureUsages::COPY_DST);
    let image_size = |image: &Image| image.size().as_vec2().to_array();
    let (size, specular) = (image_size(&image), game.specular_layer(source));
    let specular_size = specular.as_ref().map_or([1.0; 2], |(specular, _)| image_size(specular));
    let now = MaterialImages {
        image: Handle::default(),
        glow: glow.map(|glow| images.add(glow)),
        detail: game.detail_image(source).map(|detail| images.add(detail)),
        specular: specular.map(|(specular, params)| (images.add(specular), params)),
        state,
    };
    Some(SwitchTarget { image, now, size, specular_size })
}

#[cfg(test)]
mod tests {
    use bevy::ecs::world::{CommandQueue, World};
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    use bevy::asset::RenderAssetUsages;
    use p2_assets::RenderState;
    use p2_bevy_assets::library::Library;

    use super::*;

    #[test]
    fn a_switch_rewrites_its_image_and_queues_the_restyle() {
        let mut package = p2_test_package::PackageBuilder::new("Switches");
        package.class("Switch", None);
        package.class("Wall", None);
        let system = package.write_to_temp("render-switch/System").unwrap();
        std::fs::write(system.join("Default.ini"), "[Core.System]\nPaths=*.u\n").unwrap();
        let mut library = Library::new(None);
        let (game, _) = library.open(system.parent().unwrap(), "System/Switches.u").unwrap();
        let wall = game.export_by_path("Switches.Wall").unwrap();
        let mut images = Assets::<Image>::default();
        let drawn = images.add(Image::default());
        let mut switches = MaterialSwitches::default();
        switches.extend([(game.export_by_path("Switches.Switch").unwrap(), SwitchImage { image: drawn.id(), whole: true, shown: RenderState::default() })]);
        let size = Extent3d { width: 2, height: 2, depth_or_array_layers: 1 };
        let picture = Image::new_fill(size, TextureDimension::D2, &[255, 0, 0, 255], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
        let now = MaterialImages { image: Handle::default(), glow: None, detail: None, specular: None, state: RenderState::default() };
        switches.targets.insert(wall, Some(SwitchTarget { image: picture, now, size: [2.0; 2], specular_size: [1.0; 2] }));
        let world = World::new();
        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        switch_materials(game, &mut switches, &mut images, &mut commands, vec![("Switches.Switch".into(), Some("Switches.Wall".into()))]);
        assert_eq!(images.get(&drawn).map(Image::size), Some(bevy::math::UVec2::splat(2)));
        assert!(!queue.is_empty());
    }
}
