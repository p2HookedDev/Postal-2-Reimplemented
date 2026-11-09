use bevy::mesh::{Mesh, VertexAttributeValues};

/// `Modulate2X` in linear space: the engine draws all baked lighting (BSP light maps, terrain and static mesh vertex
/// colours) as texture × 2 × lighting in display (gamma) space, so 128 is full brightness. As a factor on a linear
/// (sRGB-decoded) texture: (2 · lighting)^2.2 (unclamped; the output clamps).
pub const MODULATE_2X: f32 = 4.594_794;

pub fn modulate_2x(byte: u8) -> f32 {
    static TABLE: std::sync::LazyLock<[f32; 256]> =
        std::sync::LazyLock::new(|| std::array::from_fn(|byte| (2.0 * byte as f32 / 255.0).powf(2.2)));
    TABLE[usize::from(byte)]
}

pub fn lighting_colors(colors: &[[u8; 4]]) -> Vec<[f32; 4]> {
    colors.iter().map(|&[r, g, b, _]| [modulate_2x(r), modulate_2x(g), modulate_2x(b), 1.0]).collect()
}

pub fn set_vertex_colors(mesh: &mut Mesh, colors: &[[f32; 4]]) {
    if let Some(VertexAttributeValues::Float32x4(current)) = mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
        && current.len() == colors.len()
    {
        current.copy_from_slice(colors);
        return;
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors.to_vec());
}
