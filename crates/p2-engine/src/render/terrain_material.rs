use bevy::asset::{Asset, Handle, uuid_handle};
use bevy::image::Image;
use bevy::math::{UVec4, Vec3, Vec4};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::reflect::Reflect;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::{Shader, ShaderRef};

pub const MAX_LAYERS: usize = 8;

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainLayers>;

#[derive(Clone, Debug, PartialEq)]
pub struct TerrainPassLayer {
    pub texture: Handle<Image>,
    pub alpha_map: Handle<Image>,
    pub matrix: [[f32; 4]; 4],
}

pub fn one_pass(layers: usize, shader_layers: bool) -> bool {
    (1..=MAX_LAYERS).contains(&layers) && !shader_layers
}

pub fn bevy_uv_rows(matrix: &[[f32; 4]; 4], center: Vec3) -> [Vec4; 2] {
    [0, 1].map(|c| {
        let row = Vec3::new(matrix[0][c], matrix[2][c], matrix[1][c]);
        row.extend(matrix[3][c] + row.dot(center))
    })
}

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct TerrainParams {
    pub u: [Vec4; MAX_LAYERS],
    pub v: [Vec4; MAX_LAYERS],
    pub count: UVec4,
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
#[data(100, TerrainParams, binding_array(133))]
#[bindless(index_table(range(100..133), binding(134)))]
pub struct TerrainLayers {
    pub params: TerrainParams,
    #[texture(101)]
    #[sampler(102)]
    pub texture_0: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    pub texture_1: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    pub texture_2: Option<Handle<Image>>,
    #[texture(107)]
    #[sampler(108)]
    pub texture_3: Option<Handle<Image>>,
    #[texture(109)]
    #[sampler(110)]
    pub texture_4: Option<Handle<Image>>,
    #[texture(111)]
    #[sampler(112)]
    pub texture_5: Option<Handle<Image>>,
    #[texture(113)]
    #[sampler(114)]
    pub texture_6: Option<Handle<Image>>,
    #[texture(115)]
    #[sampler(116)]
    pub texture_7: Option<Handle<Image>>,
    #[texture(119)]
    #[sampler(120)]
    pub alpha_map_1: Option<Handle<Image>>,
    #[texture(121)]
    #[sampler(122)]
    pub alpha_map_2: Option<Handle<Image>>,
    #[texture(123)]
    #[sampler(124)]
    pub alpha_map_3: Option<Handle<Image>>,
    #[texture(125)]
    #[sampler(126)]
    pub alpha_map_4: Option<Handle<Image>>,
    #[texture(127)]
    #[sampler(128)]
    pub alpha_map_5: Option<Handle<Image>>,
    #[texture(129)]
    #[sampler(130)]
    pub alpha_map_6: Option<Handle<Image>>,
    #[texture(131)]
    #[sampler(132)]
    pub alpha_map_7: Option<Handle<Image>>,
}

impl TerrainLayers {
    pub fn new(layers: &[TerrainPassLayer], center: Vec3) -> Self {
        let layers = &layers[..layers.len().min(MAX_LAYERS)];
        let mut params = TerrainParams { count: UVec4::new(layers.len() as u32, 0, 0, 0), ..TerrainParams::default() };
        for (k, layer) in layers.iter().enumerate() {
            [params.u[k], params.v[k]] = bevy_uv_rows(&layer.matrix, center);
        }
        let texture = |k: usize| layers.get(k).map(|layer| layer.texture.clone());
        let alpha_map = |k: usize| layers.get(k).map(|layer| layer.alpha_map.clone());
        Self {
            params,
            texture_0: texture(0),
            texture_1: texture(1),
            texture_2: texture(2),
            texture_3: texture(3),
            texture_4: texture(4),
            texture_5: texture(5),
            texture_6: texture(6),
            texture_7: texture(7),
            alpha_map_1: alpha_map(1),
            alpha_map_2: alpha_map(2),
            alpha_map_3: alpha_map(3),
            alpha_map_4: alpha_map(4),
            alpha_map_5: alpha_map(5),
            alpha_map_6: alpha_map(6),
            alpha_map_7: alpha_map(7),
        }
    }
}

impl From<&TerrainLayers> for TerrainParams {
    fn from(extension: &TerrainLayers) -> Self {
        extension.params
    }
}

pub(crate) const TERRAIN_SHADER: Handle<Shader> = uuid_handle!("a4c9e2f7-3b61-4d58-9e0a-7f2b18c6d953");

impl MaterialExtension for TerrainLayers {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(TERRAIN_SHADER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sectors_past_the_layer_limit_or_with_shader_layers_keep_their_passes() {
        assert!(one_pass(1, false));
        assert!(one_pass(MAX_LAYERS, false));
        assert!(!one_pass(MAX_LAYERS + 1, false));
        assert!(!one_pass(3, true));
        assert!(!one_pass(0, false));
    }

    #[test]
    fn the_uv_rows_give_the_cpu_layer_uv() {
        let matrix = [[0.01, 0.002, 0.0, 0.0], [-0.003, 0.02, 0.0, 0.0], [0.004, -0.005, 0.0, 0.0], [0.25, -0.75, 0.0, 1.0]];
        let center = Vec3::new(1200.0, -300.0, 4500.0);
        let [u, v] = bevy_uv_rows(&matrix, center);
        for [x, y, z] in [[0.0, 0.0, 0.0], [5120.0, -2048.0, 312.0], [-700.5, 64.0, -1300.0]] {
            let cpu = [0, 1].map(|c| x * matrix[0][c] + y * matrix[1][c] + z * matrix[2][c] + matrix[3][c]);
            let p = Vec3::from(p2_bevy_assets::to_bevy([x, y, z])) - center;
            let gpu = [u.truncate().dot(p) + u.w, v.truncate().dot(p) + v.w];
            for c in 0..2 {
                assert!((gpu[c] - cpu[c]).abs() < 1e-3, "{gpu:?} {cpu:?}");
            }
        }
    }

    #[test]
    fn layers_fill_in_order_and_the_first_takes_no_alpha_map() {
        let a: Handle<Image> = uuid_handle!("0b6f1c2e-5d4a-4e3b-9c8d-1a2b3c4d5e01");
        let b: Handle<Image> = uuid_handle!("0b6f1c2e-5d4a-4e3b-9c8d-1a2b3c4d5e02");
        let c: Handle<Image> = uuid_handle!("0b6f1c2e-5d4a-4e3b-9c8d-1a2b3c4d5e03");
        let layer = |texture: &Handle<Image>, alpha_map: &Handle<Image>| TerrainPassLayer {
            texture: texture.clone(),
            alpha_map: alpha_map.clone(),
            matrix: [[0.0; 4]; 4],
        };
        let layers = [layer(&a, &b), layer(&b, &c), layer(&c, &a)];
        let material = TerrainLayers::new(&layers, Vec3::ZERO);
        assert_eq!(material.params.count.x, 3);
        assert_eq!(material.texture_2, Some(c.clone()));
        assert_eq!(material.texture_3, None);
        assert_eq!(material.alpha_map_1, Some(c));
        let many = vec![layer(&a, &b); MAX_LAYERS + 2];
        assert_eq!(TerrainLayers::new(&many, Vec3::ZERO).params.count.x, MAX_LAYERS as u32);
    }
}
