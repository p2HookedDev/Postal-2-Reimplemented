//! Texture settings act at once. Pixels have left the main world, so only the render world's samplers change; prepared
//! materials and light map slabs keep the old sampler until prepared again.

use bevy::app::{App, Plugin, Update};
use bevy::asset::{Asset, AssetId, Assets};
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::prelude::{Entity, Res, ResMut, Resource, World};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::Local;
use bevy::image::{Image, ImageSamplerDescriptor};
use bevy::mesh::Mesh3d;
use bevy::pbr::{Lightmap, StandardMaterial};
use bevy::render::render_asset::RenderAssets;
use bevy::render::renderer::RenderDevice;
use bevy::render::texture::GpuImage;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
use p2_bevy_assets::TextureQuality;

use crate::display::Settings;
use crate::render::actor_light::ActorLitMaterial;
use crate::render::blended::{BlendedMaterial, TerrainLayerMaterial};
use crate::render::lightmapped::LightmappedMaterial;
use crate::render::mesh_light::MeshLitMaterial;
use crate::render::particle_material::ParticleMaterial;
use crate::render::terrain_material::TerrainMaterial;

pub struct SamplerRefreshPlugin;

impl Plugin for SamplerRefreshPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Settings>()
            .init_resource::<ChangedSamplers>()
            .add_systems(Update, refresh_samplers.after(crate::display::apply_texture_quality));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(ExtractSchedule, swap_samplers);
        }
    }
}

#[derive(Resource, Default)]
struct ChangedSamplers(Vec<(AssetId<Image>, ImageSamplerDescriptor)>);

#[derive(Default)]
struct Refresh {
    shown: TextureQuality,
    lightmaps: Vec<(Entity, Lightmap)>,
}

fn refresh_samplers(world: &mut World, mut refresh: Local<Refresh>) {
    world.resource_mut::<ChangedSamplers>().0.clear();
    for (entity, lightmap) in refresh.lightmaps.drain(..) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(lightmap);
            // A light map coming back does not make the renderer build the mesh's pipeline again; a changed mesh does.
            // Without it the mesh is drawn with the pipeline built while it had no light map, which the GPU refuses.
            if let Some(mut mesh) = entity.get_mut::<Mesh3d>() {
                mesh.set_changed();
            }
        }
    }
    let settings = world.resource_ref::<Settings>();
    if !settings.is_changed() {
        return;
    }
    let quality = settings.texture_quality();
    if quality == refresh.shown {
        return;
    }
    refresh.shown = quality;

    let mut images = world.resource_mut::<Assets<Image>>();
    let changed: Vec<_> = images.iter().filter_map(|(id, image)| Some((id, quality.refreshed_sampler(image)?))).collect();
    for (id, descriptor) in &changed {
        if let Some(image) = images.get_mut_untracked(*id) {
            image.sampler = bevy::image::ImageSampler::Descriptor(descriptor.clone());
        }
    }
    world.resource_mut::<ChangedSamplers>().0 = changed;

    let off = f32::from(u8::from(!quality.detail_textures));
    prepare_again::<StandardMaterial>(world, |_| {});
    prepare_again::<MeshLitMaterial>(world, |material| material.extension.layers.detail.w = off);
    prepare_again::<LightmappedMaterial>(world, |material| material.extension.layers.detail.w = off);
    prepare_again::<ActorLitMaterial>(world, |material| material.extension.layers.detail.w = off);
    prepare_again::<BlendedMaterial>(world, |material| material.extension.layers.detail.w = off);
    prepare_again::<TerrainLayerMaterial>(world, |material| material.extension.layers.detail.w = off);
    prepare_again::<TerrainMaterial>(world, |_| {});
    prepare_again::<ParticleMaterial>(world, |_| {});

    refresh.lightmaps = world.query::<(Entity, &Lightmap)>().iter(world).map(|(entity, lightmap)| (entity, lightmap.clone())).collect();
    for (entity, _) in &refresh.lightmaps {
        world.entity_mut(*entity).remove::<Lightmap>();
    }
}

fn prepare_again<M: Asset>(world: &mut World, change: impl Fn(&mut M)) {
    let Some(mut store) = world.get_resource_mut::<Assets<M>>() else { return };
    let ids: Vec<_> = store.ids().collect();
    for id in ids {
        if let Some(material) = store.get_mut(id) {
            change(material.into_inner());
        }
    }
}

fn swap_samplers(changed: Extract<Res<ChangedSamplers>>, device: Res<RenderDevice>, mut images: ResMut<RenderAssets<GpuImage>>) {
    for (id, descriptor) in &changed.0 {
        if let Some(image) = images.get_mut(*id) {
            image.sampler = device.create_sampler(&descriptor.as_wgpu());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::{AssetApp, AssetEvent, AssetPlugin};
    use bevy::ecs::message::Messages;
    use bevy::image::{ImageAddressMode, ImageSampler};
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    fn image(sampler: ImageSampler, mips: u32) -> Image {
        let size = Extent3d { width: 4, height: 4, depth_or_array_layers: 1 };
        let mut image = Image::new_uninit(size, TextureDimension::D2, TextureFormat::Rgba8Unorm, bevy::asset::RenderAssetUsages::RENDER_WORLD);
        image.texture_descriptor.mip_level_count = mips;
        image.sampler = sampler;
        image
    }

    #[test]
    fn a_detail_change_refreshes_samplers_and_materials() {
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()));
        app.init_asset::<Image>().init_asset::<StandardMaterial>();
        app.init_resource::<Settings>().init_resource::<ChangedSamplers>();
        app.add_systems(Update, refresh_samplers);

        let wrap = [ImageAddressMode::ClampToEdge, ImageAddressMode::Repeat];
        let made = TextureQuality::default().sampler(1, 4, wrap);
        let mut images = app.world_mut().resource_mut::<Assets<Image>>();
        let world_image = images.add(image(made, 4));
        let other = images.add(image(ImageSampler::Descriptor(ImageSamplerDescriptor::linear()), 1));
        let material = app.world_mut().resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
        app.update();
        app.world_mut().resource_mut::<Messages<AssetEvent<Image>>>().clear();
        app.world_mut().resource_mut::<Messages<AssetEvent<StandardMaterial>>>().clear();

        *app.world_mut().resource_mut::<Settings>() =
            Settings::read(|query| if query.ends_with("TextureDetailWorld") { "Low".to_owned() } else { String::new() });
        app.update();

        let changed = &app.world().resource::<ChangedSamplers>().0;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, world_image.id());
        assert_eq!(changed[0].1.lod_min_clamp, 2.0);
        assert_eq!((changed[0].1.address_mode_u, changed[0].1.address_mode_v), (wrap[0], wrap[1]));
        let images = app.world().resource::<Assets<Image>>();
        let Some(ImageSampler::Descriptor(kept)) = images.get(other.id()).map(|image| &image.sampler) else { panic!("no image") };
        assert_eq!(kept.lod_min_clamp, 0.0);
        let events = app.world().resource::<Messages<AssetEvent<StandardMaterial>>>();
        assert!(events.iter_current_update_messages().any(|event| event.is_modified(material.id())));
        assert!(!app.world().resource::<Messages<AssetEvent<Image>>>().iter_current_update_messages().any(|event| event.is_modified(world_image.id())));

        app.update();
        assert!(app.world().resource::<ChangedSamplers>().0.is_empty());
    }

    #[test]
    fn a_light_map_put_back_marks_its_mesh_changed() {
        #[derive(Resource, Default)]
        struct ChangedMeshes(usize);
        fn count(changed: bevy::ecs::prelude::Query<(), bevy::ecs::prelude::Changed<Mesh3d>>, mut count: ResMut<ChangedMeshes>) {
            count.0 = changed.iter().count();
        }
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()));
        app.init_asset::<Image>();
        app.init_resource::<Settings>().init_resource::<ChangedSamplers>().init_resource::<ChangedMeshes>();
        app.add_systems(Update, (refresh_samplers, count).chain());
        let image = app.world_mut().resource_mut::<Assets<Image>>().add(image(ImageSampler::Default, 1));
        let lit = app.world_mut().spawn((Mesh3d::default(), Lightmap { image, ..Default::default() })).id();
        app.update();
        app.update();
        assert_eq!(app.world().resource::<ChangedMeshes>().0, 0);

        *app.world_mut().resource_mut::<Settings>() =
            Settings::read(|query| if query.ends_with("TextureDetailWorld") { "Low".to_owned() } else { String::new() });
        app.update();
        assert!(app.world().entity(lit).get::<Lightmap>().is_none());
        assert_eq!(app.world().resource::<ChangedMeshes>().0, 0);

        app.update();
        assert!(app.world().entity(lit).get::<Lightmap>().is_some());
        assert_eq!(app.world().resource::<ChangedMeshes>().0, 1);
    }
}
