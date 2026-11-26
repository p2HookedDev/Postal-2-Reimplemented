use super::*;
use bevy::asset::{AssetEvent, AssetId};
use bevy::camera::visibility::{InheritedVisibility, NoFrustumCulling};
use bevy::ecs::message::MessageReader;
use bevy::ecs::prelude::Local;
use bevy::ecs::system::ParamSet;
use bevy::math::{Affine3A, Mat4};
use bevy::mesh::MeshTag;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::pbr::Material;

use crate::lighting::actor_light::ActorLitMaterial;
use crate::effects::particle_material::ParticleMaterial;
use crate::world::translucent_order::TranslucentDraw;

#[derive(Component)]
pub(super) struct InSky;

#[derive(Component)]
#[relationship(relationship_target = SkyCopies)]
pub(super) struct SkyCopyOf(Entity);

#[derive(Component)]
#[relationship_target(relationship = SkyCopyOf, linked_spawn)]
pub(super) struct SkyCopies(Vec<Entity>);

type Draw = (
    Entity,
    &'static Mesh3d,
    &'static Visibility,
    &'static InheritedVisibility,
    Has<InSky>,
    Option<&'static SkyCopies>,
    Option<&'static MeshTag>,
    Option<&'static TranslucentDraw>,
    Option<&'static SkinnedMesh>,
    Has<NoFrustumCulling>,
);

#[derive(SystemParam)]
pub(super) struct SkyDraws<'w, 's> {
    draws: Query<'w, 's, Draw, (Or<(With<InSky>, With<SkyCopies>)>, Without<SkyCopyOf>)>,
    chain: Query<'w, 's, (&'static Transform, Option<&'static ChildOf>), Without<SkyCopyOf>>,
    copies: Query<'w, 's, (&'static mut Transform, &'static mut Visibility, &'static mut Mesh3d), With<SkyCopyOf>>,
    root: Query<'w, 's, &'static Transform, (With<SkyRoot>, Without<SkyCopyOf>)>,
    joints: Query<'w, 's, (Entity, &'static SkyCopies), (Without<Mesh3d>, Without<SkyCopyOf>)>,
    joint_copies: Query<'w, 's, &'static mut Transform, (With<SkyCopyOf>, Without<Mesh3d>)>,
}

pub(super) fn sky_copies(mut commands: Commands, shown: Res<ShownScene>, mut sky: SkyDraws) {
    let Ok(root) = sky.root.single() else { return };
    let placement = root.compute_affine() * Affine3A::from_translation(-shown.origin);
    for (draw, mesh, visibility, inherited, in_sky, copies, tag, order, skin, unculled) in &sky.draws {
        let seen = in_sky
            && match visibility {
                Visibility::Hidden => false,
                Visibility::Visible => true,
                Visibility::Inherited => inherited.get(),
            };
        let shown_as = if seen { Visibility::Inherited } else { Visibility::Hidden };
        let transform = Transform::from_matrix(Mat4::from(placement * chain_of(draw, &sky.chain)));
        match copies.and_then(|copies| copies.0.first()).and_then(|&copy| sky.copies.get_mut(copy).ok()) {
            Some((mut at, mut drawn_as, mut drawn)) => {
                at.set_if_neq(transform);
                drawn_as.set_if_neq(shown_as);
                if drawn.0 != mesh.0 {
                    drawn.0 = mesh.0.clone();
                }
            }
            None => {
                let skin = skin.map(|skin| SkinnedMesh {
                    inverse_bindposes: skin.inverse_bindposes.clone(),
                    joints: skin.joints.iter().map(|&joint| commands.spawn((Transform::default(), SkyCopyOf(joint))).id()).collect(),
                });
                let mut copy = commands.spawn((mesh.clone(), transform, shown_as, RenderLayers::layer(SKY_LAYER), NoSunCast, SkyCopyOf(draw)));
                if let Some(tag) = tag {
                    copy.insert(tag.clone());
                }
                if let Some(order) = order {
                    copy.insert(*order);
                }
                if let Some(skin) = skin {
                    copy.insert(skin);
                }
                if unculled {
                    copy.insert(NoFrustumCulling);
                }
            }
        }
    }
    for (joint, copies) in &sky.joints {
        let placed = Transform::from_matrix(Mat4::from(placement * chain_of(joint, &sky.chain)));
        for &copy in &copies.0 {
            if let Ok(mut at) = sky.joint_copies.get_mut(copy) {
                at.set_if_neq(placed);
            }
        }
    }
}

fn chain_of(draw: Entity, chain: &Query<(&Transform, Option<&ChildOf>), Without<SkyCopyOf>>) -> Affine3A {
    let mut affine = Affine3A::IDENTITY;
    let mut at = Some(draw);
    while let Some((transform, parent)) = at.and_then(|entity| chain.get(entity).ok()) {
        affine = transform.compute_affine() * affine;
        at = parent.map(ChildOf::parent);
    }
    affine
}

pub(super) trait SkyLook: Material + Clone {
    type Sky: Material;

    fn sky_look(&self) -> Self::Sky;
}

impl SkyLook for StandardMaterial {
    type Sky = SkyStandardMaterial;

    fn sky_look(&self) -> SkyStandardMaterial {
        SkyStandardMaterial { base: self.clone(), extension: SkyStandard {} }
    }
}

impl SkyLook for MeshLitMaterial {
    type Sky = Self;

    fn sky_look(&self) -> Self {
        Self { extension: DynamicLit { sky: true, ..self.extension.clone() }, base: self.base.clone() }
    }
}

impl SkyLook for ActorLitMaterial {
    type Sky = Self;

    fn sky_look(&self) -> Self {
        Self { extension: crate::lighting::actor_light::HardwareLighting { sky: true, ..self.extension.clone() }, base: self.base.clone() }
    }
}

impl SkyLook for ParticleMaterial {
    type Sky = Self;

    fn sky_look(&self) -> Self {
        Self { sky: true, ..self.clone() }
    }
}

pub(super) fn follow_sky_material<M: SkyLook>(
    mut commands: Commands,
    copies: Query<(Entity, &SkyCopyOf, Option<&MeshMaterial3d<M::Sky>>)>,
    originals: Query<&MeshMaterial3d<M>, Without<SkyCopyOf>>,
    mut stores: ParamSet<(Option<Res<Assets<M>>>, Option<ResMut<Assets<M::Sky>>>)>,
    mut changes: MessageReader<AssetEvent<M>>,
    mut twins: Local<FxHashMap<AssetId<M>, Handle<M::Sky>>>,
) {
    let changed: Vec<(M::Sky, AssetId<M::Sky>)> = {
        let Some(looks) = stores.p0() else { return };
        changes
            .read()
            .filter_map(|event| match event {
                AssetEvent::Modified { id } => Some((looks.get(*id)?.sky_look(), twins.get(id)?.id())),
                _ => None,
            })
            .collect()
    };
    let wanted: Vec<(Entity, Option<AssetId<M>>, Option<Handle<M::Sky>>)> =
        copies.iter().map(|(copy, original, now)| (copy, originals.get(original.0).ok().map(|material| material.id()), now.map(|now| now.0.clone()))).collect();
    let fresh: Vec<(AssetId<M>, M::Sky)> = {
        let Some(looks) = stores.p0() else { return };
        wanted
            .iter()
            .filter_map(|(_, original, _)| original.filter(|id| !twins.contains_key(id)))
            .filter_map(|id| Some((id, looks.get(id)?.sky_look())))
            .collect()
    };
    let Some(mut skies) = stores.p1() else { return };
    for (look, id) in changed {
        if let Some(mut sky) = skies.get_mut(id) {
            *sky = look;
        }
    }
    for (id, look) in fresh {
        twins.entry(id).or_insert_with(|| skies.add(look));
    }
    for (copy, original, now) in wanted {
        match original.and_then(|id| twins.get(&id)) {
            Some(sky) if now.as_ref() != Some(sky) => {
                commands.entity(copy).insert(MeshMaterial3d(sky.clone()));
            }
            None if now.is_some() => {
                commands.entity(copy).remove::<MeshMaterial3d<M::Sky>>();
            }
            _ => {}
        }
    }
}
