//! The cached sun levels as records and tiles of one image of our own: tiles of the shadow map's size, a few columns
//! wide, and a row of records under them that `sun_levels.wesl` reads.

use bevy::app::{App, Plugin};
use bevy::asset::{AssetId, Assets, Handle, uuid_handle};
use bevy::camera::visibility::{InheritedVisibility, RenderLayers};
use bevy::ecs::prelude::{Query, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::{Mat4, Vec3};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MeshMaterial3d, StandardMaterial};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::TextureFormat;
use bevy::render::renderer::RenderDevice;
use bevy::render::settings::WgpuFeatures;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
use bevy::shader::Shader;
use bevy::transform::components::GlobalTransform;

use crate::actor_light::HardwareLighting;
use crate::components::STATIC_CASTER_LAYER;
use crate::lamps::atlas::new_atlas;
use crate::lamps::{MaterialAtlas, MaterialSlot, follow_atlas};
use crate::lightmapped::BakedLight;
use crate::mesh_light::DynamicLit;
use crate::modern::StillProp;
use crate::shaders::add_shaders;
use crate::sun_cache::SunCacheStale;

pub(crate) const LEVEL_CASTER_SHADER: Handle<Shader> = uuid_handle!("e4a1c7d2-58b3-4f69-8c0e-2d71b9f4a635");

/// Texels before the first record: the level count.
pub const HEADER_TEXELS: usize = 1;
/// The matrix in 0 to 15 by rows, texel size 16, kind 17, direction to the light 18 to 20, tile corner 21 and 22,
/// tile side 23. Matches `sun_levels.wesl`.
pub const RECORD_TEXELS: usize = 24;
const LEAST_COLUMNS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SunLevelKind {
    Cached,
    /// Looks straight down and tells a room from open sky.
    Sky,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunLevelRecord {
    pub kind: SunLevelKind,
    pub clip_from_world: Mat4,
    pub texel_size: f32,
    pub direction_to_light: Vec3,
    /// The level's view as Bevy would build its cascade view; not in the record row.
    pub world_from_view: Mat4,
    pub clip_from_view: Mat4,
}

/// Ordered from the finest level, so a level keeps its tile while the set of levels stays the same.
#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct SunLevelFrame {
    pub records: Vec<SunLevelRecord>,
    /// The one level drawn this frame, if any.
    pub drawing: Option<usize>,
    pub map_size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasLayout {
    pub columns: u32,
    pub tiles: u32,
    pub tile_size: u32,
}

impl AtlasLayout {
    /// The fewest columns that keep the image within the device's limit; `None` when no count does.
    pub fn choose(tiles: u32, tile_size: u32, max_dimension: u32) -> Option<Self> {
        let record_width = (HEADER_TEXELS + tiles as usize * RECORD_TEXELS) as u32;
        (LEAST_COLUMNS..=tiles.max(LEAST_COLUMNS))
            .map(|columns| Self { columns, tiles, tile_size })
            .find(|layout| {
                let [width, height] = layout.size();
                width <= max_dimension && height <= max_dimension && width >= record_width
            })
    }

    pub fn rows(&self) -> u32 {
        self.tiles.div_ceil(self.columns)
    }

    /// One record row under the tiles.
    pub fn size(&self) -> [u32; 2] {
        [self.columns * self.tile_size, self.rows() * self.tile_size + 1]
    }

    pub fn record_row(&self) -> u32 {
        self.rows() * self.tile_size
    }

    pub fn tile_corner(&self, tile: u32) -> [u32; 2] {
        [tile % self.columns * self.tile_size, tile / self.columns * self.tile_size]
    }

    /// The record row's texels, one float each.
    pub fn record_texels(&self, records: &[SunLevelRecord]) -> Vec<f32> {
        let mut texels = Vec::with_capacity(HEADER_TEXELS + records.len() * RECORD_TEXELS);
        texels.push(records.len() as f32);
        for (tile, record) in records.iter().enumerate() {
            let [x, y] = self.tile_corner(tile as u32);
            texels.extend(record.clip_from_world.transpose().to_cols_array());
            texels.push(record.texel_size);
            texels.push(match record.kind {
                SunLevelKind::Cached => 0.0,
                SunLevelKind::Sky => 1.0,
            });
            texels.extend(record.direction_to_light.to_array());
            texels.extend([x as f32, y as f32, self.tile_size as f32]);
        }
        texels
    }
}

/// Float depth needs a filterable 32-bit format to sit in the materials' bindless array; without one each float's bits
/// go into the four bytes of a texel, which the same bytes on the CPU side already are.
pub fn atlas_format(features: WgpuFeatures) -> TextureFormat {
    if features.contains(WgpuFeatures::FLOAT32_FILTERABLE) { TextureFormat::R32Float } else { TextureFormat::Rgba8Unorm }
}

pub fn texel_bytes(texels: &[f32]) -> Vec<u8> {
    texels.iter().flat_map(|texel| texel.to_le_bytes()).collect()
}

#[derive(Resource, Clone, Default, ExtractResource)]
#[extract_app(RenderApp)]
pub struct SunLevelAtlas {
    pub image: Option<Handle<Image>>,
    pub made: Option<(AtlasLayout, TextureFormat)>,
}

impl MaterialAtlas for SunLevelAtlas {
    const SLOT: MaterialSlot = MaterialSlot::DepthMap;

    fn wanted(&self) -> Option<Handle<Image>> {
        self.image.as_ref().cloned()
    }
}

/// A new image holds no level, so the whole cache redraws.
pub(crate) fn follow_level_atlas(frame: Res<SunLevelFrame>, device: Option<Res<RenderDevice>>, stale: Res<SunCacheStale>, mut atlas: ResMut<SunLevelAtlas>, mut images: ResMut<Assets<Image>>) {
    let Some(device) = device else { return };
    let wanted = AtlasLayout::choose(frame.records.len() as u32, frame.map_size, device.limits().max_texture_dimension_2d)
        .filter(|layout| layout.tiles > 0 && layout.tile_size > 0)
        .map(|layout| (layout, atlas_format(device.features())));
    if wanted == atlas.made {
        return;
    }
    atlas.image = wanted.map(|(layout, format)| new_atlas(&mut images, layout.size(), format));
    atlas.made = wanted;
    stale.mark_lost();
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PropCaster {
    pub(crate) mesh: AssetId<Mesh>,
    pub(crate) world_from_local: Mat4,
    /// A masked prop's texture and alpha cutoff.
    pub(crate) cut: Option<(AssetId<Image>, f32)>,
}

/// The settled props a level draws, as Bevy's shadow queue would see them; filled only on a frame a level draws.
#[derive(Resource, Default)]
pub(crate) struct SunLevelProps(pub(crate) Vec<PropCaster>);

fn clear_props(mut props: ResMut<SunLevelProps>) {
    props.0.clear();
}

type PropQuery<E> = (&'static Mesh3d, &'static GlobalTransform, &'static StillProp, &'static RenderLayers, &'static InheritedVisibility, &'static MeshMaterial3d<ExtendedMaterial<StandardMaterial, E>>);

fn extract_props<E: MaterialExtension>(
    mut props: ResMut<SunLevelProps>,
    frame: Extract<Res<SunLevelFrame>>,
    materials: Extract<Option<Res<Assets<ExtendedMaterial<StandardMaterial, E>>>>>,
    casters: Extract<Query<PropQuery<E>>>,
) {
    let Some(materials) = materials.as_deref().filter(|_| frame.drawing.is_some()) else { return };
    let static_casters = RenderLayers::layer(STATIC_CASTER_LAYER);
    let settled = casters.iter().filter(|(_, _, still, layers, seen, _)| still.0 && seen.get() && layers.intersects(&static_casters));
    props.0.extend(settled.filter_map(|(mesh, at, _, _, _, material)| {
        let base = &materials.get(&material.0)?.base;
        let cut = match base.alpha_mode {
            AlphaMode::Opaque => None,
            AlphaMode::Mask(cutoff) => Some((base.base_color_texture.as_ref()?.id(), cutoff)),
            _ => return None,
        };
        Some(PropCaster { mesh: mesh.id(), world_from_local: at.to_matrix(), cut })
    }));
}

pub struct SunLevelsPlugin;

impl Plugin for SunLevelsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SunLevelFrame>().init_resource::<SunLevelAtlas>();
        if app.get_sub_app(RenderApp).is_none() {
            return;
        }
        add_shaders(app, [(LEVEL_CASTER_SHADER, Shader::from_wesl(include_str!("level_caster.wesl"), "p2_render/level_caster.wesl"))]);
        app.add_plugins((ExtractResourcePlugin::<SunLevelFrame>::default(), ExtractResourcePlugin::<SunLevelAtlas>::default()));
        follow_atlas::<SunLevelAtlas>(app);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render
                .init_resource::<SunLevelProps>()
                .add_systems(ExtractSchedule, (clear_props, extract_props::<BakedLight>, extract_props::<DynamicLit>, extract_props::<HardwareLighting>).chain());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_columns_unless_the_device_limit_needs_more() {
        assert_eq!(AtlasLayout::choose(5, 4096, 16384).map(|layout| layout.columns), Some(2));
        assert_eq!(AtlasLayout::choose(5, 4096, 16384).map(|layout| layout.size()), Some([8192, 12289]));
        assert_eq!(AtlasLayout::choose(5, 4096, 12288).map(|layout| layout.columns), Some(3));
        assert_eq!(AtlasLayout::choose(5, 4096, 8192), None);
        assert_eq!(AtlasLayout::choose(4, 2048, 8192).map(|layout| layout.size()), Some([4096, 4097]));
    }

    #[test]
    fn tiles_fill_rows_left_to_right_above_the_record_row() {
        let layout = AtlasLayout { columns: 2, tiles: 5, tile_size: 1024 };
        let corners: Vec<[u32; 2]> = (0..5).map(|tile| layout.tile_corner(tile)).collect();
        assert_eq!(corners, [[0, 0], [1024, 0], [0, 1024], [1024, 1024], [0, 2048]]);
        assert_eq!(layout.record_row(), 3072);
        assert_eq!(layout.size(), [2048, 3073]);
    }

    #[test]
    fn a_record_holds_its_matrix_texel_kind_direction_and_tile() {
        let layout = AtlasLayout { columns: 2, tiles: 2, tile_size: 512 };
        let matrix = Mat4::from_cols_array(&std::array::from_fn(|at| at as f32));
        let cached = SunLevelRecord {
            kind: SunLevelKind::Cached,
            clip_from_world: matrix,
            texel_size: 0.5,
            direction_to_light: Vec3::new(0.1, 0.2, 0.3),
            world_from_view: Mat4::IDENTITY,
            clip_from_view: Mat4::IDENTITY,
        };
        let sky = SunLevelRecord { kind: SunLevelKind::Sky, texel_size: 96.0, direction_to_light: Vec3::Y, ..cached };
        let texels = layout.record_texels(&[cached, sky]);
        assert_eq!(texels.len(), HEADER_TEXELS + 2 * RECORD_TEXELS);
        assert_eq!(texels[0], 2.0);
        let first = &texels[HEADER_TEXELS..HEADER_TEXELS + RECORD_TEXELS];
        assert_eq!(first[..4], [matrix.x_axis.x, matrix.y_axis.x, matrix.z_axis.x, matrix.w_axis.x]);
        assert_eq!(first[..16], matrix.transpose().to_cols_array());
        assert_eq!(first[16..], [0.5, 0.0, 0.1, 0.2, 0.3, 0.0, 0.0, 512.0]);
        let second = &texels[HEADER_TEXELS + RECORD_TEXELS..];
        assert_eq!(second[16..], [96.0, 1.0, 0.0, 1.0, 0.0, 512.0, 0.0, 512.0]);
    }

    #[test]
    fn the_packed_format_keeps_every_bit_of_a_float() {
        assert_eq!(atlas_format(WgpuFeatures::FLOAT32_FILTERABLE), TextureFormat::R32Float);
        assert_eq!(atlas_format(WgpuFeatures::empty()), TextureFormat::Rgba8Unorm);
        let depth = 0.123_456_79_f32;
        let bytes = texel_bytes(&[depth]);
        assert_eq!(f32::from_bits(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])), depth);
    }
}
