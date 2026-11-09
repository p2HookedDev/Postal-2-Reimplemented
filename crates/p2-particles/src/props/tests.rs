use super::*;

#[test]
fn lookup_overrides_defaults() {
    let lookup = |name: &str| match name {
        "MaxParticles" => Some(PropertyValue::Int(30)),
        "StartSizeRange" => Some(PropertyValue::Struct(
            [(5.0, 10.0), (6.0, 11.0), (7.0, 12.0)]
                .map(|(min, max)| PropertyValue::Struct(vec![PropertyValue::Float(min), PropertyValue::Float(max)]))
                .to_vec(),
        )),
        "ColorScale" => Some(PropertyValue::Array(vec![PropertyValue::Struct(vec![
            PropertyValue::Float(0.5),
            PropertyValue::Struct([10, 20, 30, 40].map(PropertyValue::Int).to_vec()),
        ])])),
        "DrawStyle" => Some(PropertyValue::Int(1)),
        _ => None,
    };
    let p = EmitterProperties::from_lookup(lookup);
    assert_eq!(p.max_particles, 30);
    assert_eq!(p.start_size_range.y, Range::new(6.0, 11.0));
    assert_eq!(p.color_scale, vec![ColorKey { time: 0.5, color: [30, 20, 10, 40] }]);
    assert_eq!(p.draw_style, DrawStyle::AlphaBlend);
    assert_eq!(p.lifetime_range, Range::new(4.0, 4.0));
}
