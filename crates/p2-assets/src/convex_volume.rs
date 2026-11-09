use p2_package::{Export, Package};

use crate::Error;
use crate::read::{BOX_SIZE, array, invalid, skip_primitive, vec3, vec4};

#[derive(Debug, Clone, PartialEq)]
pub struct ConvexVolume {
    pub faces: Vec<ConvexFace>,
    pub edges: Vec<ConvexEdge>,
}

/// Plane as `[x, y, z, w]`, normal pointing out of the solid, and its vertices.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvexFace {
    pub plane: [f32; 4],
    pub vertices: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvexEdge {
    pub faces: [usize; 2],
    pub vertices: [usize; 2],
}

pub fn read_convex_volume(pkg: &Package, file: &[u8], export: &Export) -> Result<ConvexVolume, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;
    skip_primitive(r)?;
    let faces: Vec<ConvexFace> = array(r, |r| Ok(ConvexFace { plane: vec4(r)?, vertices: array(r, vec3)? }))?;
    let edges = array(r, |r| {
        let offset = r.pos();
        let index = |v: i32| usize::try_from(v).map_err(|_| invalid("convex volume edge", offset));
        Ok(ConvexEdge { faces: [index(r.i32()?)?, index(r.i32()?)?], vertices: [index(r.i32()?)?, index(r.i32()?)?] })
    })?;
    r.bytes(BOX_SIZE)?;
    let offset = r.pos();
    let in_range = |e: &ConvexEdge| (0..2).all(|i| faces.get(e.faces[i]).is_some_and(|f| e.vertices[i] < f.vertices.len()));
    if !edges.iter().all(in_range) {
        return Err(invalid("convex volume edge index", offset));
    }
    Ok(ConvexVolume { faces, edges })
}
