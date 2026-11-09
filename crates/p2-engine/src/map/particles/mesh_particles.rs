use super::*;

#[derive(bevy::ecs::component::Component)]
pub(crate) struct PoolSlot;

pub(super) struct MeshPool {
    pub(super) sections: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    pub(super) slots: Vec<Vec<Entity>>,
    pub(super) shown: usize,
    pub(super) reach: f32,
}

impl MeshPool {
    pub(super) fn new(sections: &[SectionView], props: &p2_particles::EmitterProperties, t: &mut SceneTargets, shared: &mut HashMap<PoolMaterialKey, Handle<StandardMaterial>>) -> Self {
        let reach = sections
            .iter()
            .filter_map(|section| t.assets.meshes.get(&section.mesh)?.get_aabb())
            .map(|aabb| Vec3::from(aabb.min()).abs().max(Vec3::from(aabb.max()).abs()).length())
            .fold(0.0, f32::max);
        let sections = sections
            .iter()
            .map(|section| {
                let two_sided = section.two_sided || props.render_two_sided;
                let alpha_mode = if props.use_mesh_blend_mode { section.alpha_mode } else { standard_alpha(props.draw_style) };
                let key = PoolMaterialKey { texture: section.texture.as_ref().map(Handle::id), alpha: alpha_key(alpha_mode), two_sided };
                let material = shared.entry(key).or_insert_with(|| {
                    t.assets.materials.add(StandardMaterial {
                        base_color_texture: section.texture.clone(),
                        alpha_mode,
                        unlit: true,
                        double_sided: two_sided,
                        cull_mode: if two_sided { None } else { Some(Face::Back) },
                        ..Default::default()
                    })
                });
                (section.mesh.clone(), material.clone())
            })
            .collect();
        Self { sections, slots: Vec::new(), shown: 0, reach }
    }
}

fn mesh_transform(particle: &MeshParticle) -> Transform {
    let turn = rotator_matrix(particle.frame) * rotator_matrix(particle.spin);
    unreal_to_bevy(Mat4::from_translation(Vec3::from_array(particle.location.into())) * turn * Mat4::from_scale(Vec3::from_array(particle.size.into())))
}

pub(super) fn mesh_box(particle: &MeshParticle, reach: f32) -> (Vec3, Vec3) {
    let at = Vec3::from(to_bevy(particle.location));
    let reach = Vec3::splat(reach * <[f32; 3]>::from(particle.size).iter().fold(0.0_f32, |most, size| most.max(size.abs())));
    (at - reach, at + reach)
}

pub(super) fn place_mesh_particles(commands: &mut Commands, view: &mut ParticleView, p: &mut ParticleActor, meshes: &[(Vec<MeshParticle>, f32)], in_view: bool) {
    let (overlay, parent) = (p.overlay, p.parent);
    for (draw, (particles, _)) in p.draws.iter_mut().zip(meshes) {
        let Some(pool) = &mut draw.pool else { continue };
        let shown = if in_view { particles.len() } else { 0 };
        while pool.slots.len() < shown {
            let layer = overlay.map_or_else(RenderLayers::default, |slot| RenderLayers::layer(OVERLAY_LAYERS[slot]));
            let placed = particles.get(pool.slots.len()).map_or_else(Transform::default, mesh_transform);
            let slot: Vec<Entity> = (pool.sections.iter())
                .map(|(mesh, material)| {
                    let section = (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), placed, Visibility::Hidden, PoolSlot);
                    commands.spawn((section, layer.clone(), ChildOf(parent))).id()
                })
                .collect();
            pool.slots.push(slot);
        }
        for (index, slot) in pool.slots.iter().enumerate() {
            let now = index < shown;
            let visibility = (now != (index < pool.shown)).then_some(if now { Visibility::Inherited } else { Visibility::Hidden });
            let transform = particles.get(index).filter(|_| now).map(mesh_transform);
            for &entity in slot {
                if let Some(transform) = transform
                    && let Ok(mut placed) = view.pool_slots.get_mut(entity)
                {
                    placed.set_if_neq(transform);
                }
                if let Some(visibility) = visibility {
                    commands.entity(entity).insert(visibility);
                }
            }
        }
        pool.shown = shown;
    }
}
