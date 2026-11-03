mod common;

use common::game_dir;

use p2_package::Loader;
use p2_render_cpu::{HardwareKind, HardwareLight, MeshLight};
use p2_vm::Vm;

const LE_SUNLIGHT: p2_render_cpu::LightEffect = p2_render_cpu::LightEffect(20);

fn loader() -> Option<Loader> {
    let dir = game_dir()?;
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    Some(loader)
}

fn map_lights(loader: &mut Loader, map: &str) -> Vec<MeshLight> {
    let id = loader.load(map).unwrap();
    let mut vm = Vm::new(&*loader);
    let exports = loader.package(id).package.exports.len();
    let objects: Vec<_> = (0..exports).filter_map(|index| vm.load_object(id, index).ok()).collect();
    objects.into_iter().filter_map(|object| p2_engine::actors::mesh_light(&vm, object)).collect()
}

#[test]
fn outdoor_maps_have_a_sun_shining_down() {
    let Some(mut loader) = loader() else { return };
    for map in ["Suburbs-3", "Suburbs-1"] {
        let lights = map_lights(&mut loader, map);
        let suns: Vec<_> = lights.iter().filter(|light| light.effect == LE_SUNLIGHT).collect();
        assert!(!suns.is_empty(), "{map}: no LE_Sunlight light among {} lights", lights.len());
        for sun in suns {
            let hardware = HardwareLight::new(sun, 1.0, Default::default(), 0.0).expect("a sun the actors are lit by");
            assert_eq!(hardware.kind, HardwareKind::Directional);
            assert!(hardware.direction[2] < 0.0, "{map}: a sun shining upward {:?}", hardware.direction);
            assert!(hardware.color.iter().sum::<f32>() > 0.0, "{map}: a black sun");
        }
    }
}
