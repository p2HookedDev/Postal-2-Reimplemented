use super::*;

impl<'a, B: Build> Decoder<'_, '_, 'a, B> {
    #[expect(clippy::too_many_lines, reason = "one match over every expression kind; the link step grew it")]
    pub(super) fn token_expr(&mut self) -> Result<(u8, B::Node), Error> {
        let offset = self.r.pos();
        let token = self.byte()?;
        let node = match token {
            0x04..=0x0e => self.flow_expr(token, offset)?,
            0x1d..=0x24 | 0x2c | 0x34 => self.const_expr(token, offset)?,
            0x70..=0xff => self.call(Callee::Native(u16::from(token)))?,
            0x60..=0x6f => {
                let index = u16::from(token - 0x60) << 8 | u16::from(self.byte()?);
                self.call(Callee::Native(index))?
            }
            0x00 => {
                let index = self.object()?;
                self.link(|| Expr::LocalVariable(index))
            }
            0x01 => {
                let index = self.object()?;
                self.link(|| Expr::InstanceVariable(index))
            }
            0x02 => {
                let index = self.object()?;
                self.link(|| Expr::DefaultVariable(index))
            }
            0x0f => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::Let { target: B::boxed(target), value: B::boxed(value) })
            }
            0x10 => {
                let (index, array) = (self.child()?, self.child()?);
                B::node(|| Expr::DynArrayElement { index: B::boxed(index), array: B::boxed(array) })
            }
            0x11 => {
                let (outer, name, flags, class) = (self.child()?, self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::New { outer: B::boxed(outer), name: B::boxed(name), flags: B::boxed(flags), class: B::boxed(class) })
            }
            0x12 => {
                let (object, skip, size, member) = (self.child()?, self.word()?, self.byte()?, self.child()?);
                B::node(|| Expr::ClassContext { object: B::boxed(object), skip, size, member: B::boxed(member) })
            }
            0x13 => {
                let (class, value) = (self.object()?, self.child()?);
                self.link(|| Expr::MetaCast { class, value: B::boxed(value) })
            }
            0x14 => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::LetBool { target: B::boxed(target), value: B::boxed(value) })
            }
            END_FUNCTION_PARMS => B::node(|| Expr::EndFunctionParms),
            0x17 => B::node(|| Expr::SelfObject),
            0x18 => {
                let (size, value) = (self.word()?, self.child()?);
                B::node(|| Expr::Skip { size, value: B::boxed(value) })
            }
            0x19 => {
                let (object, skip, size, member) = (self.child()?, self.word()?, self.byte()?, self.child()?);
                B::node(|| Expr::Context { object: B::boxed(object), skip, size, member: B::boxed(member) })
            }
            0x1a => {
                let (index, array) = (self.child()?, self.child()?);
                B::node(|| Expr::ArrayElement { index: B::boxed(index), array: B::boxed(array) })
            }
            0x1b => {
                let name = self.name()?;
                self.call(Callee::Virtual(name))?
            }
            0x1c => {
                let function = self.object()?;
                self.call(Callee::Final(function))?
            }
            0x25 => B::node(|| Expr::IntZero),
            0x26 => B::node(|| Expr::IntOne),
            0x27 => B::node(|| Expr::True),
            0x28 => B::node(|| Expr::False),
            0x29 => {
                let property = self.object()?;
                B::node(|| Expr::NativeParm(property))
            }
            0x2a => B::node(|| Expr::NoObject),
            // A bool variable token runs the next expression (the variable).
            0x2d => {
                let variable = self.child()?;
                B::node(|| Expr::BoolVariable(B::boxed(variable)))
            }
            0x2e => {
                let (class, value) = (self.object()?, self.child()?);
                self.link(|| Expr::DynamicCast { class, value: B::boxed(value) })
            }
            0x2f => {
                let (iterator, end) = (self.child()?, self.word()?);
                B::node(|| Expr::Iterator { iterator: B::boxed(iterator), end })
            }
            0x30 => B::node(|| Expr::IteratorPop),
            0x31 => B::node(|| Expr::IteratorNext),
            0x32 => {
                let (structure, left, right) = (self.object()?, self.child()?, self.child()?);
                B::node(|| Expr::StructCmpEq { structure, left: B::boxed(left), right: B::boxed(right) })
            }
            0x33 => {
                let (structure, left, right) = (self.object()?, self.child()?, self.child()?);
                B::node(|| Expr::StructCmpNe { structure, left: B::boxed(left), right: B::boxed(right) })
            }
            0x36 => {
                let (member, value) = (self.object()?, self.child()?);
                self.link(|| Expr::StructMember { member, value: B::boxed(value) })
            }
            0x37 => {
                let array = self.child()?;
                B::node(|| Expr::DynArrayLength(B::boxed(array)))
            }
            0x38 => {
                let name = self.name()?;
                self.call(Callee::Global(name))?
            }
            0x39 => {
                let (kind, value) = (self.byte()?, self.child()?);
                B::node(|| Expr::PrimitiveCast { kind, value: B::boxed(value) })
            }
            0x40 => {
                let (array, index, count) = (self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::DynArrayInsert { array: B::boxed(array), index: B::boxed(index), count: B::boxed(count) })
            }
            0x41 => {
                let (array, index, count) = (self.child()?, self.child()?, self.child()?);
                B::node(|| Expr::DynArrayRemove { array: B::boxed(array), index: B::boxed(index), count: B::boxed(count) })
            }
            DEBUG_INFO => {
                let (version, line, position, opcode) = (self.int()?, self.int()?, self.int()?, self.c_string()?);
                B::node(|| Expr::DebugInfo { version, line, position, opcode: latin1(opcode) })
            }
            // A delegate call: property, name, then call parameters. The serializer reads these linearly and
            // attaches no debug info.
            0x43 => {
                let property = self.object()?;
                let name = self.name()?;
                self.call(Callee::Delegate { property, name })?
            }
            0x44 => {
                let name = self.name()?;
                B::node(|| Expr::DelegateProperty(name))
            }
            0x45 => {
                let (target, value) = (self.child()?, self.child()?);
                B::node(|| Expr::LetDelegate { target: B::boxed(target), value: B::boxed(value) })
            }
            _ => return Err(Error::BadToken { token, offset }),
        };
        Ok((token, node))
    }

    fn flow_expr(&mut self, token: u8, offset: usize) -> Result<B::Node, Error> {
        Ok(match token {
            0x04 => {
                let value = self.child()?;
                B::node(|| Expr::Return(B::boxed(value)))
            }
            0x05 => {
                let (size, value) = (self.byte()?, self.child()?);
                B::node(|| Expr::Switch { size, value: B::boxed(value) })
            }
            0x06 => {
                let target = self.word()?;
                B::node(|| Expr::Jump(target))
            }
            0x07 => {
                let (target, condition) = (self.word()?, self.child()?);
                B::node(|| Expr::JumpIfNot { target, condition: B::boxed(condition) })
            }
            0x08 => B::node(|| Expr::Stop),
            0x09 => {
                let (line, condition) = (self.word()?, self.child()?);
                B::node(|| Expr::Assert { line, condition: B::boxed(condition) })
            }
            0x0a => {
                let next = self.word()?;
                let value = if next == 0xffff { None } else { Some(self.child()?) };
                B::node(|| Expr::Case { next, value: value.map(B::boxed) })
            }
            0x0b => B::node(|| Expr::Nothing),
            0x0c => {
                let mut labels = Vec::new();
                loop {
                    let name = self.name()?;
                    let code = self.int()?;
                    if self.pkg.name(name).eq_ignore_ascii_case("None") {
                        break B::node(|| Expr::LabelTable(labels));
                    }
                    labels.push((name, code));
                }
            }
            0x0d => {
                let value = self.child()?;
                B::node(|| Expr::GotoLabel(B::boxed(value)))
            }
            0x0e => {
                let value = self.child()?;
                B::node(|| Expr::EatString(B::boxed(value)))
            }
            _ => return Err(Error::BadToken { token, offset }),
        })
    }

    fn const_expr(&mut self, token: u8, offset: usize) -> Result<B::Node, Error> {
        Ok(match token {
            0x1d => {
                let value = self.int()?;
                B::node(|| Expr::IntConst(value))
            }
            0x1e => {
                let value = self.float()?;
                B::node(|| Expr::FloatConst(value))
            }
            0x1f => {
                let text = self.c_string()?;
                B::node(|| Expr::StringConst(latin1(text)))
            }
            0x20 => {
                let object = self.object()?;
                B::node(|| Expr::ObjectConst(object))
            }
            0x21 => {
                let name = self.name()?;
                self.link(|| Expr::NameConst(name))
            }
            0x22 => {
                let value = [self.int()?, self.int()?, self.int()?];
                B::node(|| Expr::RotationConst(value))
            }
            0x23 => {
                let value = [self.float()?, self.float()?, self.float()?];
                B::node(|| Expr::VectorConst(value))
            }
            0x24 => {
                let value = self.byte()?;
                B::node(|| Expr::ByteConst(value))
            }
            0x2c => {
                let value = self.byte()?;
                B::node(|| Expr::IntConstByte(value))
            }
            // Words until one whose high byte is 0 (the engine tests the last byte read); a final 0 is no character.
            0x34 => {
                let start = self.r.pos();
                while self.word()? >> 8 != 0 {}
                let bytes = self.read_since(start)?;
                B::node(|| {
                    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&unit| u16::from_le_bytes(unit)).collect();
                    let text = match units.split_last() {
                        Some((&0, text)) => text,
                        _ => units.as_slice(),
                    };
                    Expr::UnicodeStringConst(String::from_utf16_lossy(text))
                })
            }
            _ => return Err(Error::BadToken { token, offset }),
        })
    }
}
