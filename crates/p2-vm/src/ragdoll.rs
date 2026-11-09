use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::pose::Frame;
use p2_assets::{KarmaAsset, read_karma_assets};
use p2_physics::{BodyId, Hit, JointParams, PartParams, Parts, Throw};

use p2_math::Vector;
use crate::enums::EPhysics;
use crate::bones::PosedMesh;
use crate::{ClassId, LoaderRef, NameId, ObjectId, Vm, Prop};

mod step;
mod natives;
mod rig;
#[cfg(test)]
mod rig_tests;
use rig::scale_frames;

const MAX_SPEED: f32 = 1000.0;
/// In place of `KFriction` (0.7 on a pawn): Karma corpses slide 200-500 units to rest (unverified).
const CORPSE_FRICTION: f32 = 0.3;
/// Corpses sprawl rather than fold ...
const CONE_SCALE: f32 = 1.3;
/// ... but wider cones let limbs wrap around the body.
const MAX_CONE: f32 = 1.55;
/// A fixed twist held at exactly 0 fights the cone and makes limbs flip.
const MIN_TWIST: f32 = 0.1;
const THROWS: [(&str, Throw); 9] = [
    ("ShotGunDamage", Throw { per_damage: 11.0, ceiling: 650.0, lift: 0.3, falloff: true, tumble: false }),
    ("SuperShotgunDamage", Throw { per_damage: 11.0, ceiling: 650.0, lift: 0.3, falloff: true, tumble: false }),
    ("RifleDamage", Throw { per_damage: 14.0, ceiling: 520.0, lift: 0.2, falloff: false, tumble: false }),
    ("MachineGunDamage", Throw { per_damage: 10.0, ceiling: 350.0, lift: 0.1, falloff: false, tumble: false }),
    ("BulletDamage", Throw { per_damage: 6.0, ceiling: 260.0, lift: 0.1, falloff: false, tumble: false }),
    ("ExplodedDamage", Throw { per_damage: 5.0, ceiling: 700.0, lift: 0.6, falloff: false, tumble: true }),
    ("SmashDamage", Throw { per_damage: 6.0, ceiling: 700.0, lift: 0.5, falloff: false, tumble: true }),
    ("CuttingDamage", Throw { per_damage: 3.0, ceiling: 400.0, lift: 0.1, falloff: false, tumble: false }),
    ("BludgeonDamage", Throw { per_damage: 10.0, ceiling: 550.0, lift: 0.25, falloff: false, tumble: false }),
];
const HIT_TICKS: u64 = 2;
/// Scripts cut with 0 or 1/1000; a 0.2 stump keeps its body.
const CUT_SCALE: f32 = 0.01;

struct SeenHit {
    actor: ObjectId,
    tick: u64,
    hit: Hit,
}
const MAX_JOINT_ERROR: f32 = 50.0;

#[derive(Default)]
pub(crate) struct Ragdolls {
    assets: Option<HashMap<String, Arc<KarmaAsset>>>,
    live: BTreeMap<ObjectId, Ragdoll>,
    kept: FxHashMap<ObjectId, (Arc<PosedMesh>, Arc<[Frame]>)>,
    frozen: HashSet<ObjectId>,
    failed: HashSet<ObjectId>,
    throws: FxHashMap<ClassId, Option<Throw>>,
    tick: u64,
    hits: Vec<SeenHit>,
}

impl Ragdolls {
    pub(crate) fn for_next_level(&mut self) -> Self {
        Self { assets: self.assets.take(), ..Default::default() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

#[derive(Clone, Copy)]
struct TrackedJoint {
    child: usize,
    parent: usize,
    at_child: Vector,
    at_parent: Vector,
}

struct Ragdoll {
    bodies: Vec<Option<BodyId>>,
    all: Arc<[BodyId]>,
    joints: Vec<TrackedJoint>,
    mesh: Arc<PosedMesh>,
    scale: f32,
    frames: Arc<[Frame]>,
    fresh: bool,
    posed_asleep: Option<bool>,
    /// Lines and impulses test these while the world steps.
    parts: Parts,
    scales: ResolvedScales,
    cut_refused: bool,
    /// A bodiless root moves with this body, or `SetBoneScale` pins a cut corpse's drawn top half at the cut.
    root_offset: Option<(usize, Frame)>,
}

#[derive(Default)]
struct ResolvedScales {
    read: Vec<(NameId, u32)>,
    own: Option<Vec<f32>>,
    cut: Vec<bool>,
}

impl ResolvedScales {
    fn holds(&self, scales: &[(NameId, f32)]) -> bool {
        self.read.len() == scales.len() && self.read.iter().zip(scales).all(|(&(a, x), &(b, y))| a == b && x == y.to_bits())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Corpse {
    Kept,
    Gone,
}

impl<L: LoaderRef> Vm<L> {
    pub fn ragdoll_pose(&mut self, actor: ObjectId) -> Option<Vec<Frame>> {
        let world = self.ragdoll_world_frames(actor)?;
        let mesh = self.store.object_of(actor, Prop::Mesh).and_then(|m| self.posed_mesh(m))?;
        let to_mesh = self.mesh_to_world(actor, &mesh).inverse()?;
        Some(world.iter().map(|frame| to_mesh.then(frame)).collect())
    }

    pub(crate) fn ragdoll_world_frames(&self, actor: ObjectId) -> Option<Arc<[Frame]>> {
        let ragdolls = &self.karma.ragdolls;
        let live = ragdolls.live.get(&actor);
        let (mesh, frames) = match (live, ragdolls.kept.get(&actor)) {
            (Some(ragdoll), _) if ragdoll.fresh => return Some(ragdoll.frames.clone()),
            (Some(ragdoll), _) => (&ragdoll.mesh, &ragdoll.frames),
            (None, Some((mesh, frames))) => (mesh, frames),
            (None, None) => return None,
        };
        let scales: Vec<_> = self.bone_scales(actor).collect();
        if scales.is_empty() {
            return Some(frames.clone());
        }
        let resolved = live.filter(|ragdoll| ragdoll.scales.holds(&scales)).and_then(|ragdoll| ragdoll.scales.own.as_deref());
        let computed;
        let own: &[f32] = match resolved {
            Some(own) => own,
            None => {
                computed = self.scales_on_mesh(&scales, mesh)?;
                &computed
            }
        };
        let mut scaled = frames.to_vec();
        scale_frames(&mut scaled, own, &mesh.parents);
        Some(scaled.into())
    }

    fn bone_scale_factors(&self, actor: ObjectId, mesh: &PosedMesh) -> Option<Vec<f32>> {
        self.scales_on_mesh(&self.bone_scales(actor).collect::<Vec<_>>(), mesh)
    }

    fn scales_on_mesh(&self, scales: &[(NameId, f32)], mesh: &PosedMesh) -> Option<Vec<f32>> {
        let scales: Vec<(String, f32)> = scales.iter().map(|&(bone, scale)| (self.store.name(bone).to_ascii_lowercase(), scale)).collect();
        (!scales.is_empty()).then(|| mesh.names.iter().map(|name| scales.iter().filter(|(n, _)| n == name).map(|(_, s)| s).product()).collect())
    }

    /// Retail's frozen-ragdoll box is not modelled: a kept pose keeps the cylinder.
    pub(crate) fn ragdoll_box(&self, actor: ObjectId) -> Option<(Vector, Vector)> {
        self.karma.ragdolls.live.get(&actor)?.parts.bounds()
    }

    pub(crate) fn ragdoll_line_check(&self, actor: ObjectId, start: Vector, end: Vector) -> Option<Option<p2_collision::Hit>> {
        if self.store.physics_of(actor) != Some(EPhysics::KarmaRagDoll) {
            return None;
        }
        let Some(ragdoll) = self.karma.ragdolls.live.get(&actor) else { return Some(None) };
        Some(ragdoll.parts.segment_hit(start, end).map(|hit| p2_collision::Hit {
            time: hit.time,
            location: [0, 1, 2].map(|i| start[i] + (end[i] - start[i]) * hit.time).into(),
            normal: hit.normal,
        }))
    }

    /// In file name order, as retail: `Avg_Fem.ka` wins over `Avg_Fem_Skel.ka`.
    pub(crate) fn karma_assets(&mut self) -> &HashMap<String, Arc<KarmaAsset>> {
        if self.karma.ragdolls.assets.is_none() {
            let phase = self.profile_start();
            let mut files: Vec<std::path::PathBuf> = self
                .loader
                .package_names()
                .filter_map(|name| self.loader.find(name))
                .filter(|path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ka")))
                .map(std::path::Path::to_path_buf)
                .collect();
            files.sort_by_key(|path| path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()));
            let mut assets = HashMap::new();
            for path in files {
                let read = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| read_karma_assets(&text).map_err(|e| e.to_string()));
                match read {
                    Ok(list) => {
                        for asset in list {
                            assets.entry(asset.id.to_ascii_lowercase()).or_insert_with(|| Arc::new(asset));
                        }
                    }
                    Err(e) => self.diagnostics.log.push(format!("(Karma): Non-fatal error in MeFileLoad: {}: {e}", path.display())),
                }
            }
            self.karma.ragdolls.assets = Some(assets);
            self.profile_phase("load: karma assets", phase);
        }
        self.karma.ragdolls.assets.get_or_insert_default()
    }
}

struct Built {
    parts: Vec<PartParams>,
    joint_params: Vec<JointParams>,
    part_of_bone: Vec<Option<usize>>,
    joints: Vec<TrackedJoint>,
}
