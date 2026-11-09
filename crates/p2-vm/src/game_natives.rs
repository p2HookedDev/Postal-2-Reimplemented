//! The host simulates emitters, so these natives set script properties and queue the rest.

use p2_math::Vector;
use crate::native_id::Native;
use crate::natives::vec3;

use crate::{Error, HostCommand, Kind, LoaderRef, ObjectId, Prop, Value, Vm};

const LENGTH_SPAWN_LIMIT: usize = 5;
const DEFAULT_MAP_EXT: &str = "fuk";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParticleCommand<Properties = ()> {
    Reset(ObjectId, Properties),
    /// `SpawnParticle(ParticleIndex, 0, ...)`. The engine spawns nothing while the emitter has no `Owner` (not ticked yet)
    /// or no free slot count. `line`: `LineStart` / `LineEnd` at that moment (`SpawnParticleLength` moves `LineStart`
    /// between its particles).
    Spawn { emitter: ObjectId, offset: Vector, line: Option<[Vector; 2]> },
    /// `AllParticlesDead` cleared without a spawn: `SpawnParticleLength` does so on every call (emitter with slots),
    /// so a trail still too short for its first particle is not taken for finished (`AutoDestroy`).
    KeepAlive(ObjectId),
    CapTime { emitter: ObjectId, time: f32 },
    Kill(ObjectId),
}

impl ParticleCommand {
    pub fn with_properties<Properties>(self, read: impl FnOnce(ObjectId) -> Properties) -> ParticleCommand<Properties> {
        match self {
            Self::Reset(emitter, ()) => ParticleCommand::Reset(emitter, read(emitter)),
            Self::Spawn { emitter, offset, line } => ParticleCommand::Spawn { emitter, offset, line },
            Self::KeepAlive(emitter) => ParticleCommand::KeepAlive(emitter),
            Self::CapTime { emitter, time } => ParticleCommand::CapTime { emitter, time },
            Self::Kill(actor) => ParticleCommand::Kill(actor),
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn game_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let f = |n: usize| args.get(n).map_or(0.0, crate::interp::float);
        let v = |n: usize| vec3(args.get(n));
        let s = |n: usize| match args.get(n) {
            Some(Value::Str(text)) => text.as_str(),
            _ => "",
        };
        Ok(Some(match native {
            Native::SafeToGoIntoStasis => Value::Bool(this.is_none_or(|this| self.safe_to_go_into_stasis(this, f(0)))),
            Native::GetGameMap => {
                let dir = args.get(2).map_or(0, crate::interp::int);
                Value::Str(game_map(&self.map_files(), s(0), s(1), dir))
            }
            Native::PointToLineDist => {
                let Some((distance, u)) = point_to_line_dist(v(0), v(1), v(2)) else { return Ok(Some(Value::Bool(false))) };
                if let Some(out) = args.get_mut(3) {
                    *out = Value::Float(distance);
                }
                if let Some(out) = args.get_mut(4) {
                    *out = Value::Float(u);
                }
                Value::Bool(true)
            }
            Native::VectorsInFuzz => Value::Bool(vectors_in_fuzz(v(0), v(1), f(2))),
            Native::ThickLineCylinderCollide => Value::Bool(thick_line_cylinder_collide(v(0), v(1), f(2), f(3), v(4), f(5), f(6))),
            Native::FireHurtRadius => {
                if let Some(this) = this {
                    let damage_type = args.get(2).cloned().unwrap_or(Value::Object(None));
                    self.fire_hurt_radius(this, f(0), f(1), damage_type, [v(3), v(4)], [f(5), f(6)])?;
                }
                return Ok(None);
            }
            _ => {
                if let Some(this) = this {
                    self.emitter_native(native, this, args);
                }
                return Ok(None);
            }
        }))
    }

    fn emitter_native(&mut self, native: Native, emitter: ObjectId, args: &[Value]) {
        let f = |n: usize| args.get(n).map_or(0.0, crate::interp::float);
        match native {
            Native::ForceInit => {
                self.store.set(emitter, Prop::ActiveParticles, Value::Int(0));
                self.initialize_projection_normal(emitter);
                self.host.commands.push(HostCommand::Particle(ParticleCommand::Reset(emitter, ())));
            }
            Native::ForceSpawn => {
                let offset = self.store.vector(emitter, Prop::StartLocationOffset);
                self.host.commands.push(HostCommand::Particle(ParticleCommand::Spawn { emitter, offset, line: None }));
                let max = self.store.get(emitter, Prop::MaxParticles).as_ref().map_or(0, crate::interp::int);
                let active = self.store.get(emitter, Prop::ActiveParticles).as_ref().map_or(0, crate::interp::int);
                if max > 0 {
                    self.store.set(emitter, Prop::ActiveParticles, Value::Int((active + 1).min(max)));
                }
            }
            Native::SetProjectionNormal => {
                let value = vec3(args.first());
                self.set_vector(emitter, Prop::ProjectionNormal, value);
                self.set_vector(emitter, Prop::RealProjectionNormal, value.safe_normal());
            }
            Native::SetMaxParticles => {
                self.store.set(emitter, Prop::MaxParticles, args.first().cloned().unwrap_or(Value::Int(0)));
                self.store.set(emitter, Prop::ActiveParticles, Value::Int(0));
                self.initialize_projection_normal(emitter);
                self.host.commands.push(HostCommand::Particle(ParticleCommand::Reset(emitter, ())));
            }
            Native::SetLiveParticleTimes => self.host.commands.push(HostCommand::Particle(ParticleCommand::CapTime { emitter, time: f(0) })),
            Native::SpawnParticleLength => self.spawn_particle_length(emitter, vec3(args.first()), vec3(args.get(1)), f(2)),
            _ => {}
        }
    }

    fn initialize_projection_normal(&mut self, emitter: ObjectId) {
        let normal = self.store.vector(emitter, Prop::ProjectionNormal).safe_normal();
        self.set_vector(emitter, Prop::RealProjectionNormal, normal);
    }

    /// Retail's slot guard wraps, so a full emitter goes on spawning over its oldest particles.
    /// `ActiveParticles` saturates at `MaxParticles`; a full pee trail makes `FluidFeeder` start another.
    fn spawn_particle_length(&mut self, emitter: ObjectId, start: Vector, end: Vector, size_step: f32) {
        let length = end - start;
        // As retail, a vector of squared size below 1e-8 is left unnormalized.
        let step = (if length.size_squared() < 1e-8 { length } else { length.safe_normal() }) * size_step;
        let max = self.store.get(emitter, Prop::MaxParticles).as_ref().map_or(0, crate::interp::int);
        let mut active = self.store.get(emitter, Prop::ActiveParticles).as_ref().map_or(0, crate::interp::int);
        let mut offset = Vector::ZERO;
        let mut spawned = 0;
        while spawned < LENGTH_SPAWN_LIMIT && max > 0 && (offset - length).size() > size_step {
            self.set_vector(emitter, Prop::StartLocationOffset, offset);
            let line_start = self.store.vector(emitter, Prop::LineStart);
            let line = Some([line_start, self.store.vector(emitter, Prop::LineEnd)]);
            self.host.commands.push(HostCommand::Particle(ParticleCommand::Spawn { emitter, offset, line }));
            offset += step;
            self.set_vector(emitter, Prop::LineStart, line_start + step);
            spawned += 1;
            if active < max {
                active += 1;
            }
        }
        if spawned > 0 {
            self.set_vector(emitter, Prop::LineEnd, end);
            self.store.set(emitter, Prop::ActiveParticles, Value::Int(active));
        }
        if max > 0 {
            self.host.commands.push(HostCommand::Particle(ParticleCommand::KeepAlive(emitter)));
        }
    }

    fn safe_to_go_into_stasis(&self, controller: ObjectId, min_player_distance: f32) -> bool {
        let Some(pawn) = self.store.object_of(controller, Prop::Pawn) else { return true };
        let at = self.store.vector(pawn, Prop::Location);
        !self.any_controller(|other| {
            self.store.flag_of(other, Prop::bIsPlayer)
                && self.store.object_of(other, Prop::Pawn).is_some_and(|their| (at - self.store.vector(their, Prop::Location)).size() <= min_player_distance)
        })
    }

    /// Retail damages every actor in reach when the class `FluidPuddle` cannot be loaded.
    fn fire_hurt_radius(&mut self, emitter: ObjectId, amount: f32, radius: f32, damage_type: Value, line: [Vector; 2], default_size: [f32; 2]) -> Result<(), Error> {
        let centre = self.store.vector(emitter, Prop::Location);
        let in_reach: Vec<ObjectId> = self
            .colliders_near([centre[0], centre[1]], [centre[0], centre[1]], radius)
            .into_iter()
            .map(|e| e.actor)
            .filter(|&actor| actor != emitter && !self.store.is_deleted(actor) && self.store.flag_of(actor, Prop::bCollideActors))
            .filter(|&actor| {
                let offset = self.store.vector(actor, Prop::Location) - centre;
                offset.dot(offset) < radius * radius
            })
            .collect();
        let instigator = Value::Object(self.store.object_of(emitter, Prop::Instigator));
        let damage = Value::Int((amount as i32).max(1));
        for actor in in_reach {
            if self.store.is_deleted(actor) {
                continue;
            }
            let location = self.store.vector(actor, Prop::Location);
            let (actor_radius, actor_height) = (self.store.float_of(actor, Prop::CollisionRadius), self.store.float_of(actor, Prop::CollisionHeight));
            let hit = self.store.is_a(actor, Kind::FluidPuddle)
                || thick_line_cylinder_collide(line[0], line[1], default_size[0], default_size[1], location, actor_radius, actor_height);
            if hit {
                let args = vec![damage.clone(), instigator.clone(), Value::Vector(location), Value::Vector(Vector::ZERO), damage_type.clone()];
                self.event(actor, "TakeDamage", args)?;
            }
        }
        Ok(())
    }

    /// By folder, then by name, as the engine's file search per `Paths` entry (unverified).
    /// A name in several folders counts once, from the first search path.
    pub(crate) fn map_files(&self) -> Vec<String> {
        let ext = match self.config_value("URL", "MapExt").unwrap_or_default() {
            ext if ext.is_empty() => DEFAULT_MAP_EXT.to_owned(),
            ext => ext,
        };
        let mut files: Vec<(&std::path::Path, String)> = self
            .loader
            .package_names()
            .filter_map(|name| self.loader.find(name))
            .filter(|path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case(&ext)))
            .filter_map(|path| Some((path.parent()?, path.file_name()?.to_str()?.to_owned())))
            .collect();
        files.sort_by_cached_key(|(folder, name)| (folder.to_path_buf(), name.to_uppercase()));
        files.into_iter().map(|(_, name)| name).collect()
    }
}

pub(crate) fn game_map(files: &[String], code: &str, previous: &str, dir: i32) -> String {
    let upper = code.to_uppercase();
    let mut maps: Vec<&str> = Vec::new();
    for name in files {
        let is_mp = name.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("MP"));
        let of_type = name.split_once('-').is_some_and(|(prefix, _)| prefix.contains(code) || prefix.contains(&upper));
        if is_mp && of_type && !maps.iter().any(|m| m.eq_ignore_ascii_case(name)) {
            maps.push(name);
        }
    }
    let Some(index) = maps.iter().position(|&m| m == previous) else { return maps.first().map_or_else(String::new, |m| (*m).to_owned()) };
    let next = index as i64 + i64::from(dir);
    let next = if next < 0 {
        maps.len() - 1
    } else if next >= maps.len() as i64 {
        0
    } else {
        next as usize
    };
    maps[next].to_owned()
}

pub(crate) fn point_to_line_dist(start: Vector, end: Vector, point: Vector) -> Option<(f32, f32)> {
    let line = end - start;
    let unit = line.safe_normal();
    if unit == Vector::ZERO {
        return None;
    }
    let from_start = point - start;
    let Vector { x, y, z } = from_start * (-1.0);
    let cross = [y * unit[2] - z * unit[1], z * unit[0] - x * unit[2], x * unit[1] - y * unit[0]];
    let length = line.size();
    Some((Vector::from(cross).size(), from_start.dot(line) / (length * length)))
}

pub(crate) fn vectors_in_fuzz(a: Vector, b: Vector, fuzz: f32) -> bool {
    (0..3).all(|i| b[i] - fuzz < a[i] && a[i] < b[i] + fuzz)
}

/// As retail, the distance from the line's middle is discarded, so only the ends count.
/// Retail leaves Z of the ends' horizontal copies uninitialised; taken as 0 here (unverified).
pub(crate) fn thick_line_cylinder_collide(start: Vector, end: Vector, line_radius: f32, line_height: f32, at: Vector, radius: f32, height: f32) -> bool {
    let touches = |point: Vector| {
        let flat = [point[0] - at[0], point[1] - at[1], 0.0];
        Vector::from(flat).size() <= line_radius + radius && (point[2] - at[2]).abs() < line_height + height
    };
    touches(start) || touches(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_to_line_dist_of_a_point_beside_the_segment() {
        let (distance, u) = point_to_line_dist([0.0; 3].into(), [10.0, 0.0, 0.0].into(), [5.0, 3.0, 4.0].into()).unwrap();
        assert!((distance - 5.0).abs() < 1e-5);
        assert!((u - 0.5).abs() < 1e-6);
        let (_, u) = point_to_line_dist([0.0; 3].into(), [10.0, 0.0, 0.0].into(), [20.0, 1.0, 0.0].into()).unwrap();
        assert!((u - 2.0).abs() < 1e-6);
        assert_eq!(point_to_line_dist([1.0; 3].into(), [1.0; 3].into(), [0.0; 3].into()), None);
    }

    #[test]
    fn vectors_in_fuzz_is_strict() {
        assert!(vectors_in_fuzz([1.0, 2.0, 3.0].into(), [1.05, 1.95, 3.0].into(), 0.1));
        assert!(!vectors_in_fuzz([1.0, 2.0, 3.0].into(), [1.0, 2.0, 3.5].into(), 0.5));
        assert!(!vectors_in_fuzz([0.0; 3].into(), [0.0; 3].into(), 0.0));
    }

    #[test]
    fn thick_line_hits_only_at_its_ends() {
        let (start, end) = ([0.0; 3], [1000.0, 0.0, 0.0]);
        assert!(thick_line_cylinder_collide(start.into(), end.into(), 10.0, 20.0, [30.0, 0.0, 10.0].into(), 25.0, 40.0));
        assert!(thick_line_cylinder_collide(start.into(), end.into(), 10.0, 20.0, [1000.0, 35.0, 0.0].into(), 25.0, 40.0));
        assert!(!thick_line_cylinder_collide(start.into(), end.into(), 10.0, 20.0, [1000.0, 0.0, 60.0].into(), 25.0, 40.0));
        assert!(!thick_line_cylinder_collide(start.into(), end.into(), 10.0, 20.0, [500.0, 0.0, 0.0].into(), 25.0, 40.0));
    }

    #[test]
    fn game_map_cycles_maps_of_a_game_type() {
        let files = ["MPDGT-Asylum.fuk", "mps-Intifada.fuk", "MPDGT-Boneyard.fuk", "Church.fuk", "MPDGT.fuk", "MPS-RedLight.fuk"]
            .map(String::from);
        assert_eq!(game_map(&files, "DGT", "", 0), "MPDGT-Asylum.fuk");
        assert_eq!(game_map(&files, "DGT", "MPDGT-Asylum.fuk", 1), "MPDGT-Boneyard.fuk");
        assert_eq!(game_map(&files, "DGT", "MPDGT-Boneyard.fuk", 1), "MPDGT-Asylum.fuk");
        assert_eq!(game_map(&files, "DGT", "MPDGT-Asylum.fuk", -1), "MPDGT-Boneyard.fuk");
        assert_eq!(game_map(&files, "s", "mps-Intifada.fuk", 1), "MPS-RedLight.fuk");
        assert_eq!(game_map(&files, "XX", "", 0), "");
    }
}
