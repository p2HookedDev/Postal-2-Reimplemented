use p2_math::{Rotator, Vector};
use p2_package::Package;
use p2_script::{Objects, Resolve, Source};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EDrawType {
    Sprite = 1,
    Mesh = 2,
    StaticMesh = 8,
    Particle = 10,
    AntiPortal = 11,
}

impl EDrawType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Sprite,
            2 => Self::Mesh,
            8 => Self::StaticMesh,
            10 => Self::Particle,
            11 => Self::AntiPortal,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MeshActor<'a> {
    pub static_mesh: i32,
    /// The package `static_mesh` indexes: the actor's, or its class's when a class default sets the mesh.
    pub mesh_package: &'a Package,
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    /// Subtracted from mesh vertices before scaling and rotation.
    pub pre_pivot: Vector,
    pub instance: i32,
    pub unlit: bool,
}

pub fn read_mesh_actor<'a, R: Resolve + ?Sized>(
    objects: &mut Objects<'a, R>,
    src: Source<'a>,
    export: usize,
) -> Result<Option<MeshActor<'a>>, Error> {
    let object = objects.read(src, export)?;
    let Some((mesh_package, static_mesh)) = object.object("StaticMesh") else { return Ok(None) };
    if object.bool("bHidden") || EDrawType::from_byte(object.byte("DrawType")) != Some(EDrawType::StaticMesh) {
        return Ok(None);
    }
    let draw_scale = object.float("DrawScale");
    // Saved with the map only: a class default would index the class's package.
    let instance = object.object_in("StaticMeshInstance", src.package).unwrap_or(0);
    Ok(Some(MeshActor {
        static_mesh,
        mesh_package,
        location: object.vector("Location").into(),
        rotation: object.rotator("Rotation").into(),
        scale: Vector::from(object.vector("DrawScale3D")) * draw_scale,
        pre_pivot: object.vector("PrePivot").into(),
        instance,
        unlit: object.bool("bUnlit"),
    }))
}

pub fn read_actor_location<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<Vector, Error> {
    Ok(objects.read(src, export)?.vector("Location").into())
}

pub fn read_actor_rotation<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<Rotator, Error> {
    Ok(objects.read(src, export)?.rotator("Rotation").into())
}
