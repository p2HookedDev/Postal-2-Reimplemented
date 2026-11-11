use super::*;

pub(super) struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, zone_fog.in_set(SceneViewSystems::Fog));
    }
}

fn zone_fog(
    time: Res<Time>,
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    mut cameras: Query<(&SceneView, &mut DistanceFog)>,
) {
    let model = collision.model();
    let t = settle_step(time.delta_secs(), FOG_BLEND_SECONDS);
    for (view, mut fog) in &mut cameras {
        let zone = model.and_then(|model| {
            let position = to_bevy(view.position - shown.origin);
            shown.fogs.get(usize::from(p2_collision::point_zone(model, position.into()))).copied().flatten()
        });
        let target = zone.map(|zone| {
            let [r, g, b] = zone.color;
            (Color::srgb_u8(r, g, b).to_linear(), zone.start, zone.end.max(zone.start + 1.0))
        });
        let (start, end) = match fog.falloff {
            FogFalloff::Linear { start, end } => (start, end),
            _ => (0.0, 1.0),
        };
        let shown = (fog.color.to_linear(), start, end);
        let blended = blend_fog(shown, target, t);
        if blended != shown {
            let (color, start, end) = blended;
            fog.color = color.into();
            fog.falloff = FogFalloff::Linear { start, end };
        }
    }
}

/// Retail snaps zone fog at a zone crossing; ours fades in both looks.
const FOG_BLEND_SECONDS: f32 = 0.35;

/// `target` `None` = no fog: the colour fades out where it is. Fog coming from none starts at the target's colour and
/// distances, fully transparent, so only its strength fades in; within 1/512 it lands on the target (no fog lands on
/// [`LinearRgba::NONE`], as retail shows it).
pub(super) fn blend_fog(shown: (LinearRgba, f32, f32), target: Option<(LinearRgba, f32, f32)>, t: f32) -> (LinearRgba, f32, f32) {
    let (color, start, end) = shown;
    let landed = target.map_or(LinearRgba::NONE, |(to, _, _)| to);
    let (to, to_start, to_end) = target.unwrap_or((color.with_alpha(0.0), start, end));
    let (color, start, end) = if color.alpha <= 0.0 { (to.with_alpha(0.0), to_start, to_end) } else { (color, start, end) };
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let blended = LinearRgba::new(mix(color.red, to.red), mix(color.green, to.green), mix(color.blue, to.blue), mix(color.alpha, to.alpha));
    let near = [blended.red - to.red, blended.green - to.green, blended.blue - to.blue, blended.alpha - to.alpha].iter().all(|d| d.abs() < 1.0 / 512.0);
    let (start, end) = (mix(start, to_start), mix(end, to_end));
    if near && (start - to_start).abs() < 1.0 && (end - to_end).abs() < 1.0 { (landed, to_start, to_end) } else { (blended, start, end) }
}
