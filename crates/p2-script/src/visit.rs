use crate::Expr;

impl Expr {
    /// Pre-order.
    pub fn visit(&self, f: &mut impl FnMut(&Self)) {
        f(self);
        match self {
            Self::Return(e)
            | Self::Switch { value: e, .. }
            | Self::JumpIfNot { condition: e, .. }
            | Self::Assert { condition: e, .. }
            | Self::GotoLabel(e)
            | Self::EatString(e)
            | Self::MetaCast { value: e, .. }
            | Self::Skip { value: e, .. }
            | Self::BoolVariable(e)
            | Self::DynamicCast { value: e, .. }
            | Self::Iterator { iterator: e, .. }
            | Self::StructMember { value: e, .. }
            | Self::DynArrayLength(e)
            | Self::PrimitiveCast { value: e, .. }
            | Self::Linked { expr: e, .. }
            | Self::Case { value: Some(e), .. } => e.visit(f),
            Self::Let { target: a, value: b }
            | Self::LetBool { target: a, value: b }
            | Self::LetDelegate { target: a, value: b }
            | Self::DynArrayElement { index: a, array: b }
            | Self::ArrayElement { index: a, array: b }
            | Self::StructCmpEq { left: a, right: b, .. }
            | Self::StructCmpNe { left: a, right: b, .. }
            | Self::ClassContext { object: a, member: b, .. }
            | Self::Context { object: a, member: b, .. } => {
                a.visit(f);
                b.visit(f);
            }
            Self::DynArrayInsert { array, index, count } | Self::DynArrayRemove { array, index, count } => {
                for e in [array, index, count] {
                    e.visit(f);
                }
            }
            Self::New { outer, name, flags, class } => {
                for e in [outer, name, flags, class] {
                    e.visit(f);
                }
            }
            Self::Call { args, .. } => {
                for e in args {
                    e.visit(f);
                }
            }
            Self::LocalVariable(_)
            | Self::InstanceVariable(_)
            | Self::DefaultVariable(_)
            | Self::Jump(_)
            | Self::Stop
            | Self::Case { value: None, .. }
            | Self::Nothing
            | Self::LabelTable(_)
            | Self::EndFunctionParms
            | Self::SelfObject
            | Self::IntConst(_)
            | Self::FloatConst(_)
            | Self::StringConst(_)
            | Self::ObjectConst(_)
            | Self::NameConst(_)
            | Self::RotationConst(_)
            | Self::VectorConst(_)
            | Self::ByteConst(_)
            | Self::IntZero
            | Self::IntOne
            | Self::True
            | Self::False
            | Self::NativeParm(_)
            | Self::NoObject
            | Self::IntConstByte(_)
            | Self::IteratorPop
            | Self::IteratorNext
            | Self::UnicodeStringConst(_)
            | Self::DebugInfo { .. }
            | Self::DelegateProperty(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Callee, Expr};

    #[test]
    fn finds_nested_object_consts() {
        let spawn = Expr::Call { callee: Callee::Native(0), args: vec![Expr::ObjectConst(7), Expr::Nothing] };
        let tree = Expr::JumpIfNot { target: 0, condition: Box::new(Expr::Linked { link: 0, expr: Box::new(spawn) }) };
        let mut found = Vec::new();
        tree.visit(&mut |e| {
            if let Expr::ObjectConst(r) = e {
                found.push(*r);
            }
        });
        assert_eq!(found, [7]);
    }
}
