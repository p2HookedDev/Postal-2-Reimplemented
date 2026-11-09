use super::*;

pub(super) struct MapActors {
    pub(super) terrain_infos: Vec<p2_assets::Terrain>,
    pub(super) start: Option<(Vector, Rotator)>,
    pub(super) mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>>,
    pub(super) zone_fogs: Vec<Option<crate::actors::ZoneFog>>,
    pub(super) terrain_zones: Vec<bool>,
    pub(super) mesh_lights: HashMap<i32, p2_render_cpu::MeshLight>,
    pub(super) zone_ambients: Vec<[f32; 3]>,
    pub(super) level_brightness: f32,
    pub(super) attachments: HashMap<ObjectId, Vec<SkeletalActor>>,
    pub(super) script_errors: usize,
    pub(super) played: (Vm<Arc<Loader>>, Arc<Loader>, Option<(ObjectId, Option<ObjectId>)>, Vec<Option<ObjectId>>),
    pub(super) pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>>,
    pub(super) projectors: Vec<ProjectorActor>,
    pub(super) antiportal_paths: Vec<(String, MeshPlacement)>,
    pub(super) emitters: Vec<(ObjectId, EmitterActor)>,
    pub(super) sprites: Vec<SpriteActor>,
    pub(super) coronas: Vec<CoronaActor>,
    pub(super) fluids: Vec<fluids::FluidActor>,
}

pub(super) fn read_map_actors(started: StartedVm, model: &Model, sky_zone: Option<u8>) -> MapActors {
    let mut terrain_infos = Vec::new();
    let mut start = None;
    let mut mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>> = HashMap::new();
    let mut zone_fogs = Vec::new();
    let mut terrain_zones = Vec::new();
    let mut mesh_lights = HashMap::new();
    let mut zone_ambients = Vec::new();
    let mut level_info = None;
    let mut level_brightness = 1.0;
    let mut attachments: HashMap<ObjectId, Vec<SkeletalActor>> = HashMap::new();
    let mut pawns: HashMap<(String, Vec<Option<String>>), Vec<SkeletalActor>> = HashMap::new();
    let mut projectors = Vec::new();
    let mut antiportal_paths = Vec::new();
    let mut emitters = Vec::new();
    let mut sprites = Vec::new();
    let mut coronas = Vec::new();
    let mut fluids = Vec::new();
    let StartedVm { mut vm, loader, id, script_errors, player } = started;
    let LoadedPackage { package: pkg, data, .. } = loader.package(id);
    for (index, export) in pkg.exports.iter().enumerate() {
        let Ok(object) = vm.load_object(id, index) else { continue };
        if let Some(light) = mesh_light(&vm, object) {
            mesh_lights.insert(export_reference(index), light);
        }
        match pkg.object_name(export.class_index) {
            Some("LevelInfo") => level_info = Some(object),
            Some("TerrainInfo") => {
                let Some(class) = vm.store.object(object).class else { continue };
                terrain_infos.extend(read_terrain(pkg, data, export, &vm.store.class(class).layout).ok());
            }
            Some("PlayerStart") if start.is_none() => start = Some(location_rotation(&vm, object)),
            _ => {}
        }
    }
    let zone_actors: Vec<Option<ObjectId>> = model.zones.iter().map(|zone| usize::try_from(zone.actor - 1).ok().and_then(|export| vm.load_object(id, export).ok())).collect();
    let zone_tags: Vec<_> = zone_actors
        .iter()
        .map(|&object| {
            match object.or(level_info).and_then(|object| vm.store.get(object, "Tag")) {
                Some(p2_vm::Value::Name(tag)) => Some(tag),
                _ => None,
            }
        })
        .collect();
    let project_tags: Vec<_> = vm
        .actors()
        .filter_map(|o| match vm.store.get(o, "ProjectTag") {
            Some(p2_vm::Value::Name(tag)) if tag != p2_vm::NameId::NONE => Some(tag),
            _ => None,
        })
        .collect();
    for object in vm.actors().filter(|&o| !vm.store.is_deleted(o)) {
        projectors.extend(projector_actor(&vm, object));
        antiportal_paths.extend(antiportal_actor(&vm, object));
        emitters.extend(emitter_actor(&vm, object).map(|emitter| (object, emitter)));
        sprites.extend(sprite_actor(&vm, object));
        coronas.extend(corona_actor(&vm, object));
        let seed = fluids.len() as u32 + 1;
        fluids.extend(fluids::fluid_actor(&vm, object, seed));
        if let Some(actor) = skeletal_actor(&vm, object) {
            if let Some((base, _)) = actor.attached_to {
                attachments.entry(base).or_default().push(actor);
                continue;
            }
            pawns.entry((actor.mesh.clone(), actor.skins.clone())).or_default().push(actor);
            continue;
        }
        let Some(actor) = mesh_actor(&vm, object) else { continue };
        let transform = unreal_transform(actor.location, actor.rotation, actor.scale, actor.pre_pivot);
        let zone = p2_collision::point_zone(model, actor.location);
        let lighting = MeshLighting {
            placement: MeshPlacement {
                location: actor.location,
                rotation: actor.rotation,
                scale: actor.scale,
                pre_pivot: actor.pre_pivot,
            },
            zone,
            glow: actor.lighting.glow,
        };
        let instance = (!actor.lighting.unlit).then_some((actor.instance, lighting));
        // Movers are not level collision: retail particles trace only the level and its static geometry.
        let placement = (actor.blocks && !actor.movable).then_some(Placement { location: actor.location, rotation: actor.rotation, scale: actor.scale, pre_pivot: actor.pre_pivot });
        let in_sky = sky_zone == Some(zone);
        // Merged sections take every untagged projector and no tagged one: a model of its own carries the actor's own answer
        // (`Session::update_projections`).
        let aimed_at = matches!(vm.store.get(actor.object, "Tag"), Some(p2_vm::Value::Name(tag)) if project_tags.contains(&tag));
        let own_model = (!actor.accepts_projectors || aimed_at) && !in_sky && actor.forced_zone_tag.is_none();
        let follow =
            (actor.movable || own_model).then_some(FollowActor { actor: actor.object, scale: actor.scale, pre_pivot: actor.pre_pivot, skeletal: None });
        let only_from = actor.forced_zone_tag.as_ref().map(|tag| {
            (0..=u8::MAX).filter(|&zone| zone_tags.get(usize::from(zone)).is_some_and(|zone_tag| zone_tag.as_ref() == Some(tag))).collect::<Vec<_>>()
        });
        mesh_paths.entry((actor.static_mesh, actor.skins)).or_default().push(MeshPlacing { transform, instance, placement, follow, in_sky, only_from });
    }
    for &object in &zone_actors {
        zone_fogs.push(object.and_then(|object| zone_fog(&vm, object)));
        terrain_zones.push(object.or(level_info).is_some_and(|object| vm.store.get(object, "bTerrainZone") == Some(p2_vm::Value::Bool(true))));
        zone_ambients.push(object.or(level_info).map_or([0.0; 3], |object| zone_ambient(&vm, object)));
    }
    if let Some(p2_vm::Value::Float(brightness)) = level_info.and_then(|info| vm.store.get(info, "Brightness")) {
        level_brightness = brightness;
    }
    let played = (vm, Arc::clone(&loader), player, zone_actors);
    MapActors { terrain_infos, start, mesh_paths, zone_fogs, terrain_zones, mesh_lights, zone_ambients, level_brightness, attachments, script_errors, played, pawns, projectors, antiportal_paths, emitters, sprites, coronas, fluids }
}

pub(super) fn read_map_model(pkg: &Package, file: &[u8], package: &str) -> Result<(String, Model), LoadError> {
    let export = pkg
        .exports
        .iter()
        .find(|e| pkg.object_name(e.class_index) == Some("Level"))
        .ok_or_else(|| LoadError::NoLevel { package: package.to_owned() })?;
    let name = pkg.name(export.object_name).to_owned();
    let level = read_level(pkg, file, export).map_err(|source| LoadError::Level { name: name.clone(), source })?;
    let model = usize::try_from(level.model - 1).ok().and_then(|i| pkg.exports.get(i));
    let model = model.ok_or_else(|| LoadError::NoModel { name: name.clone() })?;
    let model = read_model(pkg, file, model).map_err(|source| LoadError::Level { name: name.clone(), source })?;
    Ok((name, model))
}
