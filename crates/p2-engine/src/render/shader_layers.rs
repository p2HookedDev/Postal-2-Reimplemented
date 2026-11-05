use bevy::asset::Handle;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::Vec4;
use bevy::pbr::StandardMaterial;
use bevy::reflect::Reflect;
use p2_assets::UvModifier;
use bevy::render::render_resource::ShaderType;

/// `DetailTexMipBias` as `Postal2.ini` ships it.
pub const DETAIL_MIP_BIAS: f32 = -0.5;

#[derive(ShaderType, Reflect, Debug, Clone, Copy, Default, PartialEq)]
pub struct ShaderLayers {
    /// x: the detail layer's UV scale (0: no detail), y: its mip bias, z: the modern flag (`modern::modern_flag`, 1 all on; read by
    /// the lightmapped, mesh, actor and terrain layer shaders, set by `modern.rs`; 0 is retail), w: 1 while the setting turns detail off.
    pub detail: Vec4,
    /// The specular layer: x mode (0 none, 1 add, 2 × the base alpha, 3 × z, 4 × the emissive texture's alpha), y 1:
    /// world space (else camera space), z the constant mask, w the coordinates (0 a cube, 1 a 2D texture at the
    /// env-map coordinates, 2 a 2D texture at the base UV).
    pub specular: Vec4,
    pub specular_u: Vec4,
    pub specular_v: Vec4,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layers {
    pub detail: Option<Handle<Image>>,
    pub specular: Option<Handle<Image>>,
    pub flat_specular: Option<Handle<Image>>,
    pub params: ShaderLayers,
    pub premultiplied: bool,
    pub specular_uv: Vec<UvModifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecularMode {
    Add = 1,
    BaseAlpha = 2,
    Constant = 3,
    GlowAlpha = 4,
    Multiply = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecularCoords {
    Cube = 0,
    EnvMap = 1,
    BaseUv = 2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecularParams {
    pub mode: SpecularMode,
    pub world: bool,
    pub constant: f32,
    pub coords: SpecularCoords,
}

impl SpecularParams {
    fn vec4(self) -> Vec4 {
        Vec4::new(f32::from(self.mode as u8), f32::from(u8::from(self.world)), self.constant, f32::from(self.coords as u8))
    }
}

impl Layers {
    pub fn new(
        detail: Option<Handle<Image>>,
        scale: f32,
        specular: Option<(Handle<Image>, SpecularParams)>,
        specular_uv: Vec<UvModifier>,
        premultiplied: bool,
    ) -> Self {
        let scale = if detail.is_some() { scale } else { 0.0 };
        let shader_params = specular.as_ref().map_or(Vec4::ZERO, |(_, params)| params.vec4());
        let (specular, flat_specular) = match specular {
            Some((image, params)) if params.coords != SpecularCoords::Cube => (None, Some(image)),
            specular => (specular.map(|(image, _)| image), None),
        };
        let params = ShaderLayers {
            detail: Vec4::new(scale, DETAIL_MIP_BIAS, 0.0, f32::from(u8::from(!p2_bevy_assets::texture_quality().detail_textures))),
            specular: shader_params,
            specular_u: Vec4::X,
            specular_v: Vec4::Y,
        };
        let specular_uv = if flat_specular.is_some() { specular_uv } else { Vec::new() };
        Self { detail, specular, flat_specular, params, premultiplied, specular_uv }
    }

    pub fn lit(&self, mut material: StandardMaterial) -> StandardMaterial {
        if self.premultiplied && material.alpha_mode == AlphaMode::Blend {
            material.alpha_mode = AlphaMode::Premultiplied;
        }
        material
    }

    pub fn images(&self) -> [Option<Handle<Image>>; 3] {
        [self.detail.clone(), self.specular.clone(), self.flat_specular.clone()]
    }

    pub fn key(&self) -> String {
        let id = |image: &Option<Handle<Image>>| image.as_ref().map(Handle::id);
        let (detail, specular, flat_specular) = (id(&self.detail), id(&self.specular), id(&self.flat_specular));
        format!("{detail:?} {specular:?} {flat_specular:?} {:?} {} {:?}", self.params, self.premultiplied, self.specular_uv)
    }
}

macro_rules! layered_extension {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(bevy::asset::Asset, bevy::render::render_resource::AsBindGroup, bevy::reflect::Reflect, Debug, Clone, Default)]
        #[data(100, ShaderLayers, binding_array(107))]
        #[bindless(index_table(range(100..107), binding(108)))]
        pub struct $name {
            pub layers: crate::render::shader_layers::ShaderLayers,
            #[texture(101)]
            #[sampler(102)]
            pub detail: Option<bevy::asset::Handle<bevy::image::Image>>,
            #[texture(103, dimension = "cube")]
            #[sampler(104)]
            pub specular: Option<bevy::asset::Handle<bevy::image::Image>>,
            #[texture(105)]
            #[sampler(106)]
            pub flat_specular: Option<bevy::asset::Handle<bevy::image::Image>>,
        }

        impl $name {
            pub fn new(layers: &crate::render::shader_layers::Layers) -> Self {
                let [detail, specular, flat_specular] = layers.images();
                Self { layers: layers.params, detail, specular, flat_specular }
            }
        }

        impl From<&$name> for crate::render::shader_layers::ShaderLayers {
            fn from(extension: &$name) -> Self {
                extension.layers
            }
        }
    };
}
pub(crate) use layered_extension;
