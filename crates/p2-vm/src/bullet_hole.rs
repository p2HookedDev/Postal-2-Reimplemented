//! Bullet holes on doors and Karma props: our addition. Retail ammo spawns its hit pack, and with it the hole, only on static hits.

use p2_script::Expr;

use crate::interp::Function;
use crate::{ClassId, Error, LoaderRef, Prop, Value, Vm};

impl<L: LoaderRef> Vm<L> {
    /// `args`: `ProcessTraceHit`'s weapon, hit actor, hit location and hit normal.
    pub(crate) fn bullet_hole(&mut self, function: &Function, [weapon, other, location, normal]: [Value; 4]) -> Result<(), Error> {
        let (Value::Object(Some(other)), Value::Vector(location), Value::Vector(normal)) = (other, location, normal) else { return Ok(()) };
        if !self.moving_receiver(other) {
            return Ok(());
        }
        let Some(splat) = self.hole_splat(function)? else { return Ok(()) };
        let owner = match weapon {
            Value::Object(Some(weapon)) => self.store.object_of(weapon, Prop::Owner),
            _ => None,
        };
        let mut rotation = (-normal).rotation();
        rotation.roll = (self.random() * 65536.0) as i32;
        self.spawn(splat, owner, [Some(Value::Vector(location)), Some(Value::Rotator(rotation))])?;
        Ok(())
    }

    /// The splat class of the first `BulletHitPack` the function names.
    fn hole_splat(&mut self, function: &Function) -> Result<Option<ClassId>, Error> {
        if let Some(&splat) = self.script.lookups.hole_splats.get(&function.index) {
            return Ok(splat);
        }
        let mut references = Vec::new();
        for statement in &function.statements {
            statement.expr.visit(&mut |e| {
                if let Expr::ObjectConst(reference) = e {
                    references.push(*reference);
                }
            });
        }
        let mut splat = None;
        for reference in references {
            let Value::Object(Some(object)) = self.object_const(function.package, reference) else { continue };
            if self.store.object(object).class_name != self.script.lookups.class_names.class {
                continue;
            }
            let Some(class) = self.class_of_object(object)? else { continue };
            if !self.class_is_a(class, "BulletHitPack") {
                continue;
            }
            if let Some(Value::Object(Some(splat_object))) = self.store.default_value(class, "mysplatclass")
                && let Some(found) = self.class_of_object(splat_object)?
            {
                splat = Some(found);
                break;
            }
        }
        self.script.lookups.hole_splats.insert(function.index, splat);
        Ok(splat)
    }
}
