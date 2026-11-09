use p2_assets::{fan_triangles, poly_flags, read_level, read_model, read_terrain, read_texture, texture_heights};
use p2_collision::BspHulls;
use p2_package::PackageId;
use p2_script::{Objects, Source};
use crate::{Kind, LoaderRef, ObjectId, Prop, Vm};
use super::{LevelMesh, ShapeGrid, World, WorldShape, brush_corners, face_along};

impl<L: LoaderRef> Vm<L> {
    /// Static world-geometry actors block any pawn that collides with the world, whatever their
    /// `bBlockActors` / `bBlockPlayers`. Class-blocking volumes and cylinder actors stay with the others.
    pub(crate) fn build_world(&mut self, map: PackageId) {
        let loader = self.loader.clone();
        let loaded = loader.package(map);
        let (pkg, data) = (&loaded.package, &loaded.data[..]);
        let model = pkg
            .exports
            .iter()
            .find(|e| pkg.object_name(e.class_index) == Some("Level"))
            .and_then(|level| read_level(pkg, data, level).ok())
            .and_then(|level| pkg.exports.get(usize::try_from(level.model.checked_sub(1)?).ok()?))
            .and_then(|model| read_model(pkg, data, model).ok());
        let mut terrains = Vec::new();
        for (index, export) in pkg.exports.iter().enumerate() {
            if pkg.object_name(export.class_index) != Some("TerrainInfo") {
                continue;
            }
            let Ok(object) = self.load_object(map, index) else { continue };
            let Some(class) = self.store.object(object).class else { continue };
            let Ok(mut terrain) = read_terrain(pkg, data, export, &self.store.class(class).layout) else { continue };
            let heights = loader.resolve_loaded(map, terrain.heightmap).and_then(|found| {
                let loaded = loader.package(found.package);
                let source = Source { package: &loaded.package, data: &loaded.data };
                let texture = read_texture(&mut Objects::new(&*loader), source, found.export?).ok()?;
                texture_heights(&texture).map(|(_, _, heights)| heights)
            });
            if let Some(heights) = heights {
                terrain.vertices = terrain.vertices_from_heights(&heights);
                terrains.push(terrain);
            }
        }
        let zone_refs: Vec<i32> = model.as_ref().map(|model| model.zones.iter().map(|zone| zone.actor).collect()).unwrap_or_default();
        let zone_actors = zone_refs
            .into_iter()
            .map(|reference: i32| reference.checked_sub(1).and_then(|export| usize::try_from(export).ok()).and_then(|export| self.load_object(map, export).ok()))
            .collect();
        let hulls = model.as_ref().map(BspHulls::new).unwrap_or_default();
        self.world = World { map: Some(map), model, hulls, zone_actors, terrains, ..World::default() };
        self.hash = crate::hash::CollisionHash::default();
        let actors: Vec<_> = self.actors().collect();
        for actor in actors {
            let world_geometry = [Prop::bStatic, Prop::bWorldGeometry, Prop::bCollideActors].into_iter().all(|f| self.store.flag_of(actor, f));
            if !world_geometry || self.store.is_deleted(actor) || (self.store.is_a(actor, Kind::BlockingVolume) && self.store.flag_of(actor, Prop::bClassBlocker)) {
                continue;
            }
            let Some(shape) = self.actor_shape(actor) else { continue };
            if self.store.flag_of(actor, Prop::bBlockNonZeroExtentTraces) {
                self.world.boxes.push(WorldShape { shape: shape.boxes, model: shape.model, actor });
            }
            if self.store.flag_of(actor, Prop::bBlockZeroExtentTraces) {
                self.world.lines.push(WorldShape { shape: shape.lines, model: shape.model, actor });
            }
            self.world.static_actors.insert(actor);
        }
        self.world.box_grid = ShapeGrid::new(&self.world.boxes);
        self.world.line_grid = ShapeGrid::new(&self.world.lines);
    }

    /// As Karma, never static meshes.
    pub(crate) fn level_meshes(&self) -> Vec<LevelMesh> {
        const NF_NOT_CSG: u8 = 0x01;
        let mut meshes = Vec::new();
        let fan = |positions: &mut Vec<[f32; 3]>, triangles: &mut Vec<[u32; 3]>, polygon: &[[f32; 3]]| {
            let base = positions.len() as u32;
            positions.extend_from_slice(polygon);
            triangles.extend(fan_triangles(polygon.len()).map(|t| t.map(|i| base + i as u32)));
        };
        if let Some(model) = &self.world.model {
            let (mut positions, mut triangles) = (Vec::new(), Vec::new());
            let skip = poly_flags::PORTAL | poly_flags::FAKE_BACKDROP;
            for polygon in model.polygons() {
                let Some(node) = model.nodes.get(polygon.node).filter(|n| n.node_flags & NF_NOT_CSG == 0) else { continue };
                if polygon.poly_flags & skip == 0 {
                    let first = triangles.len();
                    fan(&mut positions, &mut triangles, &polygon.positions);
                    face_along(&positions, &mut triangles[first..], [node.plane[0], node.plane[1], node.plane[2]].into());
                }
            }
            meshes.push(LevelMesh { vertices: positions, triangles });
        }
        for terrain in &self.world.terrains {
            let triangles = terrain.karma_triangles().into_iter().map(|t| t.map(|i| i as u32)).collect();
            meshes.push(LevelMesh { vertices: terrain.vertices.clone(), triangles });
        }
        meshes.retain(|mesh| !mesh.triangles.is_empty());
        meshes
    }

    /// As retail, every actor with `bBlockKarma` gets its own Karma model, without draw scale for a
    /// `BlockingVolume`. A `KarmaBlockingVolume` does not collide or block, and traces skip it.
    pub(crate) fn karma_blocking_hulls(&mut self) -> Vec<Vec<[f32; 3]>> {
        let volumes: Vec<ObjectId> = self
            .actors()
            .filter(|&a| !self.store.is_deleted(a) && self.store.is_a(a, Kind::BlockingVolume) && self.store.flag_of(a, Prop::bBlockKarma))
            .collect();
        let mut hulls = Vec::new();
        for volume in volumes {
            let Some(model) = self.store.object_of(volume, Prop::Brush).and_then(|brush| self.brush_model(brush)) else { continue };
            let (location, pre_pivot) = (self.store.vector(volume, Prop::Location), self.store.vector(volume, Prop::PrePivot));
            let axes = self.store.rotator_of(volume, Prop::Rotation).axes();
            hulls.push(brush_corners(&model).map(|p| [0, 1, 2].map(|c| location[c] + (0..3).map(|k| axes[k][c] * (p[k] - pre_pivot[k])).sum::<f32>())).collect());
        }
        hulls
    }
}
