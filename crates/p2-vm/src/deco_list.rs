use p2_math::{Rotator, Vector};
use crate::{Encroachment, Error, LoaderRef, ObjectId, Prop, Value, Vm};

const FLOOR_TRACE: f32 = 524_288.0;

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn spawn_deco_list(&mut self, volume: ObjectId) -> Result<(), Error> {
        let Some(list) = self.store.object_of(volume, "DecoList") else { return Ok(()) };
        let Some(model) = self.store.object_of(volume, Prop::Brush).and_then(|brush| self.brush_model(brush)) else { return Ok(()) };
        let (min, max) = model
            .points
            .iter()
            .fold(None, |bounds: Option<([f32; 3], [f32; 3])>, p| {
                let p = [p[0], p[1], p[2]];
                let (lo, hi) = bounds.unwrap_or((p, p));
                Some(([0, 1, 2].map(|i| lo[i].min(p[i])), [0, 1, 2].map(|i| hi[i].max(p[i]))))
            })
            .unwrap_or_default();
        let placement = self.placement(volume);
        let axes = placement.rotation.axes();
        let to_world = move |p: Vector| {
            let local = [0, 1, 2].map(|k| (p[k] - placement.pre_pivot[k]) * placement.scale[k]);
            placement.location + axes[0] * local[0] + axes[1] * local[1] + axes[2] * local[2]
        };
        // The extent is taken from the box already placed in the world, then placed a second time with each point.
        let extent = (to_world(Vector::from(max)) - to_world(Vector::from(min))) * 0.5;
        let class = self.find_class("Engine", "DecoVolumeObject")?;
        for index in 0.. {
            let Some(Value::Struct(entry)) = self.store.element_of(list, "Decorations", index) else { break };
            let [Value::Object(mesh), count, draw_scale, align, pitch, yaw, roll] = entry.as_slice() else { continue };
            let count = in_range(self.random(), range(count));
            for _ in 0..count as i32 {
                let z = in_range(self.random(), [-extent.z, extent.z]);
                let y = in_range(self.random(), [-extent.y, extent.y]);
                let x = in_range(self.random(), [-extent.x, extent.x]);
                let placed = [Some(Value::Vector(to_world(Vector::new(x, y, z)))), Some(Value::Rotator(Rotator::default()))];
                let Some(actor) = self.spawn_actor(class, None, None, placed)? else { continue };
                self.store.set(actor, Prop::StaticMesh, Value::Object(*mesh));
                if !self.drop_to_floor(actor, set(align))? {
                    self.destroy(actor)?;
                    continue;
                }
                let mut rotation = self.store.rotator_of(actor, Prop::Rotation);
                for (flag, axis) in [(pitch, &mut rotation.pitch), (yaw, &mut rotation.yaw), (roll, &mut rotation.roll)] {
                    if set(flag) {
                        *axis = axis.wrapping_add(self.rand());
                    }
                }
                self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
                let scale = range(draw_scale);
                if scale != [0.0, 0.0] {
                    let scale = in_range(self.random(), scale);
                    self.store.set(actor, Prop::DrawScale, Value::Float(scale));
                    self.relocate(actor);
                }
            }
        }
        Ok(())
    }

    fn drop_to_floor(&mut self, actor: ObjectId, align: bool) -> Result<bool, Error> {
        let (radius, height) = self.store.collision_size(actor);
        let mut extent = Vector::new(radius, radius, height);
        if extent == Vector::ZERO
            && let Some(mesh) = self.store.object_of(actor, Prop::StaticMesh)
            && let Some([half_height, radius]) = self.mesh_extent(mesh)
        {
            extent = Vector::new(radius, radius, half_height);
        }
        let start = self.store.vector(actor, Prop::Location);
        let end = start - Vector::new(0.0, 0.0, FLOOR_TRACE);
        let Some(hit) = self.trace_world(start, end, extent) else { return Ok(false) };
        let on_level = self.trace_bsp(start, end, extent).is_some_and(|bsp| bsp.time <= hit.time);
        if on_level && self.world_encroached(hit.location, extent) {
            return Ok(false);
        }
        self.far_move_actor(actor, hit.location, Encroachment::Checked)?;
        if align {
            let mut rotation = hit.normal.rotation();
            rotation.pitch -= 16384;
            self.store.set(actor, Prop::Rotation, Value::Rotator(rotation));
        }
        Ok(true)
    }

    /// Half the height of the mesh's own bounding box and its bounding sphere's radius, unscaled.
    fn mesh_extent(&self, mesh: ObjectId) -> Option<[f32; 2]> {
        let (package, export) = self.loader.loaded_export_by_path_of_class(&self.store.object(mesh).path, "StaticMesh")?;
        let loaded = self.loader.package(package);
        let mut r = loaded.package.native_reader(&loaded.data, &loaded.package.exports[export]).ok()?;
        let mut bounds = [0.0f32; 6];
        for value in &mut bounds {
            *value = r.f32().ok()?;
        }
        r.bytes(1).ok()?;
        let mut sphere = [0.0f32; 4];
        for value in &mut sphere {
            *value = r.f32().ok()?;
        }
        Some([(bounds[5] - bounds[2]) * 0.5, sphere[3]])
    }
}

fn range(value: &Value) -> [f32; 2] {
    match value {
        Value::Struct(members) => match members.as_slice() {
            [Value::Float(min), Value::Float(max)] => [*min, *max],
            _ => [0.0, 0.0],
        },
        _ => [0.0, 0.0],
    }
}

fn in_range(frand: f32, [min, max]: [f32; 2]) -> f32 {
    frand * (min - max) + max
}

fn set(flag: &Value) -> bool {
    matches!(flag, Value::Int(value) if *value != 0)
}
