use p2_math::{Rotator, Vector};
use p2_package::Package;
use p2_script::{Objects, Resolve, Source};

use crate::Error;

const DT_STATIC_MESH: u8 = 8;

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
    if object.bool("bHidden") || object.byte("DrawType") != DT_STATIC_MESH {
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
