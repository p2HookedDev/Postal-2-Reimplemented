use std::sync::{Arc, LazyLock};

use p2_package::{Export, Package, Reader};
use p2_script::{FieldLayout, StoredValue, StructLayout, Type, read_properties};

use crate::Error;
use crate::read::{BOX_SIZE, array, count, invalid, skip_array, skip_lazy_array, skip_primitive, vec3};
use crate::terrain::member;

/// These properties are declared in C++, not in script, so no layout can be read from a package.
static STATIC_MESH_LAYOUT: LazyLock<StructLayout> = LazyLock::new(|| {
    let field = |name: &str, ty| FieldLayout { name: Arc::from(name), array_dim: 1, flags: 0, ty };
    let material = StructLayout::new("StaticMeshMaterial".to_owned(), vec![field("Material", Type::Object), field("EnableCollision", Type::Bool)], 0, None);
    let properties = vec![
        field("UseSimpleLineCollision", Type::Bool),
        field("UseSimpleBoxCollision", Type::Bool),
        field("UseSimpleKarmaCollision", Type::Bool),
        field("UseVertexColor", Type::Bool),
        field("Materials", Type::Array(Box::new(Type::Struct(Arc::new(material))))),
    ];
    StructLayout::new("StaticMesh".to_owned(), properties, 0, None)
});

#[derive(Debug, Clone, Default)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors the mesh's saved collision flags")]
pub struct StaticMesh {
    pub sections: Vec<MeshSection>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[u8; 4]>,
    pub use_vertex_color: bool,
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub indices: Vec<u16>,
    pub collision_model: i32,
    /// `CollisionTriangles`: the triangles collision tests when the simplified model is not used, as indices into
    /// `positions`. Only triangles of materials with `EnableCollision` are here.
    pub collision_triangles: Vec<[u32; 3]>,
    pub collision_materials: Vec<u32>,
    /// Number of `CollisionNodes` (the BSP tree over `collision_triangles`); without nodes the triangles are not
    /// tested.
    pub collision_nodes: usize,
    pub use_simple_box_collision: bool,
    pub use_simple_line_collision: bool,
    pub use_simple_karma_collision: bool,
    pub physics_props: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct MeshSection {
    pub is_strip: bool,
    pub first_index: u16,
    pub min_vertex_index: u16,
    pub max_vertex_index: u16,
    pub num_triangles: u16,
    pub num_primitives: u16,
}

pub fn read_static_mesh(pkg: &Package, file: &[u8], export: &Export) -> Result<StaticMesh, Error> {
    let mut r = pkg.export_reader(file, export)?;
    let properties = read_properties(pkg, &mut r, &STATIC_MESH_LAYOUT)?;
    let r = &mut r;
    let flag = |name: &str, default: bool| match member(&properties, name) {
        Some(&StoredValue::Bool(value)) => value,
        _ => default,
    };
    let use_vertex_color = flag("UseVertexColor", false);
    let use_simple_box_collision = flag("UseSimpleBoxCollision", true);
    let use_simple_line_collision = flag("UseSimpleLineCollision", false);
    let use_simple_karma_collision = flag("UseSimpleKarmaCollision", true);

    skip_primitive(r)?;

    let sections = array(r, read_section)?;
    skip_box(r)?;

    let vertices = array(r, |r| Ok((vec3(r)?, vec3(r)?)))?;
    r.i32()?;
    let colors = array(r, |r| Ok(r.array::<4>()?))?;
    r.i32()?;
    skip_array(r, 4)?;
    r.i32()?;
    let uvs = array(r, |r| {
        let uvs = array(r, |r| Ok([r.f32()?, r.f32()?]))?;
        r.bytes(8)?;
        Ok(uvs)
    })?;
    let indices = array(r, |r| Ok(r.u16()?))?;
    r.i32()?;
    skip_array(r, 2)?;
    r.i32()?;

    let collision_model = r.compact()?;
    // Plane and three side planes (16 floats), three vertex indices, material index.
    let (collision_triangles, collision_materials) = array(r, |r| {
        r.bytes(64)?;
        let vertices = [count(r)?, count(r)?, count(r)?].map(|i| i as u32);
        Ok((vertices, count(r)? as u32))
    })?
    .into_iter()
    .unzip();
    // Triangle index, coplanar index, two child indices, box.
    let collision_nodes = array(r, |r| {
        (0..4).try_for_each(|_| r.compact().map(drop))?;
        skip_box(r)
    })?
    .len();
    skip_lazy_array(r)?;
    r.i32()?;
    let physics_props = r.compact()?;

    if !r.at_end() {
        return Err(invalid("static mesh end", r.pos()));
    }
    let (positions, normals) = vertices.into_iter().unzip();
    Ok(StaticMesh {
        sections,
        positions,
        normals,
        colors,
        use_vertex_color,
        uvs,
        indices,
        collision_model,
        collision_triangles,
        collision_materials,
        collision_nodes,
        use_simple_box_collision,
        use_simple_line_collision,
        use_simple_karma_collision,
        physics_props,
    })
}

pub fn read_static_mesh_materials(pkg: &Package, file: &[u8], export: &Export) -> Result<Vec<i32>, Error> {
    let mut r = pkg.export_reader(file, export)?;
    let properties = read_properties(pkg, &mut r, &STATIC_MESH_LAYOUT)?;
    let Some(StoredValue::Array(materials)) = member(&properties, "Materials") else {
        return Ok(Vec::new());
    };
    let reference = |element: &StoredValue| match element {
        StoredValue::Struct(members) => match member(members, "Material") {
            Some(&StoredValue::Object(index)) => index,
            _ => 0,
        },
        _ => 0,
    };
    Ok(materials.iter().map(reference).collect())
}

#[derive(Debug, Clone, Default)]
pub struct StaticMeshInstance {
    pub colors: Vec<[u8; 4]>,
    pub lights: Vec<StaticMeshLight>,
}

#[derive(Debug, Clone)]
pub struct StaticMeshLight {
    pub actor: i32,
    /// One bit per mesh vertex (LSB first): the light reaches it (faces it and is not shadowed).
    pub visibility: Vec<u8>,
    pub applied: bool,
}

impl StaticMeshLight {
    pub fn reaches(&self, index: usize) -> bool {
        self.visibility.get(index / 8).is_some_and(|bits| bits & (1 << (index % 8)) != 0)
    }
}

pub fn read_static_mesh_instance(pkg: &Package, file: &[u8], export: &Export) -> Result<StaticMeshInstance, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;
    let colors = array(r, |r| Ok(r.array::<4>()?))?;
    r.i32()?;
    let lights = array(r, |r| {
        let actor = r.compact()?;
        let visibility_bytes = count(r)?;
        let visibility = r.bytes(visibility_bytes)?.to_vec();
        let applied = r.i32()? != 0;
        Ok(StaticMeshLight { actor, visibility, applied })
    })?;
    if !r.at_end() {
        return Err(invalid("static mesh instance end", r.pos()));
    }
    Ok(StaticMeshInstance { colors, lights })
}

fn read_section(r: &mut Reader) -> Result<MeshSection, Error> {
    Ok(MeshSection {
        is_strip: r.i32()? != 0,
        first_index: r.u16()?,
        min_vertex_index: r.u16()?,
        max_vertex_index: r.u16()?,
        num_triangles: r.u16()?,
        num_primitives: r.u16()?,
    })
}

fn skip_box(r: &mut Reader) -> Result<(), Error> {
    r.bytes(BOX_SIZE)?;
    Ok(())
}
