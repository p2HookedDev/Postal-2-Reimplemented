use p2_math::Vector;
use crate::interp::{float, int, truthy};
use crate::native_id::Native;
use crate::natives::{name_arg, orthogonal_rotation, rot3, vec3};
use crate::Value;

pub(crate) fn operate(native: Native, args: &mut [Value]) -> Option<Value> {
    scalar(native, args)
        .or_else(|| spatial(native, args))
        .or_else(|| assignment(native, args))
        .or_else(|| increment(native, args))
        .or_else(|| vector_out(native, args))
}

fn scalar(native: Native, args: &[Value]) -> Option<Value> {
    let i = |n: usize| args.get(n).map_or(0, int);
    let f = |n: usize| args.get(n).map_or(0.0, float);
    let b = |n: usize| args.get(n).is_some_and(truthy);
    let v = |n: usize| vec3(args.get(n));
    let object = |n: usize| match args.get(n) {
        Some(&Value::Object(object)) => object,
        _ => None,
    };
    let color = |n: usize| match args.get(n) {
        Some(Value::Struct(members)) => [0, 1, 2, 3].map(|i| members.get(i).map_or(0, int) as u8),
        _ => [0; 4],
    };
    let colour = |bytes: [u8; 4]| Value::Struct(bytes.map(Value::Byte).to_vec());
    Some(match native {
        Native::Not_PreBool => Value::Bool(!b(0)),
        Native::EqualEqual_BoolBool => Value::Bool(b(0) == b(1)),
        Native::NotEqual_BoolBool | Native::XorXor_BoolBool => Value::Bool(b(0) != b(1)),
        Native::AndAnd_BoolBool => Value::Bool(b(0) && b(1)),
        Native::OrOr_BoolBool => Value::Bool(b(0) || b(1)),
        Native::Subtract_PreInt => Value::Int(i(0).wrapping_neg()),
        Native::Complement_PreInt => Value::Int(!i(0)),
        Native::Add_IntInt => Value::Int(i(0).wrapping_add(i(1))),
        Native::Subtract_IntInt => Value::Int(i(0).wrapping_sub(i(1))),
        Native::Multiply_IntInt => Value::Int(i(0).wrapping_mul(i(1))),
        Native::Divide_IntInt => Value::Int(if i(1) == 0 { 0 } else { i(0).wrapping_div(i(1)) }),
        Native::LessLess_IntInt => Value::Int(i(0).wrapping_shl(i(1) as u32)),
        Native::GreaterGreater_IntInt => Value::Int(i(0).wrapping_shr(i(1) as u32)),
        Native::GreaterGreaterGreater_IntInt => Value::Int((i(0) as u32).wrapping_shr(i(1) as u32) as i32),
        Native::And_IntInt => Value::Int(i(0) & i(1)),
        Native::Or_IntInt => Value::Int(i(0) | i(1)),
        Native::Xor_IntInt => Value::Int(i(0) ^ i(1)),
        Native::Less_IntInt => Value::Bool(i(0) < i(1)),
        Native::Greater_IntInt => Value::Bool(i(0) > i(1)),
        Native::LessEqual_IntInt => Value::Bool(i(0) <= i(1)),
        Native::GreaterEqual_IntInt => Value::Bool(i(0) >= i(1)),
        Native::EqualEqual_IntInt => Value::Bool(i(0) == i(1)),
        Native::NotEqual_IntInt => Value::Bool(i(0) != i(1)),
        Native::Min => Value::Int(i(0).min(i(1))),
        Native::Max => Value::Int(i(0).max(i(1))),
        Native::Clamp => Value::Int(i(0).max(i(1)).min(i(2))),
        Native::ClockwiseFrom_IntInt => {
            let (a, b) = (i(0) & 0xffff, i(1) & 0xffff);
            Value::Bool(if (a - b).abs() > 32768 { a < b } else { a > b })
        }
        Native::Subtract_PreFloat => Value::Float(-f(0)),
        Native::Add_FloatFloat => Value::Float(f(0) + f(1)),
        Native::Subtract_FloatFloat => Value::Float(f(0) - f(1)),
        Native::Multiply_FloatFloat => Value::Float(f(0) * f(1)),
        Native::Divide_FloatFloat => Value::Float(f(0) / f(1)),
        Native::Percent_FloatFloat => Value::Float(f(0) % f(1)),
        Native::MultiplyMultiply_FloatFloat => Value::Float(f(0).powf(f(1))),
        Native::Less_FloatFloat => Value::Bool(f(0) < f(1)),
        Native::Greater_FloatFloat => Value::Bool(f(0) > f(1)),
        Native::LessEqual_FloatFloat => Value::Bool(f(0) <= f(1)),
        Native::GreaterEqual_FloatFloat => Value::Bool(f(0) >= f(1)),
        Native::EqualEqual_FloatFloat => Value::Bool(f(0) == f(1)),
        Native::NotEqual_FloatFloat => Value::Bool(f(0) != f(1)),
        Native::ComplementEqual_FloatFloat => Value::Bool((f(0) - f(1)).abs() < 1e-4),
        Native::Abs => Value::Float(f(0).abs()),
        Native::Sqrt => Value::Float(f(0).sqrt()),
        Native::Square => Value::Float(f(0) * f(0)),
        Native::Sin => Value::Float(f(0).sin()),
        Native::Cos => Value::Float(f(0).cos()),
        Native::Tan => Value::Float(f(0).tan()),
        Native::Atan => Value::Float(f(0).atan()),
        Native::Asin => Value::Float(f(0).asin()),
        Native::Acos => Value::Float(f(0).acos()),
        Native::Atan2 => Value::Float(f(0).atan2(f(1))),
        Native::Exp => Value::Float(f(0).exp()),
        Native::Loge => Value::Float(f(0).ln()),
        Native::Log10 => Value::Float(f(0).log10()),
        Native::Ceil => Value::Float(f(0).ceil()),
        Native::FMin => Value::Float(f(0).min(f(1))),
        Native::FMax => Value::Float(f(0).max(f(1))),
        Native::FClamp => Value::Float(f(0).max(f(1)).min(f(2))),
        Native::Lerp => Value::Float(f(1) + (f(2) - f(1)) * f(0)),
        Native::Smerp => {
            let alpha = f(0);
            Value::Float(f(1) + (3.0 * alpha * alpha - 2.0 * alpha * alpha * alpha) * (f(2) - f(1)))
        }
        Native::FloatSpringInterp => Value::Float(spring(f(0), f(1), f(2), f(3))),
        Native::VectorSpringInterp => {
            let (dest, current) = (v(0), v(1));
            Value::Vector([0, 1, 2].map(|i| spring(dest[i], current[i], f(2), f(3))).into())
        }
        Native::EqualEqual_ObjectObject => Value::Bool(object(0) == object(1)),
        Native::NotEqual_ObjectObject => Value::Bool(object(0) != object(1)),
        Native::EqualEqual_NameName => Value::Bool(name_arg(args, 0) == name_arg(args, 1)),
        Native::NotEqual_NameName => Value::Bool(name_arg(args, 0) != name_arg(args, 1)),
        // Per byte member: sums and differences wrap; products truncate, out of range saturates (unverified).
        Native::Subtract_ColorColor => {
            let (a, c) = (color(0), color(1));
            colour([0, 1, 2, 3].map(|k| a[k].wrapping_sub(c[k])))
        }
        Native::Add_ColorColor => {
            let (a, c) = (color(0), color(1));
            colour([0, 1, 2, 3].map(|k| a[k].wrapping_add(c[k])))
        }
        Native::Multiply_FloatColor => {
            let scale = f(0);
            colour(color(1).map(|c| (f32::from(c) * scale) as u8))
        }
        Native::Multiply_ColorFloat => {
            let scale = f(1);
            colour(color(0).map(|c| (f32::from(c) * scale) as u8))
        }
        _ => return None,
    })
}

fn spatial(native: Native, args: &[Value]) -> Option<Value> {
    let f = |n: usize| args.get(n).map_or(0.0, float);
    let v = |n: usize| vec3(args.get(n));
    let r = |n: usize| rot3(args.get(n));
    Some(match native {
        Native::Subtract_PreVector => Value::Vector(-v(0)),
        Native::Add_VectorVector => Value::Vector(v(0) + v(1)),
        Native::Subtract_VectorVector => Value::Vector(v(0) - v(1)),
        Native::Multiply_VectorVector => Value::Vector(v(0) * v(1)),
        Native::Multiply_VectorFloat => Value::Vector(v(0) * f(1)),
        Native::Multiply_FloatVector => Value::Vector(v(1) * f(0)),
        Native::Divide_VectorFloat => Value::Vector(v(0) / f(1)),
        Native::Dot_VectorVector => Value::Float(v(0).dot(v(1))),
        Native::Cross_VectorVector => Value::Vector(v(0).cross(v(1))),
        Native::EqualEqual_VectorVector => Value::Bool(v(0) == v(1)),
        Native::NotEqual_VectorVector => Value::Bool(v(0) != v(1)),
        Native::VSize => Value::Float(v(0).size()),
        Native::Normal => {
            let value = v(0);
            let length = value.size();
            Value::Vector(if length > 0.0 { value / length } else { Vector::ZERO })
        }
        Native::VLerp => {
            let (a, b, alpha) = (v(0), v(1), f(2));
            Value::Vector([0, 1, 2].map(|i| a[i] + alpha * (b[i] - a[i])).into())
        }
        Native::MirrorVectorByNormal => {
            let (a, n) = (v(0), v(1));
            let d = 2.0 * a.dot(n);
            Value::Vector([0, 1, 2].map(|i| a[i] - n[i] * d).into())
        }
        Native::GreaterGreater_VectorRotator => Value::Vector(r(1).rotate(v(0))),
        Native::LessLess_VectorRotator => Value::Vector(r(1).axes().map(|axis| axis.dot(v(0))).into()),
        Native::Normalize => Value::Rotator(r(0).map(|c| {
            let c = c & 0xffff;
            if c > 32767 { c - 0x10000 } else { c }
        })),
        Native::RLerp => {
            let (a, b, alpha) = (r(0), r(1), f(2));
            Value::Rotator([0, 1, 2].map(|i| a[i] + (alpha * (b[i] - a[i]) as f32) as i32).into())
        }
        Native::OrthoRotation => Value::Rotator(orthogonal_rotation(v(0), v(1), v(2))),
        Native::Add_RotatorRotator => {
            let (a, b) = (r(0), r(1));
            Value::Rotator([0, 1, 2].map(|i| a[i].wrapping_add(b[i])).into())
        }
        Native::Subtract_RotatorRotator => {
            let (a, b) = (r(0), r(1));
            Value::Rotator([0, 1, 2].map(|i| a[i].wrapping_sub(b[i])).into())
        }
        Native::Multiply_RotatorFloat => Value::Rotator(r(0).map(|c| (c as f32 * f(1)) as i32)),
        Native::Multiply_FloatRotator => Value::Rotator(r(1).map(|c| (c as f32 * f(0)) as i32)),
        Native::Divide_RotatorFloat => Value::Rotator(r(0).map(|c| (c as f32 / f(1)) as i32)),
        Native::EqualEqual_RotatorRotator => Value::Bool(r(0) == r(1)),
        Native::NotEqual_RotatorRotator => Value::Bool(r(0) != r(1)),
        _ => return None,
    })
}

fn assignment(native: Native, args: &mut [Value]) -> Option<Value> {
    let [left, right, ..] = args else { return None };
    let byte = |v: &Value| int(v) as u8;
    let (v, r) = (|v: &Value| vec3(Some(v)), |v: &Value| rot3(Some(v)));
    let result = match native {
        // Bytes wrap like C `BYTE`s; `/=` by 0 gives 0.
        Native::AddEqual_ByteByte => Value::Byte(byte(left).wrapping_add(byte(right))),
        Native::SubtractEqual_ByteByte => Value::Byte(byte(left).wrapping_sub(byte(right))),
        Native::MultiplyEqual_ByteByte => Value::Byte(byte(left).wrapping_mul(byte(right))),
        Native::DivideEqual_ByteByte => Value::Byte(byte(left).checked_div(byte(right)).unwrap_or(0)),
        Native::MultiplyEqual_VectorFloat => Value::Vector(v(left) * float(right)),
        Native::MultiplyEqual_VectorVector => Value::Vector(v(left) * v(right)),
        Native::DivideEqual_VectorFloat => Value::Vector(v(left) * (1.0 / float(right))),
        Native::AddEqual_VectorVector => Value::Vector(v(left) + v(right)),
        Native::SubtractEqual_VectorVector => Value::Vector(v(left) - v(right)),
        Native::MultiplyEqual_RotatorFloat => Value::Rotator(r(left).map(|c| (c as f32 * float(right)) as i32)),
        Native::DivideEqual_RotatorFloat => Value::Rotator(r(left).map(|c| (c as f32 / float(right)) as i32)),
        Native::AddEqual_RotatorRotator => {
            let (a, b) = (r(left), r(right));
            Value::Rotator([0, 1, 2].map(|i| a[i].wrapping_add(b[i])).into())
        }
        Native::SubtractEqual_RotatorRotator => {
            let (a, b) = (r(left), r(right));
            Value::Rotator([0, 1, 2].map(|i| a[i].wrapping_sub(b[i])).into())
        }
        Native::AddEqual_IntInt => Value::Int(int(left).wrapping_add(int(right))),
        Native::SubtractEqual_IntInt => Value::Int(int(left).wrapping_sub(int(right))),
        Native::MultiplyEqual_IntFloat => Value::Int((int(left) as f32 * float(right)) as i32),
        Native::DivideEqual_IntFloat => Value::Int((int(left) as f32 / float(right)) as i32),
        Native::AddEqual_FloatFloat => Value::Float(float(left) + float(right)),
        Native::SubtractEqual_FloatFloat => Value::Float(float(left) - float(right)),
        Native::MultiplyEqual_FloatFloat => Value::Float(float(left) * float(right)),
        Native::DivideEqual_FloatFloat => Value::Float(float(left) / float(right)),
        Native::ConcatEqual_StrStr | Native::AtEqual_StrStr => {
            let (Value::Str(a), Value::Str(b)) = (&*left, &*right) else { return None };
            let separator = if native == Native::AtEqual_StrStr { " " } else { "" };
            Value::Str(format!("{a}{separator}{b}"))
        }
        _ => return None,
    };
    *left = result.clone();
    Some(result)
}

fn increment(native: Native, args: &mut [Value]) -> Option<Value> {
    let (step, prefix) = match native {
        Native::AddAdd_PreInt | Native::AddAdd_PreByte => (1, true),
        Native::AddAdd_Int | Native::AddAdd_Byte => (1, false),
        Native::SubtractSubtract_PreInt | Native::SubtractSubtract_PreByte => (-1, true),
        Native::SubtractSubtract_Int | Native::SubtractSubtract_Byte => (-1, false),
        _ => return None,
    };
    let slot = args.first_mut()?;
    let old = slot.clone();
    let new = match old {
        Value::Byte(v) => Value::Byte(v.wrapping_add_signed(step as i8)),
        ref other => Value::Int(int(other).wrapping_add(step)),
    };
    *slot = new.clone();
    Some(if prefix { new } else { old })
}

fn vector_out(native: Native, args: &mut [Value]) -> Option<Value> {
    match native {
        Native::GetAxes if args.len() >= 4 => {
            let [x, y, z] = rot3(args.first()).axes();
            args[1] = Value::Vector(x);
            args[2] = Value::Vector(y);
            args[3] = Value::Vector(z);
        }
        Native::GetUnAxes if args.len() >= 4 => {
            let m = rot3(args.first()).axes();
            for (k, slot) in args.iter_mut().skip(1).take(3).enumerate() {
                *slot = Value::Vector([m[0][k], m[1][k], m[2][k]].into());
            }
        }
        Native::Invert if args.len() >= 3 => {
            let m = [vec3(args.first()), vec3(args.get(1)), vec3(args.get(2))];
            for (k, slot) in args.iter_mut().take(3).enumerate() {
                *slot = Value::Vector([m[0][k], m[1][k], m[2][k]].into());
            }
        }
        _ => return None,
    }
    Some(Value::Int(0))
}

fn spring(dest: f32, current: f32, delta: f32, constant: f32) -> f32 {
    let constant = if constant == 0.0 { 2048.0 } else { constant };
    let next = (dest - current) * constant * delta * delta + current;
    let past = if current <= dest { dest < next } else { next <= dest };
    if past { dest } else { next }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_and_increment_write_their_first_argument() {
        let mut args = [Value::Int(5), Value::Int(3)];
        assert_eq!(operate(Native::AddEqual_IntInt, &mut args), Some(Value::Int(8)));
        assert_eq!(args[0], Value::Int(8));
        let mut args = [Value::Byte(255)];
        assert_eq!(operate(Native::AddAdd_Byte, &mut args), Some(Value::Byte(255)));
        assert_eq!(args[0], Value::Byte(0));
    }

    #[test]
    fn vector_operators() {
        let mut args = [Value::Vector(Vector::new(3.0, 4.0, 0.0)), Value::Float(2.0)];
        assert_eq!(operate(Native::VSize, &mut args), Some(Value::Float(5.0)));
        assert_eq!(operate(Native::Multiply_VectorFloat, &mut args), Some(Value::Vector(Vector::new(6.0, 8.0, 0.0))));
    }

    #[test]
    fn other_natives_are_not_operators() {
        assert_eq!(operate(Native::Spawn, &mut []), None);
        assert_eq!(operate(Native::GetAxes, &mut [Value::Int(0)]), None);
    }
}
