#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Context;
use p2_assets::pose::{Frame, LocalBone, bone_frames};
use p2_assets::{Model, ModelPolygon, poly_flags};
use p2_math::Vector;
use p2_package::{Export, MAP_EXTENSION, Package, TagValue};
use p2_render_cpu::{Plane, VisibleZones, ZonePortals};
use p2_script::{NativeName, ScriptObject, decode_script, disassemble, native_names, read_export};

const USAGE: &str = "usage: p2-dump <package> [--props | --code | --reach | --zones | --walk | --above | --rig <.ka file> <mesh>] | p2-dump <folder> --shaders";


fn level_model(pkg: &Package, data: &[u8]) -> anyhow::Result<Model> {
    pkg.exports
        .iter()
        .filter(|export| p2_script::class_name(pkg, export) == "Model")
        .filter_map(|export| p2_assets::read_model(pkg, data, export).ok())
        .max_by_key(|model| model.nodes.len())
        .context("no BSP model in the package")
}

fn ray_hits<'a>(polygons: &[&'a ModelPolygon], origin: Vector, direction: Vector) -> Vec<(f32, &'a ModelPolygon)> {
    let mut hits: Vec<_> = polygons
        .iter()
        .filter_map(|&polygon| {
            let normal = Vector::from(polygon.normal);
            let facing = normal.dot(direction);
            if facing == 0.0 || (facing > 0.0 && polygon.poly_flags & poly_flags::TWO_SIDED == 0) {
                return None;
            }
            let distance = normal.dot(Vector::from(*polygon.positions.first()?) - origin) / facing;
            if distance <= 1.0 {
                return None;
            }
            let at = origin + direction * distance;
            let corners = || polygon.positions.iter().map(|&p| Vector::from(p));
            let (mut left, mut right) = (false, false);
            for (a, b) in corners().zip(corners().cycle().skip(1)) {
                let turn = (b - a).cross(at - a).dot(normal);
                left |= turn > 0.0;
                right |= turn < 0.0;
            }
            (!(left && right)).then_some((distance, polygon))
        })
        .collect();
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    hits
}

const SURVEY_RAYS: usize = 4000;

fn zone_survey(pkg: &Package, data: &[u8], out: &mut impl Write) -> anyhow::Result<()> {
    let model = level_model(pkg, data)?;
    let polygons = model.polygons();
    let front_zone = |polygon: &ModelPolygon| model.nodes[polygon.node].zones[1];
    let actor = |zone: usize| {
        let export = model.zones.get(zone).and_then(|zone| usize::try_from(zone.actor).ok()?.checked_sub(1)).and_then(|index| pkg.exports.get(index));
        export.map_or("-", |export| pkg.name(export.object_name))
    };
    writeln!(out, "zone actor polygons backdrop portals min max")?;
    for zone in 0..=u8::MAX {
        let own: Vec<&ModelPolygon> = polygons.iter().filter(|polygon| front_zone(polygon) == zone).collect();
        if own.is_empty() {
            continue;
        }
        let count = |flag: u32| own.iter().filter(|polygon| polygon.poly_flags & flag != 0).count();
        let (min, max) = own.iter().flat_map(|polygon| &polygon.positions).fold(([f32::MAX; 3], [f32::MIN; 3]), |(min, max), p| {
            ([0, 1, 2].map(|k| min[k].min(p[k])), [0, 1, 2].map(|k| max[k].max(p[k])))
        });
        writeln!(
            out,
            "{zone} {} {} {} {} {:.0?} {:.0?}",
            actor(usize::from(zone)),
            own.len(),
            count(poly_flags::FAKE_BACKDROP),
            count(poly_flags::PORTAL),
            min,
            max
        )?;
    }
    writeln!(out, "portals: zones, centre")?;
    for polygon in polygons.iter().filter(|polygon| polygon.poly_flags & poly_flags::PORTAL != 0) {
        let centre = polygon.positions.iter().fold(Vector::ZERO, |sum, &p| sum + Vector::from(p)) / polygon.positions.len().max(1) as f32;
        writeln!(out, "  {:?} {:.0} {:.0} {:.0}", model.nodes[polygon.node].zones, centre.x, centre.y, centre.z)?;
    }

    let solid: Vec<&ModelPolygon> =
        polygons.iter().filter(|polygon| polygon.poly_flags & (poly_flags::INVISIBLE | poly_flags::PORTAL | poly_flags::TRANSLUCENT) == 0).collect();
    let level: Vec<&ModelPolygon> = solid.iter().copied().filter(|polygon| polygon.poly_flags & poly_flags::FAKE_BACKDROP == 0).collect();
    let mut behind: BTreeMap<(u8, Option<u8>), (usize, f32)> = BTreeMap::new();
    for backdrop in solid.iter().filter(|polygon| polygon.poly_flags & poly_flags::FAKE_BACKDROP != 0) {
        let centre = backdrop.positions.iter().fold(Vector::ZERO, |sum, &p| sum + Vector::from(p)) / backdrop.positions.len().max(1) as f32;
        let hit = ray_hits(&level, centre, -Vector::from(backdrop.normal)).first().map(|&(distance, polygon)| (distance, front_zone(polygon)));
        let entry = behind.entry((front_zone(backdrop), hit.map(|(_, zone)| zone))).or_insert((0, f32::MAX));
        entry.0 += 1;
        entry.1 = entry.1.min(hit.map_or(f32::MAX, |(distance, _)| distance));
    }
    writeln!(out, "straight behind each backdrop polygon, (its zone, the zone of the level polygon behind): (polygons, nearest)")?;
    for (key, (count, nearest)) in behind {
        writeln!(out, "  {key:?}: {count} {nearest:.0}")?;
    }

    for line in std::io::stdin().lines() {
        let numbers: Vec<f32> = line?.split_whitespace().filter_map(|part| part.parse().ok()).collect();
        let [x, y, z] = numbers[..] else { continue };
        let eye = Vector::new(x, y, z);
        let leaf = p2_collision::point_leaf(&model, eye).and_then(|leaf| model.leaves.get(leaf));
        writeln!(out, "eye {x} {y} {z}: zone {}, leaf's visible zones {:#018x}", p2_collision::point_zone(&model, eye), leaf.map_or(0, |leaf| leaf.visible_zones))?;
        let mut first: BTreeMap<u8, usize> = BTreeMap::new();
        let mut leaks: BTreeMap<(u8, u8), (usize, f32, f32, f32)> = BTreeMap::new();
        let mut sky = 0;
        for i in 0..SURVEY_RAYS {
            let up = 1.0 - (i as f32 + 0.5) / SURVEY_RAYS as f32 * 2.0;
            let (flat, turn) = ((1.0 - up * up).sqrt(), i as f32 * 2.399_963);
            let direction = Vector::new(flat * turn.cos(), flat * turn.sin(), up);
            let hits = ray_hits(&solid, eye, direction);
            let Some(&(_, nearest)) = hits.first() else { continue };
            *first.entry(front_zone(nearest)).or_default() += 1;
            if nearest.poly_flags & poly_flags::FAKE_BACKDROP == 0 {
                continue;
            }
            sky += 1;
            if let Some(&(distance, polygon)) = hits.iter().find(|(_, polygon)| polygon.poly_flags & poly_flags::FAKE_BACKDROP == 0) {
                let height = (eye + direction * distance).z;
                let entry = leaks.entry((front_zone(nearest), front_zone(polygon))).or_insert((0, f32::MAX, f32::MAX, f32::MIN));
                *entry = (entry.0 + 1, entry.1.min(distance), entry.2.min(height), entry.3.max(height));
            }
        }
        writeln!(out, "  rays by the zone of the first polygon met: {first:?}")?;
        writeln!(out, "  {sky} rays meet a backdrop first; behind it, (backdrop's zone, zone of the level polygon): (rays, nearest, lowest z, highest z)")?;
        for (key, (count, nearest, low, high)) in leaks {
            writeln!(out, "    {key:?}: {count} {nearest:.0} {low:.0} {high:.0}")?;
        }
    }
    Ok(())
}

fn turn_between(a: &Frame, b: &Frame) -> f32 {
    let unit = |frame: &Frame| frame.axes.map(|axis| Vector::from(axis).safe_normal());
    let (a, b) = (unit(a), unit(b));
    let trace: f32 = (0..3).map(|i| a[i].dot(b[i])).sum();
    ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
}

fn rig_check(pkg: &Package, data: &[u8], asset_path: &Path, mesh_name: &str, out: &mut impl Write) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(asset_path).with_context(|| format!("reading {}", asset_path.display()))?;
    let export = pkg
        .exports
        .iter()
        .find(|export| p2_script::class_name(pkg, export) == "SkeletalMesh" && pkg.name(export.object_name).eq_ignore_ascii_case(mesh_name))
        .with_context(|| format!("no skeletal mesh {mesh_name}"))?;
    let mesh = p2_assets::read_skeletal_mesh(pkg, data, export)?;
    let parents: Vec<usize> = mesh.bones.iter().map(|bone| bone.parent).collect();
    let bind: Vec<LocalBone> = mesh.bones.iter().map(|bone| (bone.orientation, bone.position)).collect();
    let frames = bone_frames(&parents, &bind, |_| 1.0);
    let bone = |name: &str| mesh.bones.iter().position(|bone| pkg.name(bone.name).eq_ignore_ascii_case(name));
    writeln!(out, "mesh {mesh_name}: {} bones, scale {:?}, origin {:?}, rot origin {:?}", mesh.bones.len(), mesh.scale, mesh.origin, mesh.rot_origin)?;
    for asset in p2_assets::read_karma_assets(&text)? {
        let units = if asset.scale > 0.0 { 1.0 / asset.scale } else { 1.0 };
        let part = |name: &str| {
            let part = asset.parts.iter().find(|part| part.id.eq_ignore_ascii_case(name))?;
            let row = |at: usize| [part.tm[at], part.tm[at + 1], part.tm[at + 2]];
            Some(Frame { axes: [row(0), row(4), row(8)], origin: row(12).map(|c| c * units) })
        };
        let unmatched: Vec<&str> = asset.parts.iter().map(|part| part.id.as_str()).filter(|name| bone(name).is_none()).collect();
        writeln!(out, "asset {}: {} parts, {} joints; parts with no bone of that name: {unmatched:?}", asset.id, asset.parts.len(), asset.joints.len())?;
        writeln!(out, "joint: child in its parent, asset against mesh: turn apart (degrees), place apart (units), bone length asset / mesh")?;
        for joint in &asset.joints {
            let [child, parent] = &joint.parts;
            let (Some(asset_child), Some(asset_parent)) = (part(child), part(parent)) else { continue };
            let (Some(mesh_child), Some(mesh_parent)) = (bone(child), bone(parent)) else {
                writeln!(out, "  {child} on {parent}: no such bones")?;
                continue;
            };
            let (Some(in_asset), Some(in_mesh)) =
                (asset_parent.inverse().map(|inverse| inverse.then(&asset_child)), frames[mesh_parent].inverse().map(|inverse| inverse.then(&frames[mesh_child])))
            else {
                continue;
            };
            let apart = (Vector::from(in_asset.origin) - Vector::from(in_mesh.origin)).size();
            writeln!(
                out,
                "  {child} on {parent}: {:.1} {apart:.1} {:.1} / {:.1}",
                turn_between(&in_asset, &in_mesh),
                Vector::from(in_asset.origin).size(),
                Vector::from(in_mesh.origin).size()
            )?;
        }
    }
    Ok(())
}

const WALK_EYES: usize = 80;
const WALK_RAYS: i32 = 8;
const EYE_HEIGHT: f32 = 40.0;
const WALL_GAPS: [f32; 2] = [30.0, 4.0];

fn node_locations(pkg: &Package, data: &[u8]) -> Vec<Vector> {
    let nodes = pkg.exports.iter().filter(|export| matches!(p2_script::class_name(pkg, export), "PathNode" | "PlayerStart"));
    nodes
        .filter_map(|export| {
            let object = pkg.read_object(data, export).ok()?;
            object.properties.iter().find_map(|property| match &property.value {
                TagValue::Struct { data, .. } if pkg.name(property.name) == "Location" => {
                    let number = |at: usize| Some(f32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
                    Some(Vector::new(number(0)?, number(4)?, number(8)?))
                }
                _ => None,
            })
        })
        .collect()
}

fn view_sides(eye: Vector, forward: Vector, right: Vector, up: Vector) -> Vec<Plane> {
    [right, -right, up, -up]
        .into_iter()
        .map(|side| {
            let normal = (forward - side).safe_normal();
            Plane { normal, d: normal.dot(eye) }
        })
        .collect()
}

#[derive(Default)]
struct WalkTally {
    eyes: usize,
    outside: usize,
    rays: usize,
    unreached: usize,
    outside_volume: usize,
    examples: BTreeMap<(u8, u8), (usize, String)>,
}

fn walk_check(pkg: &Package, data: &[u8], out: &mut impl Write) -> anyhow::Result<()> {
    let model = level_model(pkg, data)?;
    let portals = ZonePortals::new(&model, Vec::new());
    let polygons = model.polygons();
    let sky_zones: Vec<usize> = (0..model.zones.len())
        .filter(|&zone| {
            let export = usize::try_from(model.zones[zone].actor).ok().and_then(|actor| pkg.exports.get(actor.checked_sub(1)?));
            export.is_some_and(|export| p2_script::class_name(pkg, export) == "SkyZoneInfo")
        })
        .collect();
    let drawn: Vec<&ModelPolygon> = polygons
        .iter()
        .filter(|polygon| polygon.poly_flags & (poly_flags::INVISIBLE | poly_flags::PORTAL) == 0)
        .filter(|polygon| !sky_zones.contains(&usize::from(model.nodes[polygon.node].zones[1])))
        .collect();
    let nodes = node_locations(pkg, data);
    let step = nodes.len().div_ceil(WALK_EYES).max(1);
    let level = Vector::new(0.0, 0.0, 1.0);
    let headings = [Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, -1.0, 0.0)];
    let mut seen = VisibleZones::default();
    let mut tallies = [WalkTally::default(), WalkTally::default(), WalkTally::default()];
    for node in nodes.iter().step_by(step) {
        let base = *node + level * EYE_HEIGHT;
        let mut eyes = vec![(0, base)];
        for heading in headings {
            let Some(hit) = p2_collision::line_check(&model, base, base + heading * 4000.0) else { continue };
            let reach = hit.time * 4000.0;
            eyes.extend(WALL_GAPS.iter().enumerate().filter(|&(_, &gap)| reach > gap).map(|(kind, &gap)| (kind + 1, base + heading * (reach - gap))));
        }
        for (kind, eye) in eyes {
            let tally = &mut tallies[kind];
            tally.eyes += 1;
            let zone = p2_collision::point_zone(&model, eye);
            if zone == 0 {
                tally.outside += 1;
                continue;
            }
            for forward in headings {
                let right = forward.cross(level);
                portals.visible(eye, zone, &view_sides(eye, forward, right, level), &mut seen);
                for (i, j) in (0..WALK_RAYS).flat_map(|i| (0..WALK_RAYS).map(move |j| (i, j))) {
                    let across = |k: i32| (k as f32 + 0.5) / WALK_RAYS as f32 * 1.9 - 0.95;
                    let direction = (forward + right * across(i) + level * across(j)).safe_normal();
                    tally.rays += 1;
                    for (distance, polygon) in ray_hits(&drawn, eye, direction) {
                        let node = &model.nodes[polygon.node];
                        let zones = if polygon.poly_flags & poly_flags::TWO_SIDED == 0 { [node.zones[1]; 2] } else { node.zones };
                        let at = eye + direction * distance;
                        let clear = p2_collision::line_check(&model, eye, eye + direction * (distance - 1.0)).is_none();
                        if clear && !zones.contains(&0) {
                            if !zones.iter().any(|&z| seen.reached(z)) {
                                tally.unreached += 1;
                                let example = format!("eye {:.0} {:.0} {:.0} looking {:.2} {:.2} {:.2}, {distance:.0} away", eye.x, eye.y, eye.z, direction.x, direction.y, direction.z);
                                tally.examples.entry((zone, zones[0])).or_insert((0, example)).0 += 1;
                            } else if !zones.iter().any(|&z| seen.sees(z, at, at)) {
                                tally.outside_volume += 1;
                            }
                        }
                        if polygon.poly_flags & (poly_flags::MASKED | poly_flags::TRANSLUCENT) == 0 {
                            break;
                        }
                    }
                }
            }
        }
    }
    writeln!(out, "{} path nodes, every {step}. of them an eye; the walk runs without antiportal actors", nodes.len())?;
    for (name, tally) in ["at the node", "30 from a wall", "4 from a wall"].iter().zip(&tallies) {
        writeln!(
            out,
            "{name}: {} eyes ({} in zone 0, all seen), {} rays; a polygon in sight of a zone the walk did not reach: {}; reached but outside its volumes: {}",
            tally.eyes, tally.outside, tally.rays, tally.unreached, tally.outside_volume
        )?;
        for ((from, to), (count, example)) in &tally.examples {
            writeln!(out, "  eye zone {from}, polygon zone {to}: {count}, first: {example}")?;
        }
    }
    Ok(())
}

fn light_reaches(pkg: &Package, data: &[u8], out: &mut impl Write) -> anyhow::Result<()> {
    let model = level_model(pkg, data)?;
    for line in std::io::stdin().lines() {
        let numbers: Vec<f32> = line?.split_whitespace().filter_map(|part| part.parse().ok()).collect();
        let [x, y, z, radius] = numbers[..] else { continue };
        let zones: Vec<String> = p2_collision::light_zones(&model, Vector::new(x, y, z), radius).iter().map(u8::to_string).collect();
        writeln!(out, "{x} {y} {z} {radius} {radius} {}", zones.join(","))?;
    }
    Ok(())
}

const ABOVE_MESH_REACH: f32 = 1024.0;
const ABOVE_MESH_PROPS: [&str; 12] = [
    "StaticMesh",
    "CullDistance",
    "bAcceptsProjectors",
    "Tag",
    "bUnlit",
    "bStatic",
    "bMovable",
    "bHidden",
    "DrawScale",
    "DrawScale3D",
    "ForcedVisibilityZoneTag",
    "bShadowCast",
];

fn location_of(pkg: &Package, object: &p2_package::ObjectData) -> Option<Vector> {
    object.properties.iter().find_map(|property| match &property.value {
        TagValue::Struct { data, .. } if pkg.name(property.name) == "Location" => {
            let number = |at: usize| Some(f32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
            Some(Vector::new(number(0)?, number(4)?, number(8)?))
        }
        _ => None,
    })
}

fn above_probe(pkg: &Package, data: &[u8], out: &mut impl Write) -> anyhow::Result<()> {
    let model = level_model(pkg, data)?;
    let polygons = model.polygons();
    let sky_zones: Vec<u8> = (0..model.zones.len())
        .filter(|&zone| {
            let export = usize::try_from(model.zones[zone].actor).ok().and_then(|actor| pkg.exports.get(actor.checked_sub(1)?));
            export.is_some_and(|export| p2_script::class_name(pkg, export) == "SkyZoneInfo")
        })
        .filter_map(|zone| u8::try_from(zone).ok())
        .collect();
    let sky_location = pkg
        .exports
        .iter()
        .filter(|export| p2_script::class_name(pkg, export) == "SkyZoneInfo")
        .find_map(|export| location_of(pkg, &pkg.read_object(data, export).ok()?));
    let sky_zone = sky_location.map(|location| p2_collision::point_zone(&model, location));
    writeln!(out, "{} zones; SkyZoneInfo zones {sky_zones:?}; SkyZoneInfo at {sky_location:?} lies in zone {sky_zone:?}", model.zones.len())?;
    let meshes: Vec<(&Export, p2_package::ObjectData, Vector)> = pkg
        .exports
        .iter()
        .filter_map(|export| {
            let object = pkg.read_object(data, export).ok()?;
            object.properties.iter().any(|property| pkg.name(property.name) == "StaticMesh").then_some(())?;
            let location = location_of(pkg, &object)?;
            Some((export, object, location))
        })
        .collect();
    for line in std::io::stdin().lines() {
        let numbers: Vec<f32> = line?.split_whitespace().filter_map(|part| part.parse().ok()).collect();
        let [x, y, z] = numbers[..] else { continue };
        let eye = Vector::new(x, y, z);
        writeln!(out, "\npoint {x} {y} {z}: zone {}", p2_collision::point_zone(&model, eye))?;
        writeln!(out, "  BSP polygons over the point, lowest first: z range, normal z, flags, node zones, in sky, material")?;
        let (up, top) = (Vector::new(0.0, 0.0, 1.0), Vector::new(x, y, 200_000.0));
        let mut over: Vec<(f32, f32, &ModelPolygon)> = polygons
            .iter()
            .filter(|polygon| !ray_hits(&[polygon], eye, up).is_empty() || ray_hits(&[polygon], top, -up).first().is_some_and(|&(distance, _)| top.z - distance > z))
            .map(|polygon| {
                let heights = polygon.positions.iter().map(|p| p[2]);
                (heights.clone().fold(f32::MAX, f32::min), heights.fold(f32::MIN, f32::max), polygon)
            })
            .collect();
        over.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (low, high, polygon) in over {
            let zones = model.nodes[polygon.node].zones;
            writeln!(
                out,
                "    {low:.0}..{high:.0} {:.2} {:#010x} {zones:?} {} {}",
                polygon.normal[2],
                polygon.poly_flags,
                sky_zone == Some(zones[1]),
                pkg.object_name(polygon.material).unwrap_or("-")
            )?;
        }
        writeln!(out, "  actors with a StaticMesh within {ABOVE_MESH_REACH} in xy and above z - 64:")?;
        for (export, object, location) in &meshes {
            let apart = ((location.x - x).powi(2) + (location.y - y).powi(2)).sqrt();
            if apart > ABOVE_MESH_REACH || location.z < z - 64.0 {
                continue;
            }
            write!(out, "    {} {} at {:.0} {:.0} {:.0}:", p2_script::class_name(pkg, export), pkg.name(export.object_name), location.x, location.y, location.z)?;
            for property in object.properties.iter().filter(|property| ABOVE_MESH_PROPS.contains(&pkg.name(property.name))) {
                let value = match &property.value {
                    TagValue::Object(index) => pkg.object_name(*index).unwrap_or("-").to_string(),
                    TagValue::Name(name) => pkg.name(*name).to_string(),
                    TagValue::Struct { data, .. } => format!("{:?}", data.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c)).collect::<Vec<_>>()),
                    other => format!("{other:?}"),
                };
                write!(out, " {}={value}", pkg.name(property.name))?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().context(USAGE)?;
    let mode = args.next();
    let mode = mode.as_ref().and_then(|m| m.to_str());
    let mut out = BufWriter::new(std::io::stdout().lock());
    if mode == Some("--shaders") {
        return shader_census(Path::new(&path), &mut out);
    }
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let pkg = Package::parse(&data).with_context(|| format!("parsing {}", path.display()))?;
    match mode {
        None => dump(&pkg, &mut out)?,
        Some("--props") => dump_properties(&pkg, &data, &mut out)?,
        Some("--reach") => light_reaches(&pkg, &data, &mut out)?,
        Some("--zones") => zone_survey(&pkg, &data, &mut out)?,
        Some("--walk") => walk_check(&pkg, &data, &mut out)?,
        Some("--above") => above_probe(&pkg, &data, &mut out)?,
        Some("--rig") => {
            let (asset, mesh) = (args.next().context(USAGE)?, args.next().context(USAGE)?);
            rig_check(&pkg, &data, Path::new(&asset), &mesh.to_string_lossy(), &mut out)?;
        }
        Some("--code") => dump_code(&pkg, &data, &natives_near(Path::new(&path)), &mut out)?,
        Some(_) => anyhow::bail!(USAGE),
    }
    Ok(())
}

fn natives_near(path: &Path) -> HashMap<u16, NativeName> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let system = [dir.to_path_buf(), dir.join("..").join("System")].into_iter().find(|d| d.join("Core.u").is_file());
    let mut natives = HashMap::new();
    let Some(entries) = system.and_then(|system| std::fs::read_dir(system).ok()) else { return natives };
    for path in entries.flatten().map(|e| e.path()) {
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")) {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else { continue };
        match Package::parse(&data) {
            Ok(pkg) => natives.extend(native_names(&pkg, &data)),
            Err(e) => eprintln!("native names: {}: {e}", path.display()),
        }
    }
    natives
}

fn dump_code(pkg: &Package, data: &[u8], natives: &HashMap<u16, NativeName>, out: &mut impl Write) -> std::io::Result<()> {
    for index in 0..pkg.exports.len() {
        let object = match read_export(pkg, data, index) {
            Ok(Some(object)) => object,
            Ok(None) => continue,
            Err(e) => {
                writeln!(out, "\nexport {} ERROR {e}", index + 1)?;
                continue;
            }
        };
        let Some(base) = object.as_struct().filter(|s| s.script_size > 0) else { continue };
        let kind = match object {
            ScriptObject::Class(_) => "class",
            ScriptObject::State(_) => "state",
            ScriptObject::Function(_) => "function",
            _ => "struct",
        };
        let export = &pkg.exports[index];
        writeln!(out, "\n{kind} {}.{}", object_name(pkg, export.package_index), pkg.name(export.object_name))?;
        match decode_script(pkg, &base.script, base.script_size) {
            Ok(statements) => write!(out, "{}", disassemble(pkg, &statements, natives))?,
            Err(e) => writeln!(out, "ERROR {e}")?,
        }
    }
    out.flush()
}

fn dump_properties(pkg: &Package, data: &[u8], out: &mut impl Write) -> std::io::Result<()> {
    for (i, exp) in pkg.exports.iter().enumerate().filter(|(_, e)| e.serial_size > 0) {
        write!(out, "{:6} {} {}", i + 1, p2_script::class_name(pkg, exp), pkg.name(exp.object_name))?;
        let object = match pkg.read_object(data, exp) {
            Ok(object) => object,
            Err(e) => {
                writeln!(out, " ERROR {e}")?;
                continue;
            }
        };
        let end = exp.serial_offset + exp.serial_size;
        let Some(native) = end.checked_sub(object.native_offset) else {
            writeln!(out, " ERROR properties end at {:#x}, past the export's end {end:#x}", object.native_offset)?;
            continue;
        };
        writeln!(out, " native {native:#x} bytes")?;
        for p in &object.properties {
            write!(out, "         {}[{}] = ", pkg.name(p.name), p.array_index)?;
            match &p.value {
                TagValue::Object(index) => writeln!(out, "Object {}", object_name(pkg, *index))?,
                TagValue::Name(name) => writeln!(out, "Name {}", pkg.name(*name))?,
                TagValue::Struct { name, data } => writeln!(out, "Struct {} {data:02x?}", pkg.name(*name))?,
                value => writeln!(out, "{value:?}")?,
            }
        }
    }
    out.flush()
}

fn shader_census(dir: &Path, out: &mut impl Write) -> anyhow::Result<()> {
    let mut forms: HashMap<String, usize> = HashMap::new();
    let mut multi_pass = Vec::new();
    let (mut skipped_packages, mut skipped_shaders) = (0usize, 0usize);
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?.flatten().map(|e| e.path()) {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let extension = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(extension.as_deref(), Some("utx" | "usx" | MAP_EXTENSION)) {
                continue;
            }
            let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let Ok(pkg) = Package::parse(&data) else {
                skipped_packages += 1;
                continue;
            };
            for exp in pkg.exports.iter().filter(|e| e.serial_size > 0 && object_name(&pkg, e.class_index) == "Shader") {
                let Some((form, several_passes)) = shader_form(&pkg, &data, exp) else {
                    skipped_shaders += 1;
                    continue;
                };
                if several_passes {
                    let file = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
                    multi_pass.push(format!("{file} {}: {form}", pkg.name(exp.object_name)));
                }
                *forms.entry(form).or_default() += 1;
            }
        }
    }
    let mut forms: Vec<_> = forms.into_iter().collect();
    forms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    writeln!(out, "forms ({} shaders)", forms.iter().map(|(_, n)| n).sum::<usize>())?;
    for (form, count) in forms {
        writeln!(out, "{count:5} {form}")?;
    }
    multi_pass.sort();
    writeln!(out, "\nmulti-pass ({})", multi_pass.len())?;
    for line in multi_pass {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "\nskipped: {skipped_packages} packages and {skipped_shaders} shaders that could not be read")?;
    out.flush()?;
    Ok(())
}

fn shader_form(pkg: &Package, data: &[u8], exp: &Export) -> Option<(String, bool)> {
    let object = pkg.read_object(data, exp).ok()?;
    let property = |name: &str| object.properties.iter().find(|p| pkg.name(p.name) == name).map(|p| &p.value);
    let reference = |name| match property(name) {
        Some(TagValue::Object(index)) => *index,
        _ => 0,
    };
    let (diffuse, opacity) = (reference("Diffuse"), reference("Opacity"));
    let (glow, glow_mask) = (reference("SelfIllumination"), reference("SelfIlluminationMask"));
    let (specular, mask) = (reference("Specular"), reference("SpecularityMask"));
    let opacity_form = match opacity {
        0 => "none".to_owned(),
        _ if opacity == diffuse => "diffuse".to_owned(),
        _ => material_chain(pkg, data, opacity),
    };
    let glow_form = match (glow, glow_mask) {
        (0, _) => "none".to_owned(),
        (_, 0) => "unlit".to_owned(),
        _ if glow_mask == glow => "own-alpha".to_owned(),
        _ => format!("masked:{}", material_chain(pkg, data, glow_mask)),
    };
    let mask_form = match mask {
        0 => "none".to_owned(),
        _ if mask == diffuse => "diffuse".to_owned(),
        _ => material_chain(pkg, data, mask),
    };
    let blending = match property("OutputBlending") {
        Some(TagValue::Byte(b)) => *b,
        _ => 0,
    };
    let form = format!(
        "opacity={opacity_form} glow={glow_form} specular={} mask={mask_form} blend={blending}",
        material_chain(pkg, data, specular)
    );
    let several_passes = opacity != 0 && opacity != diffuse && ((glow != 0 && glow_mask != 0) || specular != 0);
    Some((form, several_passes))
}

fn material_chain(pkg: &Package, data: &[u8], mut index: i32) -> String {
    let mut classes = Vec::new();
    for _ in 0..8 {
        if index == 0 {
            break;
        }
        let Ok(slot) = usize::try_from(index) else {
            let import = usize::try_from(!index).ok().and_then(|i| pkg.imports.get(i));
            classes.push(format!("import:{}", import.map_or("?", |import| pkg.name(import.class_name))));
            break;
        };
        let Some(exp) = pkg.exports.get(slot - 1) else { break };
        classes.push(object_name(pkg, exp.class_index).to_owned());
        let material = pkg.read_object(data, exp).ok().and_then(|object| {
            object.properties.iter().find(|p| pkg.name(p.name) == "Material").and_then(|p| match p.value {
                TagValue::Object(next) => Some(next),
                _ => None,
            })
        });
        match material {
            Some(next) => index = next,
            None => break,
        }
    }
    if classes.is_empty() { "-".to_owned() } else { classes.join(">") }
}

fn object_name(pkg: &Package, index: i32) -> &str {
    match index {
        0 => "None",
        _ => pkg.object_name(index).unwrap_or("<invalid>"),
    }
}

fn dump(pkg: &Package, out: &mut impl Write) -> std::io::Result<()> {
    let s = &pkg.summary;
    writeln!(out, "version {} licensee {} flags {:#010x}", s.version, s.licensee, s.package_flags)?;
    let guid: String = s.guid.iter().map(|b| format!("{b:02x}")).collect();
    writeln!(out, "guid {guid}")?;
    for (i, g) in s.generations.iter().enumerate() {
        writeln!(out, "generation {i}: exports {} names {}", g.export_count, g.name_count)?;
    }

    writeln!(out, "\nnames ({}) at {:#x}", pkg.names.len(), s.name_offset)?;
    for (i, n) in pkg.names.iter().enumerate() {
        writeln!(out, "{i:6} {:#010x} {}", n.flags, n.text)?;
    }

    writeln!(out, "\nimports ({}) at {:#x}", pkg.imports.len(), s.import_offset)?;
    for (i, imp) in pkg.imports.iter().enumerate() {
        writeln!(
            out,
            "{:6} {}.{} {} outer {}",
            -(i as i64) - 1,
            pkg.name(imp.class_package),
            pkg.name(imp.class_name),
            pkg.name(imp.object_name),
            object_name(pkg, imp.package_index),
        )?;
    }

    writeln!(out, "\nexports ({}) at {:#x}", pkg.exports.len(), s.export_offset)?;
    for (i, exp) in pkg.exports.iter().enumerate() {
        writeln!(
            out,
            "{:6} {} {} super {} outer {} flags {:#010x} data {:#x}+{:#x}",
            i + 1,
            p2_script::class_name(pkg, exp),
            pkg.name(exp.object_name),
            object_name(pkg, exp.super_index),
            object_name(pkg, exp.package_index),
            exp.object_flags,
            exp.serial_offset,
            exp.serial_size,
        )?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_core_u() {
        let Some(dir) = std::env::var_os("P2_GAME_DIR") else {
            eprintln!("P2_GAME_DIR unset, skipping");
            return;
        };
        let data = std::fs::read(Path::new(&dir).join("System/Core.u")).unwrap();
        let pkg = Package::parse(&data).unwrap();
        let mut out = Vec::new();
        dump(&pkg, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("version 118 licensee 2"));
        assert!(!text.contains("<invalid>"));
    }
}
