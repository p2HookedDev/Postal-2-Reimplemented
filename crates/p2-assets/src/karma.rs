use p2_package::{Export, Package, Reader};

use crate::Error;
use crate::read::{array, invalid, vec3};

#[derive(Debug, Clone, PartialEq)]
pub struct KMeshProps {
    pub inertia_tensor: [f32; 6],
    pub com_nudge: [f32; 3],
    pub geom: AggGeom,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AggGeom {
    pub spheres: Vec<SphereElem>,
    pub boxes: Vec<BoxElem>,
    pub cylinders: Vec<CylinderElem>,
    pub convexes: Vec<ConvexElem>,
}

pub type ElemTm = [[f32; 4]; 4];

#[derive(Debug, Clone, PartialEq)]
pub struct SphereElem {
    pub tm: ElemTm,
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoxElem {
    pub tm: ElemTm,
    pub size: [f32; 3],
}

/// Along the element's Z axis (unverified).
#[derive(Debug, Clone, PartialEq)]
pub struct CylinderElem {
    pub tm: ElemTm,
    pub radius: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConvexElem {
    pub tm: ElemTm,
    pub vertices: Vec<[f32; 3]>,
}

pub fn read_kmesh_props(pkg: &Package, file: &[u8], export: &Export) -> Result<KMeshProps, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;
    let mut inertia_tensor = [0.0; 6];
    for value in &mut inertia_tensor {
        *value = r.f32()?;
    }
    let com_nudge = vec3(r)?;
    let spheres = array(r, |r| Ok(SphereElem { tm: matrix(r)?, radius: r.f32()? }))?;
    let boxes = array(r, |r| Ok(BoxElem { tm: matrix(r)?, size: vec3(r)? }))?;
    let cylinders = array(r, |r| Ok(CylinderElem { tm: matrix(r)?, radius: r.f32()?, height: r.f32()? }))?;
    let convexes = array(r, |r| Ok(ConvexElem { tm: matrix(r)?, vertices: array(r, vec3)? }))?;
    if !r.at_end() {
        return Err(invalid("KMeshProps end", r.pos()));
    }
    Ok(KMeshProps { inertia_tensor, com_nudge, geom: AggGeom { spheres, boxes, cylinders, convexes } })
}

fn matrix(r: &mut Reader) -> Result<ElemTm, Error> {
    let mut m = [[0.0; 4]; 4];
    for value in m.iter_mut().flatten() {
        *value = r.f32()?;
    }
    Ok(m)
}
