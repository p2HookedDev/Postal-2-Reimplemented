use crate::Error;

/// One `ASSET` of a `.ka` file. Lengths are asset units: Unreal units × `scale`.
#[derive(Debug, Clone, PartialEq)]
pub struct KarmaAsset {
    pub id: String,
    /// Asset units per Unreal unit (0.02 in every shipped file: Karma's 1/50).
    pub scale: f32,
    pub mass_scale: f32,
    pub geometries: Vec<KaGeometry>,
    pub models: Vec<KaModel>,
    pub parts: Vec<KaPart>,
    pub joints: Vec<KaJoint>,
    pub no_collision: Vec<[String; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KaGeometry {
    pub id: String,
    pub primitives: Vec<KaPrimitive>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KaPrimitive {
    pub shape: KaShape,
    pub tm: KaMatrix,
}

/// 16 floats as stored: rows X axis, Y axis, Z axis, origin (each with a 4th entry 0, 0, 0, 1). A point `p` of the
/// primitive is at `p.X·X + p.Y·Y + p.Z·Z + origin` in the part frame.
pub type KaMatrix = [f32; 16];

#[derive(Debug, Clone, PartialEq)]
pub enum KaShape {
    Sphyl { radius: f32, height: f32 },
    Sphere { radius: f32 },
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct KaModel {
    pub id: String,
    pub dynamic: bool,
    pub collides: bool,
    pub geometry: Option<String>,
    pub mass: f32,
    pub mass_offset: [f32; 3],
    pub inertia: [f32; 6],
    pub linear_damping: f32,
    pub angular_damping: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KaPart {
    pub id: String,
    pub model: String,
    pub parent: Option<String>,
    pub tm: KaMatrix,
}

/// A `JOINT` between `parts[0]` (`part1`, the child bone) and `parts[1]` (`part2`, its parent): per part the joint's
/// position and its primary and orthogonal axes in that part's frame.
#[derive(Debug, Clone, PartialEq)]
pub struct KaJoint {
    pub id: String,
    pub parts: [String; 2],
    pub kind: KaJointKind,
    pub positions: [[f32; 3]; 2],
    pub primary_axes: [[f32; 3]; 2],
    pub orthogonal_axes: [[f32; 3]; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub enum KaJointKind {
    Hinge { high: f32, low: f32, high_stiffness: f32, low_stiffness: f32, limited: bool },
    Skeletal {
        cone_type: KaConeType,
        cone_half_angles: [f32; 2],
        cone_stiffness: f32,
        cone_damping: f32,
        twist_type: KaTwistType,
        twist_half_angle: f32,
        twist_stiffness: f32,
        twist_damping: f32,
    },
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KaConeType {
    Free,
    Slot,
    Cone,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KaTwistType {
    Free,
    Limited,
    Fixed,
}

pub fn read_karma_assets(text: &str) -> Result<Vec<KarmaAsset>, Error> {
    let root = parse(text)?;
    let karma = root.iter().find(|e| e.name == "KARMA").ok_or_else(|| ka("no KARMA element"))?;
    karma.children.iter().filter(|e| e.name == "ASSET").map(asset).collect()
}

fn ka(message: impl Into<String>) -> Error {
    Error::KarmaAsset(message.into())
}

fn asset(e: &Element) -> Result<KarmaAsset, Error> {
    let attribute_float = |name: &str, default: f32| e.attribute(name).map_or(Ok(default), |v| number(v, name));
    let mut asset = KarmaAsset {
        id: e.required("id")?.to_owned(),
        scale: attribute_float("scale", 1.0)?,
        mass_scale: attribute_float("mass_scale", 1.0)?,
        geometries: Vec::new(),
        models: Vec::new(),
        parts: Vec::new(),
        joints: Vec::new(),
        no_collision: Vec::new(),
    };
    for child in &e.children {
        match child.name {
            "GEOMETRY" => asset.geometries.push(geometry(child)?),
            "MODEL" => asset.models.push(model(child)?),
            "PART" => asset.parts.push(KaPart {
                id: child.required("id")?.to_owned(),
                model: child.required("model")?.to_owned(),
                parent: child.attribute("parent").map(str::to_owned),
                tm: child.floats("TM")?,
            }),
            "JOINT" => asset.joints.push(joint(child)?),
            "NO_COLLISION" => asset.no_collision.push([child.required("part1")?.to_owned(), child.required("part2")?.to_owned()]),
            _ => {}
        }
    }
    Ok(asset)
}

fn geometry(e: &Element) -> Result<KaGeometry, Error> {
    let primitives = e
        .children
        .iter()
        .filter(|c| c.name == "PRIMITIVE")
        .map(|p| {
            let shape = match p.required("type")? {
                "sphyl" => KaShape::Sphyl { radius: p.float("RADIUS")?, height: p.float("HEIGHT")? },
                "sphere" => KaShape::Sphere { radius: p.float("RADIUS")? },
                other => KaShape::Other(other.to_owned()),
            };
            Ok(KaPrimitive { shape, tm: p.floats("TM")? })
        })
        .collect::<Result<_, Error>>()?;
    Ok(KaGeometry { id: e.required("id")?.to_owned(), primitives })
}

fn model(e: &Element) -> Result<KaModel, Error> {
    let kind = e.required("type")?;
    let (dynamic, collides) = match kind {
        "dynamics_only" => (true, false),
        "dynamics_and_geometry" => (true, true),
        "geometry_only" => (false, true),
        other => return Err(ka(format!("model type {other}"))),
    };
    let dynamics = e.child("DYNAMICS");
    let float = |name: &str, default: f32| dynamics.map_or(Ok(default), |d| d.child(name).map_or(Ok(default), |_| d.float(name)));
    Ok(KaModel {
        id: e.required("id")?.to_owned(),
        dynamic,
        collides,
        geometry: e.attribute("geometry").map(str::to_owned),
        mass: float("MASS", 1.0)?,
        mass_offset: dynamics.map_or(Ok([0.0; 3]), |d| d.floats("MASS_OFFSET"))?,
        inertia: dynamics.map_or(Ok([1.0, 0.0, 0.0, 1.0, 0.0, 1.0]), |d| d.floats("INERTIA"))?,
        linear_damping: float("LIN_DAMP", 0.0)?,
        angular_damping: float("ANG_DAMP", 0.0)?,
    })
}

fn cone_type(e: &Element) -> Result<KaConeType, Error> {
    Ok(match e.float("CONE_TYPE")? as i32 {
        0 => KaConeType::Free,
        1 => KaConeType::Slot,
        3 => KaConeType::Fixed,
        _ => KaConeType::Cone,
    })
}

fn twist_type(e: &Element) -> Result<KaTwistType, Error> {
    Ok(match e.float("TWIST_TYPE")? as i32 {
        0 => KaTwistType::Free,
        1 => KaTwistType::Limited,
        _ => KaTwistType::Fixed,
    })
}

fn joint(e: &Element) -> Result<KaJoint, Error> {
    let kind = match e.required("type")? {
        "hinge" => KaJointKind::Hinge {
            high: e.float("HIGH_LIMIT")?,
            low: e.float("LOW_LIMIT")?,
            high_stiffness: e.float("HIGH_STIFFNESS")?,
            low_stiffness: e.float("LOW_STIFFNESS")?,
            limited: e.float("LIMITED")? != 0.0,
        },
        "skeletal" => KaJointKind::Skeletal {
            cone_type: cone_type(e)?,
            cone_half_angles: [e.float("CONE_HALF_ANGLE_X")?, e.float("CONE_HALF_ANGLE_Y")?],
            cone_stiffness: e.float("CONE_STIFFNESS")?,
            cone_damping: e.float("CONE_DAMPING")?,
            twist_type: twist_type(e)?,
            twist_half_angle: e.float("TWIST_HALF_ANGLE")?,
            twist_stiffness: e.float("TWIST_STIFFNESS")?,
            twist_damping: e.float("TWIST_DAMPING")?,
        },
        other => KaJointKind::Other(other.to_owned()),
    };
    Ok(KaJoint {
        id: e.required("id")?.to_owned(),
        parts: [e.required("part1")?.to_owned(), e.required("part2")?.to_owned()],
        kind,
        positions: [e.floats("POS1")?, e.floats("POS2")?],
        primary_axes: [e.floats("PRIMARY_AXIS1")?, e.floats("PRIMARY_AXIS2")?],
        orthogonal_axes: [e.floats("ORTHOGONAL_AXIS1")?, e.floats("ORTHOGONAL_AXIS2")?],
    })
}

fn number(text: &str, what: &str) -> Result<f32, Error> {
    text.trim().parse().map_err(|_| ka(format!("{what}: not a number: {text}")))
}

struct Element<'a> {
    name: &'a str,
    attributes: Vec<(&'a str, &'a str)>,
    text: &'a str,
    children: Vec<Self>,
}

impl<'a> Element<'a> {
    fn attribute(&self, name: &str) -> Option<&'a str> {
        self.attributes.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    fn required(&self, name: &str) -> Result<&'a str, Error> {
        self.attribute(name).ok_or_else(|| ka(format!("{} without {name}", self.name)))
    }

    fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|c| c.name == name)
    }

    fn float(&self, name: &str) -> Result<f32, Error> {
        let child = self.child(name).ok_or_else(|| ka(format!("{} without {name}", self.name)))?;
        number(child.text, name)
    }

    fn floats<const N: usize>(&self, name: &str) -> Result<[f32; N], Error> {
        let child = self.child(name).ok_or_else(|| ka(format!("{} without {name}", self.name)))?;
        let values: Vec<f32> = child.text.split(',').map(|v| number(v, name)).collect::<Result<_, _>>()?;
        values.try_into().map_err(|v: Vec<f32>| ka(format!("{name}: {} values, {N} expected", v.len())))
    }
}

fn parse(text: &str) -> Result<Vec<Element<'_>>, Error> {
    let mut stack: Vec<Element> = Vec::new();
    let mut top = Vec::new();
    let mut rest = text;
    let at = |rest: &str| text.len() - rest.len();
    while let Some(open) = rest.find('<') {
        let between = &rest[..open];
        rest = &rest[open..];
        if let Some(element) = stack.last_mut()
            && element.children.is_empty()
            && !between.trim().is_empty()
        {
            element.text = between.trim();
        }
        if let Some(skipped) = rest.strip_prefix("<?").or_else(|| rest.strip_prefix("<!--")) {
            let end = if rest.starts_with("<?") { "?>" } else { "-->" };
            let close = skipped.find(end).ok_or_else(|| ka(format!("unclosed declaration at byte {}", at(rest))))?;
            rest = &skipped[close + end.len()..];
            continue;
        }
        let close = rest.find('>').ok_or_else(|| ka(format!("unclosed tag at byte {}", at(rest))))?;
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let element = stack.pop().ok_or_else(|| ka(format!("stray </{name}>")))?;
            if element.name != name.trim() {
                return Err(ka(format!("<{}> closed by </{name}>", element.name)));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => top.push(element),
            }
            continue;
        }
        let (tag, empty) = match tag.strip_suffix('/') {
            Some(tag) => (tag, true),
            None => (tag, false),
        };
        let element = start_tag(tag)?;
        if empty {
            match stack.last_mut() {
                Some(parent) => parent.children.push(element),
                None => top.push(element),
            }
        } else {
            stack.push(element);
        }
    }
    match stack.last() {
        Some(open) => Err(ka(format!("<{}> not closed", open.name))),
        None => Ok(top),
    }
}

fn start_tag(tag: &str) -> Result<Element<'_>, Error> {
    let tag = tag.trim();
    let (name, mut rest) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
    let mut attributes = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let (key, value) = rest.split_once('=').ok_or_else(|| ka(format!("attribute without value in <{tag}>")))?;
        let value = value.trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'').ok_or_else(|| ka(format!("unquoted attribute in <{tag}>")))?;
        let end = value[1..].find(quote).ok_or_else(|| ka(format!("unclosed attribute in <{tag}>")))?;
        attributes.push((key.trim(), &value[1..=end]));
        rest = &value[end + 2..];
    }
    Ok(Element { name, attributes, text: "", children: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<KARMA ka_file_version="1.0">
	<ASSET id="Test_Skel" graphic="Test.PSK" scale="0.02" mass_scale="1" length_scale="1">
		<GEOMETRY id="bone a">
			<PRIMITIVE id="bone a_1" type="sphyl">
				<RADIUS>0.1</RADIUS>
				<HEIGHT>0.5</HEIGHT>
				<TM>0,0,-1,0,0,1,0,0,1,0,0,0,0.25,0,0,1</TM>
			</PRIMITIVE>
		</GEOMETRY>
		<MODEL id="bone a" type="dynamics_and_geometry" geometry="bone a">
			<DYNAMICS>
				<MASS>0.07</MASS>
				<DENSITY>1</DENSITY>
				<MASS_OFFSET>0.25,0,0</MASS_OFFSET>
				<INERTIA>1,0,0,2,0,3</INERTIA>
				<LIN_DAMP>0</LIN_DAMP>
				<ANG_DAMP>0.5</ANG_DAMP>
			</DYNAMICS>
		</MODEL>
		<PART id="bone a" model="bone a" parent="root">
			<TM>1,0,0,0,0,1,0,0,0,0,1,0,0,1,0,1</TM>
		</PART>
		<NO_COLLISION part1="root" part2="bone a"></NO_COLLISION>
		<JOINT id="bone a" part1="bone a" part2="root" type="hinge">
			<HIGH_LIMIT>1.7</HIGH_LIMIT>
			<LOW_LIMIT>-0.1</LOW_LIMIT>
			<HIGH_STIFFNESS>100</HIGH_STIFFNESS>
			<LOW_STIFFNESS>1100</LOW_STIFFNESS>
			<LIMITED>1</LIMITED>
			<POS1>0,0,0</POS1>
			<POS2>0.7,0,0</POS2>
			<PRIMARY_AXIS1>0,0,1</PRIMARY_AXIS1>
			<PRIMARY_AXIS2>0,0,1</PRIMARY_AXIS2>
			<ORTHOGONAL_AXIS1>0,1,0</ORTHOGONAL_AXIS1>
			<ORTHOGONAL_AXIS2>0,1,0</ORTHOGONAL_AXIS2>
		</JOINT>
	</ASSET>
</KARMA>"#;

    #[test]
    fn reads_every_element() {
        let assets = read_karma_assets(SAMPLE).unwrap();
        let [asset] = assets.as_slice() else { panic!("{assets:?}") };
        assert_eq!((asset.id.as_str(), asset.scale), ("Test_Skel", 0.02));
        assert_eq!(asset.geometries[0].primitives[0].shape, KaShape::Sphyl { radius: 0.1, height: 0.5 });
        assert_eq!(asset.geometries[0].primitives[0].tm[12], 0.25);
        let model = &asset.models[0];
        assert!(model.dynamic && model.collides && model.geometry.as_deref() == Some("bone a"));
        assert_eq!((model.mass, model.inertia[3], model.angular_damping), (0.07, 2.0, 0.5));
        assert_eq!(asset.parts[0].parent.as_deref(), Some("root"));
        assert_eq!(asset.no_collision, vec![["root".to_owned(), "bone a".to_owned()]]);
        let joint = &asset.joints[0];
        assert_eq!(joint.parts, ["bone a".to_owned(), "root".to_owned()]);
        assert_eq!(joint.positions[1], [0.7, 0.0, 0.0]);
        assert!(matches!(joint.kind, KaJointKind::Hinge { low, high, limited: true, .. } if low == -0.1 && high == 1.7));
    }

    #[test]
    fn bad_nesting_is_an_error() {
        assert!(read_karma_assets("<KARMA><ASSET id=\"a\"></KARMA>").is_err());
    }
}
