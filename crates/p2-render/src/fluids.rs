use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::prelude::{Add, Changed, Commands, Component, On, Query, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::math::{Affine2, Vec2, Vec3};
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology, VertexAttributeValues};
use p2_assets::{UvModifier, uv_matrix};
use p2_bevy_assets::to_bevy;
use p2_particles::FluidVertex;
use crate::scene::{MaterialImages, SectionView};
use crate::view::{ActorLook, ActorMaterials, SectionMaterial, image_size};

#[derive(Component)]
pub struct FluidDrawn {
    pub images: MaterialImages,
    pub vertices: Vec<FluidVertex>,
    pub indices: Vec<u32>,
    /// Game seconds its texture has scrolled for.
    pub seconds: f32,
}

#[derive(Component)]
struct FluidDraw {
    mesh: Handle<Mesh>,
    material: SectionMaterial,
    uv: Vec<UvModifier>,
    uv_size: [f32; 2],
}

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FluidSystems;

pub struct FluidPlugin;

impl Plugin for FluidPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(add_fluid_draw).add_systems(PostUpdate, refill_fluids.in_set(FluidSystems));
    }
}

fn add_fluid_draw(
    add: On<Add<FluidDrawn>>,
    mut commands: Commands,
    fluids: Query<&FluidDrawn>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    mut materials: ActorMaterials,
) {
    let entity = add.event().entity;
    let Ok(fluid) = fluids.get(entity) else { return };
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    fill_fluid(&mut mesh, &fluid.vertices, &fluid.indices);
    let mesh = meshes.add(mesh);
    let uv_size = image_size(&images, &fluid.images.image);
    let mut section = SectionView::textured(mesh.clone(), Some(fluid.images.clone()));
    section.no_scene_lights = true;
    section.two_sided = true;
    let material = materials.material(ActorLook::Fluid, &section);
    let mut draw = commands.entity(entity);
    draw.insert((Mesh3d(mesh.clone()), NoFrustumCulling, FluidDraw { mesh, material: material.clone(), uv: section.uv, uv_size }));
    material.insert(&mut draw);
}

/// The vertex colour is the light, 1 being neutral. No water skin reads the simulation's height alpha; retail's
/// use of it is (unverified).
fn fill_fluid(mesh: &mut Mesh, vertices: &[FluidVertex], indices: &[u32]) {
    let (positions, normals, colors, uvs, indices): (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<[f32; 2]>, Vec<u32>) = if vertices.is_empty() || indices.is_empty() {
        (vec![[0.0; 3]], vec![[0.0, 1.0, 0.0]], vec![[0.0; 4]], vec![[0.0; 2]], vec![0, 0, 0])
    } else {
        (
            vertices.iter().map(|v| to_bevy(v.position)).collect(),
            vertices.iter().map(|v| Vec3::from(to_bevy(v.normal)).normalize_or_zero().to_array()).collect(),
            vec![[1.0; 4]; vertices.len()],
            vertices.iter().map(|v| v.uv).collect(),
            indices.to_vec(),
        )
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

fn refill_fluid(mesh: &mut Mesh, vertices: &[FluidVertex]) {
    if vertices.is_empty() || mesh.count_vertices() != vertices.len() {
        return;
    }
    if let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION) {
        for (position, v) in positions.iter_mut().zip(vertices) {
            *position = to_bevy(v.position);
        }
    }
    if let Some(VertexAttributeValues::Float32x3(normals)) = mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL) {
        for (normal, v) in normals.iter_mut().zip(vertices) {
            *normal = Vec3::from(to_bevy(v.normal)).normalize_or_zero().to_array();
        }
    }
}

fn refill_fluids(changed: Query<(&FluidDrawn, &FluidDraw), Changed<FluidDrawn>>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ActorMaterials) {
    for (fluid, draw) in &changed {
        if let Some(mut mesh) = meshes.get_mut(&draw.mesh) {
            p2_host::frame_times::mesh_written(p2_host::frame_times::MeshWrite::Fluid);
            refill_fluid(&mut mesh, &fluid.vertices);
        }
        if !draw.uv.is_empty() {
            let [[a, b, c], [d, e, f]] = uv_matrix(&draw.uv, fluid.seconds, draw.uv_size);
            materials.set_uv(&draw.material, Affine2::from_cols(Vec2::new(a, d), Vec2::new(b, e), Vec2::new(c, f)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetApp;
    use bevy::pbr::StandardMaterial;
    use p2_math::Vector;

    fn vertex(x: f32) -> FluidVertex {
        FluidVertex { position: Vector::new(x, 0.0, 0.0), normal: Vector::new(0.0, 0.0, 1.0), alpha: 255, uv: [0.0; 2] }
    }

    #[test]
    fn a_surface_handed_over_gets_a_mesh_that_follows_its_vertices() {
        let mut app = App::new();
        app.add_plugins((bevy::MinimalPlugins, bevy::asset::AssetPlugin::default(), FluidPlugin))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<crate::mesh_light::MeshLitMaterial>()
            .init_resource::<crate::view::ActorMaterialCache>();
        let images = MaterialImages { image: Handle::default(), glow: None, detail: None, specular: None, state: Default::default() };
        let drawn = FluidDrawn { images, vertices: vec![vertex(0.0), vertex(1.0), vertex(2.0)], indices: vec![0, 1, 2], seconds: 0.0 };
        let fluid = app.world_mut().spawn(drawn).id();
        app.update();
        let first_x = |app: &App| {
            let mesh = app.world().get::<Mesh3d>(fluid).expect("a mesh on the surface");
            let mesh = app.world().resource::<Assets<Mesh>>().get(&mesh.0).expect("the mesh asset");
            match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
                Some(VertexAttributeValues::Float32x3(positions)) => positions.iter().map(|p| p[0]).collect::<Vec<f32>>(),
                _ => Vec::new(),
            }
        };
        assert_eq!(first_x(&app), [0.0, 1.0, 2.0]);
        app.world_mut().get_mut::<FluidDrawn>(fluid).expect("the surface").vertices[0] = vertex(5.0);
        app.update();
        assert_eq!(first_x(&app), [5.0, 1.0, 2.0]);
    }
}
