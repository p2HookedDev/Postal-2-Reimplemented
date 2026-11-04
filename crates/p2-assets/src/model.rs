use p2_math::Vector;
use p2_package::{Export, Package, Reader};

use crate::read::{array, count, invalid, lazy_bytes, skip_primitive, vec3};
use crate::{Error, Mip, Texture, TextureFormat};

const MIN_VERSION: u16 = 116;

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub vectors: Vec<[f32; 3]>,
    pub points: Vec<[f32; 3]>,
    pub nodes: Vec<BspNode>,
    pub surfaces: Vec<BspSurface>,
    pub verts: Vec<Vert>,
    pub num_shared_sides: i32,
    pub zones: Vec<Zone>,
    pub polys: i32,
    pub bounds: Vec<Aabb>,
    pub leaf_hulls: Vec<i32>,
    pub leaves: Vec<Leaf>,
    pub lights: Vec<i32>,
    pub root_outside: bool,
    pub linked: bool,
    pub sections: Vec<BspSection>,
    pub light_maps: Vec<LightMap>,
    pub light_map_textures: Vec<LightMapTexture>,
}

#[derive(Debug, Clone, Copy)]
pub struct BspNode {
    pub plane: [f32; 4],
    pub node_flags: u8,
    pub vert_pool: usize,
    pub surface: usize,
    pub back: Option<usize>,
    pub front: Option<usize>,
    pub coplanar: Option<usize>,
    pub collision_bound: Option<usize>,
    pub render_bound: Option<usize>,
    pub zones: [u8; 2],
    pub num_vertices: u8,
    pub leaves: [i32; 2],
    pub section: Option<usize>,
    pub first_vertex: i32,
    pub light_map: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct BspSurface {
    /// Object reference to the material (0 = none).
    pub material: i32,
    pub poly_flags: u32,
    pub base: usize,
    pub normal: usize,
    pub texture_u: usize,
    pub texture_v: usize,
    pub brush_poly: Option<usize>,
    pub actor: i32,
    pub plane: [f32; 4],
    pub light_map_scale: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Vert {
    pub point: usize,
    pub side: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct Zone {
    pub actor: i32,
    pub last_render_time: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub valid: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Leaf {
    pub zone: i32,
    pub permeating: i32,
    pub volumetric: i32,
    pub visible_zones: u64,
}

#[derive(Debug, Clone)]
pub struct BspSection {
    pub vertices: Vec<BspVertex>,
    pub material: i32,
    pub num_nodes: i32,
    pub poly_flags: u32,
    /// Index into [`Model::light_map_textures`] (-1 = none).
    pub light_map_texture: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct BspVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub light_map_uv: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct LightMap {
    pub texture: i32,
    pub surface: i32,
    /// Zone of the lit nodes (unverified).
    pub zone: i32,
    pub offset: [i32; 2],
    pub size: [i32; 2],
    pub world_to_light_map: [f32; 16],
    pub base: [f32; 3],
    pub axis_x: [f32; 3],
    pub axis_y: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct LightMapTexture {
    pub light_maps: Vec<i32>,
    /// Texture format byte (3 = DXT1 in the install). Like `width` and `height`, only meaningful
    /// when `data` is non-empty: unbuilt textures store uninitialised values.
    pub format: u8,
    pub width: i32,
    pub height: i32,
    pub data: [Vec<u8>; 2],
}

#[derive(Debug, Clone)]
pub struct Poly {
    pub base: [f32; 3],
    pub normal: [f32; 3],
    pub texture_u: [f32; 3],
    pub texture_v: [f32; 3],
    pub vertices: Vec<[f32; 3]>,
    pub poly_flags: u32,
    pub actor: i32,
    pub material: i32,
    pub item_name: usize,
    pub link: i32,
    pub brush_poly: i32,
    pub light_map_scale: f32,
}

/// Poly flag bits a renderer needs to skip surfaces. Standard UE2 values (unverified for POSTAL 2).
pub mod poly_flags {
    pub const INVISIBLE: u32 = 0x0000_0001;
    pub const FAKE_BACKDROP: u32 = 0x0000_0080;
    pub const PORTAL: u32 = 0x0400_0000;
    pub const ANTI_PORTAL: u32 = 0x0800_0000;
    pub const MASKED: u32 = 0x0000_0002;
    pub const TRANSLUCENT: u32 = 0x0000_0004;
    pub const TWO_SIDED: u32 = 0x0000_0100;
    /// Not lit (sky and cloud surfaces): drawn without a light map and without dynamic light passes.
    pub const UNLIT: u32 = 0x0040_0000;
}

#[derive(Debug, Clone)]
pub struct ModelPolygon {
    pub node: usize,
    pub surface: usize,
    pub material: i32,
    pub poly_flags: u32,
    pub normal: [f32; 3],
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates in texels: divide by the material's width and height.
    pub uvs: Vec<[f32; 2]>,
}

pub fn read_model(pkg: &Package, file: &[u8], export: &Export) -> Result<Model, Error> {
    check_version(pkg)?;
    let licensee = pkg.summary.licensee;
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;

    skip_primitive(r)?;

    let vectors = array(r, vec3)?;
    let points = array(r, vec3)?;
    let nodes = array(r, |r| read_node(r, licensee))?;
    let surfaces = array(r, read_surface)?;
    let verts = array(r, |r| Ok(Vert { point: index(r)?, side: optional(r)? }))?;
    let num_shared_sides = r.i32()?;
    let num_zones = r.i32()?;
    let zones = (0..num_zones)
        .map(|_| {
            let actor = r.compact()?;
            skip_zone_set(r, licensee)?;
            skip_zone_set(r, licensee)?;
            Ok(Zone { actor, last_render_time: r.f32()? })
        })
        .collect::<Result<_, Error>>()?;
    let polys = r.compact()?;

    let bounds = array(r, read_box)?;
    let leaf_hulls = array(r, |r| Ok(r.i32()?))?;
    let leaves = array(r, |r| {
        Ok(Leaf {
            zone: r.compact()?,
            permeating: r.compact()?,
            volumetric: r.compact()?,
            visible_zones: u64::from_le_bytes(r.array()?),
        })
    })?;
    let lights = array(r, |r| Ok(r.compact()?))?;
    let root_outside = r.i32()? != 0;
    let linked = r.i32()? != 0;

    let sections = array(r, read_section)?;
    let light_maps = array(r, read_light_map)?;
    let light_map_textures = array(r, read_light_map_texture)?;

    if !r.at_end() {
        return Err(invalid("model end", r.pos()));
    }
    let model = Model {
        vectors,
        points,
        nodes,
        surfaces,
        verts,
        num_shared_sides,
        zones,
        polys,
        bounds,
        leaf_hulls,
        leaves,
        lights,
        root_outside,
        linked,
        sections,
        light_maps,
        light_map_textures,
    };
    model.validate(export.serial_offset)?;
    Ok(model)
}

pub fn read_polys(pkg: &Package, file: &[u8], export: &Export) -> Result<Vec<Poly>, Error> {
    check_version(pkg)?;
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;
    let offset = r.pos();
    let num = usize::try_from(r.i32()?).map_err(|_| invalid("polys count", offset))?;
    r.i32()?;
    let polys = (0..num).map(|_| read_poly(r, pkg)).collect::<Result<_, _>>()?;
    if !r.at_end() {
        return Err(invalid("polys end", r.pos()));
    }
    Ok(polys)
}

impl LightMap {
    /// Lumel coordinates of (x, y, z, w) · `world_to_light_map` (columns 0 and 1): w is 1 for a point, 0 for a direction.
    pub fn lumel(&self, [x, y, z]: [f32; 3], w: f32) -> [f32; 2] {
        let m = &self.world_to_light_map;
        [x * m[0] + y * m[4] + z * m[8] + w * m[12], x * m[1] + y * m[5] + z * m[9] + w * m[13]]
    }
}

impl LightMapTexture {
    /// The atlas as a texture: format and size as saved, `data` as mips 0 and 1 (DXT1 512² in every lit map).
    /// Unlit levels store no data and meaningless format / size values: `None`.
    pub fn texture(&self) -> Option<Texture> {
        let (width, height) = (u32::try_from(self.width).ok()?, u32::try_from(self.height).ok()?);
        if self.data[0].is_empty() || width == 0 || height == 0 {
            return None;
        }
        let format = TextureFormat::from_byte(self.format).ok()?;
        let mips = self
            .data
            .iter()
            .enumerate()
            .filter(|(_, data)| !data.is_empty())
            .map(|(level, data)| Mip { width: (width >> level).max(1), height: (height >> level).max(1), data: data.clone() })
            .collect();
        Some(Texture { format, palette: None, mips, masked: false, clamp: [false; 2], lod_set: 0 })
    }
}

impl Model {
    /// Light map atlas index and per-vertex atlas UVs (0..1) of a polygon; `None` if its node has no light map or its
    /// section is [`poly_flags::UNLIT`].
    /// Light map texel = (x, y, z, 1) · `world_to_light_map` (columns 0 and 1), then offset into the atlas.
    pub fn light_map_uvs(&self, polygon: &ModelPolygon) -> Option<(usize, Vec<[f32; 2]>)> {
        let node = self.nodes.get(polygon.node)?;
        if node.section.and_then(|section| self.sections.get(section)).is_some_and(|section| section.poly_flags & poly_flags::UNLIT != 0) {
            return None;
        }
        let map = self.light_maps.get(node.light_map?)?;
        let atlas_index = usize::try_from(map.texture).ok()?;
        let atlas = self.light_map_textures.get(atlas_index)?;
        let (width, height) = (atlas.width as f32, atlas.height as f32);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let (offset_u, offset_v) = (map.offset[0] as f32, map.offset[1] as f32);
        let uvs = polygon
            .positions
            .iter()
            .map(|&point| {
                let [u, v] = map.lumel(point, 1.0);
                [(offset_u + u) / width, (offset_v + v) / height]
            })
            .collect();
        Some((atlas_index, uvs))
    }

    pub fn polygons(&self) -> Vec<ModelPolygon> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.num_vertices > 0)
            .filter_map(|(i, node)| self.polygon(i, node))
            .collect()
    }

    fn polygon(&self, node_index: usize, node: &BspNode) -> Option<ModelPolygon> {
        let surface = self.surfaces.get(node.surface)?;
        let base = *self.points.get(surface.base)?;
        let texture_u = *self.vectors.get(surface.texture_u)?;
        let texture_v = *self.vectors.get(surface.texture_v)?;
        let verts = self.verts.get(node.vert_pool..node.vert_pool + usize::from(node.num_vertices))?;
        let positions = verts.iter().map(|v| self.points.get(v.point).copied()).collect::<Option<Vec<_>>>()?;
        let uvs = positions
            .iter()
            .map(|&p| {
                let d = Vector::from(p) - Vector::from(base);
                [d.dot(texture_u.into()), d.dot(texture_v.into())]
            })
            .collect();
        Some(ModelPolygon {
            node: node_index,
            surface: node.surface,
            material: surface.material,
            poly_flags: surface.poly_flags,
            normal: *self.vectors.get(surface.normal)?,
            positions,
            uvs,
        })
    }

    fn validate(&self, offset: usize) -> Result<(), Error> {
        let in_range = |i: Option<usize>, len: usize| i.is_none_or(|i| i < len);
        for node in &self.nodes {
            let ok = node.surface < self.surfaces.len()
                && node.vert_pool + usize::from(node.num_vertices) <= self.verts.len()
                && [node.back, node.front, node.coplanar].iter().all(|&i| in_range(i, self.nodes.len()))
                && in_range(node.collision_bound, self.leaf_hulls.len())
                && in_range(node.render_bound, self.bounds.len())
                && in_range(node.section, self.sections.len())
                && in_range(node.light_map, self.light_maps.len());
            if !ok {
                return Err(invalid("BSP node index", offset));
            }
        }
        for s in &self.surfaces {
            let ok = s.base < self.points.len()
                && [s.normal, s.texture_u, s.texture_v].iter().all(|&i| i < self.vectors.len());
            if !ok {
                return Err(invalid("BSP surface index", offset));
            }
        }
        // Only vertices in node pools: the array can hold stale entries past them.
        let shared_sides = usize::try_from(self.num_shared_sides).unwrap_or(0);
        for node in &self.nodes {
            let pool = &self.verts[node.vert_pool..node.vert_pool + usize::from(node.num_vertices)];
            if pool.iter().any(|v| v.point >= self.points.len() || !in_range(v.side, shared_sides)) {
                return Err(invalid("BSP vertex index", offset));
            }
        }
        Ok(())
    }
}

pub fn fan_triangles(n: usize) -> impl Iterator<Item = [usize; 3]> {
    (1..n.saturating_sub(1)).map(|i| [0, i, i + 1])
}

fn check_version(pkg: &Package) -> Result<(), Error> {
    let version = pkg.summary.version;
    if version < MIN_VERSION {
        return Err(p2_package::Error::UnsupportedVersion(version).into());
    }
    Ok(())
}

fn read_node(r: &mut Reader, licensee: u16) -> Result<BspNode, Error> {
    let plane = vec4(r)?;
    skip_zone_set(r, licensee)?;
    let node_flags = r.u8()?;
    let vert_pool = index(r)?;
    let surface = index(r)?;
    let back = optional(r)?;
    let front = optional(r)?;
    let coplanar = optional(r)?;
    let collision_bound = optional(r)?;
    let render_bound = optional(r)?;
    r.bytes(32)?;  // two spheres (version >= 70), unverified: exclusive sphere bounds
    let zones = [r.u8()?, r.u8()?];
    let num_vertices = r.u8()?;
    let leaves = [r.i32()?, r.i32()?];
    let section = optional_int(r)?;
    let first_vertex = r.i32()?;
    let light_map = optional_int(r)?;
    Ok(BspNode {
        plane,
        node_flags,
        vert_pool,
        surface,
        back,
        front,
        coplanar,
        collision_bound,
        render_bound,
        zones,
        num_vertices,
        leaves,
        section,
        first_vertex,
        light_map,
    })
}

fn read_surface(r: &mut Reader) -> Result<BspSurface, Error> {
    Ok(BspSurface {
        material: r.compact()?,
        poly_flags: r.u32()?,
        base: index(r)?,
        normal: index(r)?,
        texture_u: index(r)?,
        texture_v: index(r)?,
        brush_poly: optional(r)?,
        actor: r.compact()?,
        plane: vec4(r)?,
        light_map_scale: r.f32()?,
    })
}

fn read_section(r: &mut Reader) -> Result<BspSection, Error> {
    let vertices = array(r, |r| {
        let position = vec3(r)?;
        let uv = [r.f32()?, r.f32()?];
        let light_map_uv = [r.f32()?, r.f32()?];
        let normal = vec3(r)?;
        Ok(BspVertex { position, normal, uv, light_map_uv })
    })?;
    r.i32()?;
    Ok(BspSection {
        vertices,
        material: r.compact()?,
        num_nodes: r.i32()?,
        poly_flags: r.u32()?,
        light_map_texture: r.i32()?,
    })
}

fn read_light_map(r: &mut Reader) -> Result<LightMap, Error> {
    let texture = r.compact()?;
    let surface = r.compact()?;
    let zone = r.compact()?;
    let offset = [r.compact()?, r.compact()?];
    let size = [r.compact()?, r.compact()?];
    let mut world_to_light_map = [0.0; 16];
    for v in &mut world_to_light_map {
        *v = r.f32()?;
    }
    let base = vec3(r)?;
    let axis_x = vec3(r)?;
    let axis_y = vec3(r)?;
    array(r, |r| {
        r.compact()?;
        let n = count(r)?;
        r.bytes(n)?;
        r.bytes(8)?;
        r.i32()?;
        r.bytes(16)?;
        Ok(())
    })?;
    r.compact()?;
    r.i32()?;
    Ok(LightMap { texture, surface, zone, offset, size, world_to_light_map, base, axis_x, axis_y })
}

fn read_light_map_texture(r: &mut Reader) -> Result<LightMapTexture, Error> {
    r.compact()?;
    let light_maps = array(r, |r| Ok(r.i32()?))?;
    r.bytes(8)?;
    r.i32()?;
    let data = [lazy_bytes(r)?.to_vec(), lazy_bytes(r)?.to_vec()];
    let format = r.u8()?;
    let width = r.i32()?;
    let height = r.i32()?;
    r.i32()?;
    Ok(LightMapTexture { light_maps, format, width, height, data })
}

fn read_poly(r: &mut Reader, pkg: &Package) -> Result<Poly, Error> {
    let offset = r.pos();
    let num_vertices = count(r)?;
    if num_vertices > 16 {
        return Err(invalid("poly vertex count", offset));
    }
    let base = vec3(r)?;
    let normal = vec3(r)?;
    let texture_u = vec3(r)?;
    let texture_v = vec3(r)?;
    let vertices = (0..num_vertices).map(|_| vec3(r)).collect::<Result<_, _>>()?;
    let poly_flags = r.u32()?;
    let actor = r.compact()?;
    let material = r.compact()?;
    let item_name = pkg.read_name(r)?;
    Ok(Poly {
        base,
        normal,
        texture_u,
        texture_v,
        vertices,
        poly_flags,
        actor,
        material,
        item_name,
        link: r.compact()?,
        brush_poly: r.compact()?,
        light_map_scale: r.f32()?,
    })
}

/// Zone set: licensee >= 1 stores an `INT` byte count and that many mask bytes (256 zones);
/// licensee 0 stores a `QWORD`.
fn skip_zone_set(r: &mut Reader, licensee: u16) -> Result<(), Error> {
    if licensee == 0 {
        r.bytes(8)?;
    } else {
        let offset = r.pos();
        let n = usize::try_from(r.i32()?).map_err(|_| invalid("zone set size", offset))?;
        r.bytes(n)?;
    }
    Ok(())
}

fn read_box(r: &mut Reader) -> Result<Aabb, Error> {
    Ok(Aabb { min: vec3(r)?, max: vec3(r)?, valid: r.u8()? != 0 })
}

fn index(r: &mut Reader) -> Result<usize, Error> {
    let offset = r.pos();
    usize::try_from(r.compact()?).map_err(|_| invalid("index", offset))
}

fn optional(r: &mut Reader) -> Result<Option<usize>, Error> {
    let offset = r.pos();
    to_optional(r.compact()?, offset)
}

fn optional_int(r: &mut Reader) -> Result<Option<usize>, Error> {
    let offset = r.pos();
    to_optional(r.i32()?, offset)
}

fn to_optional(value: i32, offset: usize) -> Result<Option<usize>, Error> {
    match value {
        -1 => Ok(None),
        v => usize::try_from(v).map(Some).map_err(|_| invalid("index", offset)),
    }
}

fn vec4(r: &mut Reader) -> Result<[f32; 4], Error> {
    Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlit_section_has_no_light_map() {
        let atlas = LightMapTexture { light_maps: vec![0], format: 3, width: 4, height: 4, data: [Vec::new(), Vec::new()] };
        let map = LightMap { texture: 0, surface: 0, zone: 0, offset: [0; 2], size: [4; 2], world_to_light_map: [0.0; 16], base: [0.0; 3], axis_x: [0.0; 3], axis_y: [0.0; 3] };
        let polygon = ModelPolygon { node: 0, surface: 0, material: 0, poly_flags: 0, normal: [0.0; 3], positions: vec![[0.0; 3]], uvs: vec![[0.0; 2]] };
        let node = BspNode {
            plane: [0.0; 4],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [0; 2],
            num_vertices: 0,
            leaves: [0; 2],
            section: Some(0),
            first_vertex: 0,
            light_map: Some(0),
        };
        let model = |poly_flags| {
            let section = BspSection { vertices: Vec::new(), material: 0, num_nodes: 0, poly_flags, light_map_texture: 0 };
            Model { nodes: vec![node], sections: vec![section], light_maps: vec![map.clone()], light_map_textures: vec![atlas.clone()], ..Model::default() }
        };
        assert!(model(0).light_map_uvs(&polygon).is_some());
        assert!(model(poly_flags::UNLIT).light_map_uvs(&polygon).is_none());
    }

    #[test]
    fn fan() {
        assert_eq!(fan_triangles(4).collect::<Vec<_>>(), [[0, 1, 2], [0, 2, 3]]);
        assert_eq!(fan_triangles(2).count(), 0);
        assert_eq!(fan_triangles(0).count(), 0);
    }

    #[test]
    fn zone_set() {
        let mut data = 32i32.to_le_bytes().to_vec();
        data.extend([0xff; 32]);
        let mut r = Reader::new(&data);
        skip_zone_set(&mut r, 2).unwrap();
        assert!(r.at_end());
        let mut r = Reader::new(&[0; 8]);
        skip_zone_set(&mut r, 0).unwrap();
        assert!(r.at_end());
    }
}
