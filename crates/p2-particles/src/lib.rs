//! Particle emitters simulated as retail; the caller feeds properties through a closure and draws.
//!
//! ```
//! use p2_particles::{Emitter, EmitterProperties, EmitterSettings};
//! let props = EmitterProperties::from_lookup(|_name: &str| None);
//! let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
//! emitter.tick(0.5, p2_math::Vector::ZERO, p2_math::Rotator::ZERO);
//! assert!(emitter.sprites(0, p2_particles::View::default()).count() <= 10);
//! ```

mod fluid;
mod math;
mod props;
mod sim;

pub use props::{
    ColorKey, SizeKey,
    BeamEnd, BeamEndPoint, BeamProperties, BeamScale, CoordinateSystem, DirectionUsage, DrawStyle, EmitterKind, EmitterProperties, EmitterSettings, PropertyValue, Range,
    RangeVector, RotationSource, ShapeExtend, StartLocationShape, VelocityDirection,
};
pub use fluid::{FluidOscillator, FluidSettings, FluidSurface, FluidToucher, FluidVertex};
pub use p2_math::Rng;
pub use sim::{ActorForce, Beam, BeamPoint, Collide, CollisionHit, Emitter, MeshParticle, Owner, ParticleEmitter, SpriteParticle, StripPoint, View};
