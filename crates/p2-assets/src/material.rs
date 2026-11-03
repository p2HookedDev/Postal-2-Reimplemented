use p2_package::Package;
use p2_script::{ObjectProperties, Objects, Resolve, Source};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseTexture {
    Texture(i32),
    Import(i32),
    Color,
    Cubemap(i32),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blending {
    Opaque,
    Masked,
    Alpha,
    Additive,
    Modulate,
    Invisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscillationType {
    Pan,
    Stretch,
    StretchRepeat,
    Other,
}

impl OscillationType {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Pan,
            1 => Self::Stretch,
            2 => Self::StretchRepeat,
            _ => Self::Other,
        }
    }
}

/// `Shader.OutputBlending`, `OB_Normal` 0 .. `OB_Darken` 6; values past the enum act as `OB_Invisible`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputBlending {
    Normal,
    Masked,
    Modulate,
    Translucent,
    Invisible,
    Brighten,
    Darken,
}

impl OutputBlending {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Normal,
            1 => Self::Masked,
            2 => Self::Modulate,
            3 => Self::Translucent,
            5 => Self::Brighten,
            6 => Self::Darken,
            _ => Self::Invisible,
        }
    }
}

/// `FinalBlend.FrameBufferBlending`, `FB_Overwrite` 0 .. `FB_Invisible` 7; values past the enum act as `FB_Invisible`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBufferBlending {
    Overwrite,
    Modulate,
    AlphaBlend,
    AlphaModulate,
    Translucent,
    Darken,
    Brighten,
    Invisible,
}

impl FrameBufferBlending {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Overwrite,
            1 => Self::Modulate,
            2 => Self::AlphaBlend,
            3 => Self::AlphaModulate,
            4 => Self::Translucent,
            5 => Self::Darken,
            6 => Self::Brighten,
            _ => Self::Invisible,
        }
    }

    pub fn byte(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorOperation {
    Material1,
    Material2,
    Multiply,
    Add,
    Subtract,
    AlphaBlendWithMask,
    AddWithMaskModulation,
    Mask,
    /// A value past the enum: combines as [`ColorOperation::Mask`], while the base texture walk starts from Material1.
    Other,
}

impl ColorOperation {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Material1,
            1 => Self::Material2,
            2 => Self::Multiply,
            3 => Self::Add,
            4 => Self::Subtract,
            5 => Self::AlphaBlendWithMask,
            6 => Self::AddWithMaskModulation,
            7 => Self::Mask,
            _ => Self::Other,
        }
    }
}

/// `Combiner.AlphaOperation`; values past the enum act as [`AlphaOperation::Material2`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaOperation {
    Mask,
    Multiply,
    Add,
    Material1,
    Material2,
}

impl AlphaOperation {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Mask,
            1 => Self::Multiply,
            2 => Self::Add,
            3 => Self::Material1,
            _ => Self::Material2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlowLayer {
    pub material: i32,
    pub mask: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailLayer {
    pub material: i32,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecularLayer {
    pub material: i32,
    pub mask: i32,
    pub mask_is_diffuse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UvModifier {
    Panner { direction: [f32; 2], rate: f32 },
    Scaler { scale: [f32; 2], offset: [f32; 2] },
    Rotator { rotation: [i32; 3], constant: bool, offset: [f32; 2] },
    Oscillator { rate: [f32; 2], phase: [f32; 2], amplitude: [f32; 2], kind: [OscillationType; 2], offset: [f32; 2] },
}

/// 2D affine UV transform: `u' = m[0][0]·u + m[0][1]·v + m[0][2]`, `v' = m[1][0]·u + m[1][1]·v + m[1][2]`.
pub type UvMatrix = [[f32; 3]; 2];

const UV_IDENTITY: UvMatrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

fn uv_then(a: UvMatrix, b: UvMatrix) -> UvMatrix {
    let row = |r: [f32; 3]| [
        r[0] * a[0][0] + r[1] * a[1][0],
        r[0] * a[0][1] + r[1] * a[1][1],
        r[0] * a[0][2] + r[1] * a[1][2] + r[2],
    ];
    [row(b[0]), row(b[1])]
}

fn around(m: UvMatrix, pivot: [f32; 2]) -> UvMatrix {
    let to = [[1.0, 0.0, -pivot[0]], [0.0, 1.0, -pivot[1]]];
    let back = [[1.0, 0.0, pivot[0]], [0.0, 1.0, pivot[1]]];
    uv_then(uv_then(to, m), back)
}

impl UvModifier {
    pub fn matrix(&self, seconds: f32, size: [f32; 2]) -> UvMatrix {
        let pivot = |offset: [f32; 2]| [offset[0] / size[0].max(1.0), offset[1] / size[1].max(1.0)];
        match *self {
            Self::Panner { direction, rate } => {
                let [du, dv] = direction.map(|d| (d * rate * seconds).fract());
                [[1.0, 0.0, du], [0.0, 1.0, dv]]
            }
            Self::Scaler { scale, offset } => [[scale[0], 0.0, offset[0]], [0.0, scale[1], offset[1]]],
            Self::Rotator { rotation, constant, offset } => {
                let rotation = if constant { rotation.map(|r| (r as f32 * seconds) as i32 & 0xffff) } else { rotation };
                let [x, y, z] = p2_math::axes_at(rotation.map(radians));
                around([[x.x, y.x, z.x], [x.y, y.y, z.y]], pivot(offset))
            }
            Self::Oscillator { rate, phase, amplitude, kind, offset } => {
                let value = |axis: usize| {
                    let f = if kind[axis] == OscillationType::StretchRepeat { 0.5 } else { 2.0 };
                    let angle = ((rate[axis] * seconds).fract() + phase[axis] * std::f32::consts::TAU) * f * std::f32::consts::PI;
                    angle.sin() * amplitude[axis]
                };
                let mut m = UV_IDENTITY;
                for axis in 0..2 {
                    match kind[axis] {
                        OscillationType::Pan => m[axis][2] = value(axis),
                        OscillationType::Stretch | OscillationType::StretchRepeat => m[axis][axis] = value(axis) + 1.0,
                        OscillationType::Other => {}
                    }
                }
                around(m, pivot(offset))
            }
        }
    }
}

/// The combined transform of `modifiers` (outermost first; the innermost applies first).
pub fn uv_matrix(modifiers: &[UvModifier], seconds: f32, size: [f32; 2]) -> UvMatrix {
    modifiers.iter().rev().fold(UV_IDENTITY, |m, modifier| uv_then(m, modifier.matrix(seconds, size)))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalBlend {
    pub blending: FrameBufferBlending,
    pub alpha_ref: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "independent material flags of retail's chain (`darken` the fourth)")]
pub struct RenderState {
    pub blending: Option<Blending>,
    pub two_sided: bool,
    pub uv: Vec<UvModifier>,
    pub opacity: Option<i32>,
    pub wet: Option<i32>,
    pub color: Option<[u8; 4]>,
    pub color_blend: bool,
    /// The blending was set by a `Shader` with `OB_Darken`: the frame buffer is multiplied by one minus the colour
    /// (D3D `ZERO` / `INVSRCCOLOR`), drawn as [`Blending::Modulate`] of the inverted colour.
    pub darken: bool,
    pub alpha_ref: Option<u8>,
    /// The innermost `FinalBlend` of the chain: as in retail, each one met overwrites the frame buffer
    /// state, so the last one met decides.
    pub final_blend: Option<FinalBlend>,
    pub uv_stream: Option<u8>,
    /// A `Shader` whose `SelfIllumination` has no mask is drawn alone, with no lighting (unverified).
    pub unlit: bool,
    pub glow: Option<GlowLayer>,
    pub detail: Option<DetailLayer>,
    pub specular: Option<SpecularLayer>,
    pub env_map: Option<u8>,
    pub specular_uv: Vec<UvModifier>,
    pub combiner: Option<i32>,
    pub switch: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combiner {
    pub material1: i32,
    pub material2: i32,
    pub mask: i32,
    pub ops: CombineOps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CombineOps {
    pub color: ColorOperation,
    pub alpha: AlphaOperation,
    pub invert_mask: bool,
    pub modulate: u8,
}

impl CombineOps {
    /// One texel of `a` (Material1) combined with `b` (Material2); `mask` `None` uses `a`'s alpha.
    /// Subtract is `b` − `a`; add-with-mask-modulation is `b` + mask alpha × `a` (unverified).
    pub fn texel(&self, a: [u8; 4], b: [u8; 4], mask: Option<[u8; 4]>) -> [u8; 4] {
        let f = |c: u8| f32::from(c) / 255.0;
        let mask_alpha = mask.map_or(a[3], |m| m[3]);
        let alpha = f(if self.invert_mask { 255 - mask_alpha } else { mask_alpha });
        let color = |i: usize| {
            let (x, y) = (f(a[i]), f(b[i]));
            match self.color {
                ColorOperation::Material1 => x,
                ColorOperation::Material2 => y,
                ColorOperation::Multiply => x * y * f32::from(self.modulate.max(1)),
                ColorOperation::Add => x + y,
                ColorOperation::Subtract => y - x,
                ColorOperation::AlphaBlendWithMask => x + (y - x) * alpha,
                ColorOperation::AddWithMaskModulation => y + alpha * x,
                ColorOperation::Mask | ColorOperation::Other => mask.map_or(x, |m| f(m[i])),
            }
        };
        let out_alpha = match self.alpha {
            AlphaOperation::Mask => alpha,
            AlphaOperation::Multiply => f(a[3]) * f(b[3]),
            AlphaOperation::Add => f(a[3]) + f(b[3]),
            AlphaOperation::Material1 => f(a[3]),
            AlphaOperation::Material2 => f(b[3]),
        };
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [byte(color(0)), byte(color(1)), byte(color(2)), byte(out_alpha)]
    }
}

pub fn read_combiner<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<Combiner, Error> {
    let object = objects.read(src, export)?;
    let reference = |name| object.object_in(name, src.package).unwrap_or(0);
    let modulate = if object.bool("Modulate4X") { 4 } else if object.bool("Modulate2X") { 2 } else { 1 };
    Ok(Combiner {
        material1: reference("Material1"),
        material2: reference("Material2"),
        mask: reference("Mask"),
        ops: CombineOps {
            color: ColorOperation::from_byte(object.byte("CombineOperation")),
            alpha: AlphaOperation::from_byte(object.byte("AlphaOperation")),
            invert_mask: object.bool("InvertMask"),
            modulate,
        },
    })
}

/// The six `Faces` of the `Cubemap` `export` (0-based) of `src`: object references in its package (0: none), in the
/// order the D3D driver uploads them, +X −X +Y −Y +Z −Z (unverified).
pub fn read_cubemap_faces<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<[i32; 6], Error> {
    let object = objects.read(src, export)?;
    Ok(std::array::from_fn(|face| match object.get("Faces", face as u32) {
        Some((package, &p2_script::StoredValue::Object(reference))) if std::ptr::eq(package, src.package) => reference,
        _ => 0,
    }))
}

pub(crate) const MAX_MATERIAL_DEPTH: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MaterialClass {
    Texture,
    Shader,
    FinalBlend,
    ColorModifier,
    ConstantColor,
    VertexColor,
    Combiner,
    WetTexture,
    MaterialSwitch,
    Modifier,
    TexPanner,
    TexRotator,
    TexOscillator,
    TexEnvMap,
    TexScaler,
    TexCoordSource,
    OtherTexModifier,
}

impl MaterialClass {
    /// Names compare ignoring case, as the engine's names do.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "texture" | "movietexture" => Self::Texture,
            "shader" => Self::Shader,
            "finalblend" => Self::FinalBlend,
            "colormodifier" => Self::ColorModifier,
            "constantcolor" => Self::ConstantColor,
            "vertexcolor" => Self::VertexColor,
            "combiner" => Self::Combiner,
            "wettexture" => Self::WetTexture,
            "materialswitch" => Self::MaterialSwitch,
            "modifier" | "opacitymodifier" | "mattypemodifier" => Self::Modifier,
            "texpanner" => Self::TexPanner,
            "texrotator" => Self::TexRotator,
            "texoscillator" => Self::TexOscillator,
            "texenvmap" => Self::TexEnvMap,
            "texscaler" => Self::TexScaler,
            "texcoordsource" => Self::TexCoordSource,
            "texmodifier" | "texmatrix" => Self::OtherTexModifier,
            _ => return None,
        })
    }
}

/// Follows `material` (an object reference in `pkg`) down to the texture a simple renderer should draw, adding the
/// chain's render state to `state`. Stops at the first import; following it is the caller's job (same `state`).
pub fn base_texture<'a, R: Resolve + ?Sized>(
    objects: &mut Objects<'a, R>,
    src: Source<'a>,
    material: i32,
    state: &mut RenderState,
) -> Result<BaseTexture, Error> {
    let pkg = src.package;
    let mut current = material;
    for _ in 0..MAX_MATERIAL_DEPTH {
        let export = match current {
            0 => return Ok(BaseTexture::None),
            ..0 => return Ok(BaseTexture::Import(current)),
            _ => pkg.exports.get(current as usize - 1).ok_or(Error::BadReference(current))?,
        };
        let name = pkg.object_name(export.class_index).unwrap_or_default();
        if name.eq_ignore_ascii_case("Cubemap") {
            return Ok(BaseTexture::Cubemap(current));
        }
        let Some(class) = MaterialClass::from_name(name) else { return Ok(BaseTexture::None) };
        match class {
            MaterialClass::Combiner => {
                state.combiner.get_or_insert(current);
            }
            MaterialClass::WetTexture => {
                state.wet.get_or_insert(current);
            }
            MaterialClass::MaterialSwitch => {
                state.switch.get_or_insert(current);
            }
            _ => {}
        }
        let object = objects.read(src, current as usize - 1)?;
        let follow: &[&str] = match class {
            MaterialClass::Texture => {
                texture_state(&object, pkg, state);
                return Ok(BaseTexture::Texture(current));
            }
            MaterialClass::ConstantColor => {
                state.multiply_color(object.color("Color"));
                return Ok(BaseTexture::Color);
            }
            MaterialClass::VertexColor => return Ok(BaseTexture::Color),
            MaterialClass::Shader => shader(&object, pkg, state),
            MaterialClass::FinalBlend => final_blend(&object, state),
            MaterialClass::ColorModifier => {
                state.two_sided |= object.bool("RenderTwoSided");
                state.color_blend |= object.bool("AlphaBlend");
                state.multiply_color(object.color("Color"));
                &["Material"]
            }
            MaterialClass::Combiner => match ColorOperation::from_byte(object.byte("CombineOperation")) {
                ColorOperation::Material2 => &["Material2", "Material1"],
                ColorOperation::Mask => &["Mask", "Material1"],
                _ => &["Material1", "Material2"],
            },
            MaterialClass::WetTexture => &["SourceTexture"],
            MaterialClass::MaterialSwitch | MaterialClass::Modifier => &["Material"],
            MaterialClass::TexPanner
            | MaterialClass::TexRotator
            | MaterialClass::TexOscillator
            | MaterialClass::TexEnvMap
            | MaterialClass::TexScaler
            | MaterialClass::TexCoordSource
            | MaterialClass::OtherTexModifier => {
                tex_modifier(class, &object, state);
                &["Material"]
            }
        };
        // A class default would index the class's package; material classes are not known to set one (unverified).
        current = follow.iter().map(|name| object.object_in(name, pkg).unwrap_or(0)).find(|&index| index != 0).unwrap_or(0);
    }
    Err(Error::MaterialChain(material))
}

fn texture_state(object: &ObjectProperties, pkg: &Package, state: &mut RenderState) {
    let flag = |name| object.bool(name);
    let detail = object.object_in("Detail", pkg).unwrap_or(0);
    state.two_sided |= flag("bTwoSided");
    if detail != 0 {
        state.detail.get_or_insert(DetailLayer { material: detail, scale: object.float("DetailScale") });
    }
    state.blending.get_or_insert(if flag("bAlphaTexture") {
        Blending::Alpha
    } else if flag("bMasked") {
        state.alpha_ref.get_or_insert(127);
        Blending::Masked
    } else {
        Blending::Opaque
    });
}

fn shader(object: &ObjectProperties, pkg: &Package, state: &mut RenderState) -> &'static [&'static str] {
    let byte = |name| object.byte(name);
    let reference = |name| object.object_in(name, pkg).unwrap_or(0);
    state.two_sided |= object.bool("TwoSided");
    if reference("Detail") != 0 {
        state.detail.get_or_insert(DetailLayer { material: reference("Detail"), scale: object.float("DetailScale") });
    }
    if reference("Specular") != 0 {
        let mask = reference("SpecularityMask");
        state.specular.get_or_insert(SpecularLayer { material: reference("Specular"), mask, mask_is_diffuse: mask != 0 && mask == reference("Diffuse") });
    }
    let opacity = reference("Opacity") != 0;
    // A separate opacity material gives the alpha; the same material as `Diffuse` means the
    // diffuse alpha, which the base texture already has.
    if opacity && reference("Opacity") != reference("Diffuse") && state.opacity.is_none() {
        state.opacity = Some(reference("Opacity"));
    }
    // `OB_Normal` without `Opacity` is opaque: a `bAlphaTexture` diffuse's alpha then only masks
    // the specular.
    let output = OutputBlending::from_byte(byte("OutputBlending"));
    let blending = match output {
        OutputBlending::Normal if opacity => Blending::Alpha,
        OutputBlending::Normal => Blending::Opaque,
        OutputBlending::Masked => Blending::Masked,
        OutputBlending::Modulate | OutputBlending::Darken => Blending::Modulate,
        OutputBlending::Translucent | OutputBlending::Brighten => Blending::Additive,
        OutputBlending::Invisible => Blending::Invisible,
    };
    if state.blending.is_none() {
        state.blending = Some(blending);
        state.darken = output == OutputBlending::Darken;
    }
    let (glow, mask) = (reference("SelfIllumination"), reference("SelfIlluminationMask"));
    if glow != 0 && mask == 0 {
        state.unlit = true;
        &["SelfIllumination"]
    } else {
        if glow != 0 && reference("Diffuse") != 0 {
            state.glow.get_or_insert(GlowLayer { material: glow, mask });
        }
        &["Diffuse", "SelfIllumination"]
    }
}

fn final_blend(object: &ObjectProperties, state: &mut RenderState) -> &'static [&'static str] {
    let (flag, byte) = (|name| object.bool(name), |name| object.byte(name));
    state.two_sided |= flag("TwoSided");
    if flag("AlphaTest") {
        state.alpha_ref.get_or_insert(byte("AlphaRef"));
    }
    let frame_buffer = FrameBufferBlending::from_byte(byte("FrameBufferBlending"));
    state.final_blend = Some(FinalBlend { blending: frame_buffer, alpha_ref: flag("AlphaTest").then(|| byte("AlphaRef")) });
    state.blending.get_or_insert(match frame_buffer {
        FrameBufferBlending::Overwrite if flag("AlphaTest") => Blending::Masked,
        FrameBufferBlending::Overwrite => Blending::Opaque,
        FrameBufferBlending::Modulate | FrameBufferBlending::Darken => Blending::Modulate,
        FrameBufferBlending::AlphaBlend | FrameBufferBlending::AlphaModulate => Blending::Alpha,
        FrameBufferBlending::Translucent | FrameBufferBlending::Brighten => Blending::Additive,
        FrameBufferBlending::Invisible => Blending::Invisible,
    });
    &["Material"]
}

fn tex_modifier(class: MaterialClass, object: &ObjectProperties, state: &mut RenderState) {
    let (float, byte) = (|name| object.float(name), |name| object.byte(name));
    // `TexModifier.TexCoordSource`: the first one that is not `TCS_NoChange` (13) picks the coordinates; vertex
    // streams (0..7) are drawn, generated coordinates (world, camera, environment map) are not yet.
    if state.uv_stream.is_none() {
        let source = if class == MaterialClass::TexCoordSource { u8::try_from(object.int("SourceChannel")).unwrap_or(0) } else { byte("TexCoordSource") };
        if source < 8 {
            state.uv_stream = Some(source);
        }
    }
    match class {
        MaterialClass::TexPanner => {
            let [pitch, yaw, _] = object.rotator("PanDirection");
            let (pitch, yaw) = (radians(pitch), radians(yaw));
            let direction = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin()];
            state.uv.push(UvModifier::Panner { direction, rate: float("PanRate") });
        }
        MaterialClass::TexRotator => {
            let offset = [float("UOffset"), float("VOffset")];
            let rotation = object.rotator("Rotation");
            // TR_FixedRotation 0, TR_ConstantlyRotating 1; oscillating rotation (2) is not applied yet.
            let constant = byte("TexRotationType") == 1;
            state.uv.push(UvModifier::Rotator { rotation, constant, offset });
        }
        MaterialClass::TexOscillator => {
            state.uv.push(UvModifier::Oscillator {
                rate: [float("UOscillationRate"), float("VOscillationRate")],
                phase: [float("UOscillationPhase"), float("VOscillationPhase")],
                amplitude: [float("UOscillationAmplitude"), float("VOscillationAmplitude")],
                kind: [byte("UOscillationType"), byte("VOscillationType")].map(OscillationType::from_byte),
                offset: [float("UOffset"), float("VOffset")],
            });
        }
        MaterialClass::TexEnvMap => {
            state.env_map.get_or_insert(byte("EnvMapType"));
        }
        MaterialClass::TexScaler => {
            let scale = [float("UScale"), float("VScale")];
            let offset = [float("UOffset"), float("VOffset")];
            state.uv.push(UvModifier::Scaler { scale, offset });
        }
        _ => {}
    }
}

impl RenderState {
    fn multiply_color(&mut self, color: [u8; 4]) {
        let current = self.color.unwrap_or([255; 4]);
        self.color = Some(std::array::from_fn(|i| ((u16::from(current[i]) * u16::from(color[i]) + 127) / 255) as u8));
    }
}

fn radians(units: i32) -> f32 {
    p2_math::radians(units as f32)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panner_offset_wraps() {
        let panner = UvModifier::Panner { direction: [0.0, 1.0], rate: 0.5 };
        let m = panner.matrix(3.0, [256.0, 256.0]);
        assert!(m[0][2].abs() < 1e-6 && (m[1][2] - 0.5).abs() < 1e-6, "{m:?}");
    }

    #[test]
    fn combiner_texels() {
        let ops = |color, alpha, invert_mask, modulate| CombineOps { color: ColorOperation::from_byte(color), alpha: AlphaOperation::from_byte(alpha), invert_mask, modulate };
        let (a, b) = ([100, 200, 0, 255], [128, 128, 255, 51]);
        assert_eq!(ops(2, 1, false, 2).texel(a, b, None), [100, 201, 0, 51]);
        assert_eq!(ops(5, 0, true, 1).texel(a, b, Some([0, 0, 0, 255])), [100, 200, 0, 0]);
        assert_eq!(ops(5, 0, false, 1).texel(a, b, Some([0, 0, 0, 255])), [128, 128, 255, 255]);
        assert_eq!(ops(4, 4, false, 1).texel(a, b, None), [28, 0, 255, 51]);
    }

    #[test]
    fn colors_multiply() {
        let mut state = RenderState::default();
        state.multiply_color([255, 128, 0, 255]);
        state.multiply_color([128, 255, 255, 255]);
        assert_eq!(state.color, Some([128, 128, 0, 255]));
    }

    #[test]
    fn rotator_turns_around_pivot() {
        let rotator = UvModifier::Rotator { rotation: [0, 16384, 0], constant: false, offset: [128.0, 128.0] };
        let m = rotator.matrix(0.0, [256.0, 256.0]);
        let (u, v) = (1.0, 0.5);
        let (u, v) = (m[0][0] * u + m[0][1] * v + m[0][2], m[1][0] * u + m[1][1] * v + m[1][2]);
        assert!((u - 0.5).abs() < 1e-5 && (v - 1.0).abs() < 1e-5, "{u} {v}");
    }
}