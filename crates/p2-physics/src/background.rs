use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::{Impact, World};

type Change = Box<dyn FnOnce(&mut World) + Send + Sync>;

struct Worker {
    steps: Sender<(World, f32)>,
    done: Mutex<Receiver<(World, Vec<Impact>)>>,
}

pub struct Background {
    world: Option<World>,
    worker: Option<Worker>,
    stepping: bool,
    queued: Vec<Change>,
    impacts: Vec<Impact>,
    waits: (u32, Duration),
}

impl Background {
    pub fn new(world: World) -> Self {
        Self { world: Some(world), worker: None, stepping: false, queued: Vec::new(), impacts: Vec::new(), waits: (0, Duration::ZERO) }
    }

    pub fn start(&mut self, dt: f32) {
        self.finish();
        let Some(mut world) = self.world.take() else { return };
        if self.worker.is_none() {
            self.worker = spawn();
        }
        match &self.worker {
            Some(worker) => match worker.steps.send((world, dt)) {
                Ok(()) => self.stepping = true,
                Err(returned) => {
                    self.worker = None;
                    let (mut world, dt) = returned.0;
                    self.impacts.extend(world.step(dt));
                    self.world = Some(world);
                }
            },
            None => {
                self.impacts.extend(world.step(dt));
                self.world = Some(world);
            }
        }
    }

    pub fn world(&mut self) -> Option<&mut World> {
        self.finish();
        self.world.as_mut()
    }

    pub fn change(&mut self, change: impl FnOnce(&mut World) + Send + Sync + 'static) {
        match (&mut self.world, self.stepping) {
            (Some(world), false) => change(world),
            _ => self.queued.push(Box::new(change)),
        }
    }

    pub fn take_impacts(&mut self) -> Vec<Impact> {
        self.finish();
        std::mem::take(&mut self.impacts)
    }

    pub fn take_waits(&mut self) -> (u32, Duration) {
        std::mem::replace(&mut self.waits, (0, Duration::ZERO))
    }

    fn finish(&mut self) {
        if self.stepping {
            self.stepping = false;
            let Some(done) = self.worker.as_mut().and_then(|w| w.done.get_mut().ok()) else { return };
            let result = match done.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => {
                    let started = Instant::now();
                    let result = done.recv().ok();
                    self.waits.0 += 1;
                    self.waits.1 += started.elapsed();
                    result
                }
                Err(TryRecvError::Disconnected) => None,
            };
            match result {
                Some((world, impacts)) => {
                    self.world = Some(world);
                    self.impacts.extend(impacts);
                }
                None => self.worker = None,
            }
        }
        let Some(world) = &mut self.world else {
            self.queued.clear();
            return;
        };
        for change in self.queued.drain(..) {
            change(world);
        }
    }
}

fn spawn() -> Option<Worker> {
    let (steps, jobs) = channel::<(World, f32)>();
    let (results, done) = channel();
    let thread = std::thread::Builder::new().name("physics".to_owned()).spawn(move || {
        for (mut world, dt) in jobs {
            let impacts = world.step(dt);
            if results.send((world, impacts)).is_err() {
                break;
            }
        }
    });
    thread.ok().map(|_| Worker { steps, done: Mutex::new(done) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyParams, Shape};

    fn ball(world: &mut World) -> crate::BodyId {
        world.add_body(&BodyParams {
            shapes: vec![Shape::Sphere([0.0; 3].into(), 10.0)],
            location: [0.0; 3].into(),
            rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 1.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.0,
            restitution: 0.0,
            asleep: false,
            stay_upright: false,
            allow_yaw: false,
        })
    }

    #[test]
    fn changes_queued_during_a_step_apply_after_it_in_order() {
        let mut world = World::new([0.0; 3].into());
        let id = ball(&mut world);
        let mut background = Background::new(world);
        background.start(0.1);
        background.change(move |w| w.set_velocity(id, [100.0, 0.0, 0.0].into()));
        background.change(move |w| w.set_velocity(id, [0.0, 50.0, 0.0].into()));
        let velocity = background.world().unwrap().velocity(id).unwrap();
        assert_eq!(velocity, [0.0, 50.0, 0.0].into());
        background.start(0.1);
        let (p2_math::Vector { x, y, .. }, _) = background.world().unwrap().pose(id).unwrap();
        assert!(x.abs() < 1e-3 && (y - 5.0).abs() < 0.01, "at {x} {y}");
    }

    #[test]
    fn impacts_of_background_steps_reach_the_caller() {
        let mut world = World::new([0.0, 0.0, -950.0].into());
        let vertices = [[-100.0, -100.0, 0.0], [100.0, -100.0, 0.0], [100.0, 100.0, 0.0], [-100.0, 100.0, 0.0]];
        world.add_static_mesh(&vertices, vec![[0, 1, 2], [0, 2, 3]]).unwrap();
        let id = ball(&mut world);
        world.set_pose(id, [0.0, 0.0, 50.0].into(), [0.0, 0.0, 0.0, 1.0]);
        let mut background = Background::new(world);
        let mut impacts = Vec::new();
        for _ in 0..60 {
            background.start(1.0 / 60.0);
            impacts.extend(background.take_impacts());
        }
        assert!(impacts.iter().any(|i| i.body == id && i.other.is_none()), "{impacts:?}");
    }
}
