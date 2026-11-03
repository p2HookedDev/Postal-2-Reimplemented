use p2_math::{Rotator, Vector};
use crate::interp::{float, int, truthy};
use crate::native_id::Native;
use crate::{Error, HostCommand, LoaderRef, MusicCommand, NameId, ObjectId, PlayedSound, Prop, ProjectorEvent, Value, Vm};

pub const AUDIO_DEFAULT_RADIUS: f32 = 80.0;

#[expect(clippy::too_many_lines, reason = "one match over the native enum")]
pub(crate) fn call<L: LoaderRef>(vm: &mut Vm<L>, name: &str, native: Option<Native>, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
    let Some(native) = native else { return Err(Error::UnknownNative(name.to_owned())) };
    if let Some(value) = crate::operators::operate(native, args) {
        return Ok(Some(value));
    }
    let i = |n: usize| args.get(n).map_or(0, int);
    let f = |n: usize| args.get(n).map_or(0.0, float);
    let b = |n: usize| args.get(n).is_some_and(truthy);
    let s = |n: usize| match args.get(n) {
        Some(Value::Str(text)) => text.as_str(),
        _ => "",
    };
    let object = |n: usize| object_arg(args, n);
    let value = match native {
        // Draws yaw first, then pitch, then roll when asked, as retail.
        Native::RotRand => {
            let mut turn = || (vm.rand() * 2) % 0xffff;
            let (yaw, pitch) = (turn(), turn());
            Value::Rotator([pitch, yaw, if b(0) { turn() } else { 0 }].into())
        }
        Native::GetEnum => Value::Name(match object(0) {
            Some(e) => vm.enum_name(e, i(1)),
            None => NameId::NONE,
        }),
        Native::GetPropertyText => Value::Str(this.map_or_else(String::new, |this| vm.property_text(this, s(0)))),
        Native::SetPropertyText => {
            if let Some(this) = this {
                vm.set_property_text(this, s(0), s(1));
            }
            return Ok(None);
        }
        Native::SplitString => {
            let (text, delim) = (s(0), s(1));
            let pieces: Vec<Value> = if delim.is_empty() { vec![Value::Str(text.to_owned())] } else { text.split(delim).map(|p| Value::Str(p.to_owned())).collect() };
            let count = pieces.len() as i32;
            if let Some(slot) = args.get_mut(2) {
                *slot = Value::Array(pieces);
            }
            Value::Int(count)
        }
        Native::SplitStringSingle => {
            let (text, delim) = (s(0), s(1));
            match (!delim.is_empty()).then(|| text.split_once(delim)).flatten() {
                Some((head, rest)) => {
                    let (head, rest) = (head.to_owned(), rest.to_owned());
                    args[0] = Value::Str(rest);
                    if let Some(slot) = args.get_mut(2) {
                        *slot = Value::Str(head);
                    }
                    Value::Bool(true)
                }
                None => Value::Bool(false),
            }
        }
        Native::FindObject => {
            let class = object(1).map(|class| vm.name(vm.object(class).name).to_owned()).unwrap_or_default();
            Value::Object(vm.find_object_by_path(s(0), &class))
        }
        Native::FRand => Value::Float(vm.random()),
        // `rand() % Max`, so never above 32767; 0 for Max <= 0.
        Native::Rand => Value::Int(if i(0) > 0 { vm.rand() % i(0) } else { 0 }),
        // Size squared summed Y, X, Z, with the square root in double precision, as retail.
        Native::VRand => loop {
            let v = Vector::from([0; 3].map(|_| vm.random() * 2.0 - 1.0));
            let size_squared = v.y * v.y + v.x * v.x + v.z * v.z;
            if size_squared <= 1.0 {
                break Value::Vector(v * (1.0 / (f64::from(size_squared).sqrt() as f32)));
            }
        },
        Native::Concat_StrStr => Value::Str([s(0), s(1)].concat()),
        Native::At_StrStr => Value::Str(format!("{} {}", s(0), s(1))),
        Native::EqualEqual_StrStr => Value::Bool(s(0) == s(1)),
        Native::NotEqual_StrStr => Value::Bool(s(0) != s(1)),
        Native::Less_StrStr => Value::Bool(s(0) < s(1)),
        Native::Greater_StrStr => Value::Bool(s(0) > s(1)),
        Native::LessEqual_StrStr => Value::Bool(s(0) <= s(1)),
        Native::GreaterEqual_StrStr => Value::Bool(s(0) >= s(1)),
        Native::ComplementEqual_StrStr => Value::Bool(s(0).eq_ignore_ascii_case(s(1))),
        Native::Len => Value::Int(s(0).chars().count() as i32),
        Native::Caps => Value::Str(s(0).to_uppercase()),
        Native::Locs => Value::Str(s(0).to_lowercase()),
        Native::Left => Value::Str(s(0).chars().take(i(1).max(0) as usize).collect()),
        Native::Right => {
            let text = s(0);
            let count = text.chars().count();
            Value::Str(text.chars().skip(count.saturating_sub(i(1).max(0) as usize)).collect())
        }
        Native::Mid => {
            let count = if args.len() > 2 && i(2) > 0 { i(2) as usize } else { usize::MAX };
            Value::Str(s(0).chars().skip(i(1).max(0) as usize).take(count).collect())
        }
        Native::Asc => Value::Int(s(0).chars().next().map_or(0, |c| c as i32)),
        Native::Chr => Value::Str(char::from_u32(i(0).max(0) as u32).map(String::from).unwrap_or_default()),
        Native::InStr => Value::Int(s(0).find(s(1)).map_or(-1, |at| s(0)[..at].chars().count() as i32)),
        Native::GotoState => {
            if let Some(this) = this {
                // An omitted state is the current one (`GotoState(, '')`): the code goes to the label in that state.
                let state = if matches!(args.first(), Some(Value::Map)) { vm.state_name(this) } else { name_arg(&args[..], 0) };
                vm.goto_state(this, state, name_arg(&args[..], 1))?;
            }
            return Ok(None);
        }
        Native::GetStateName => Value::Name(this.map_or(NameId::NONE, |this| vm.state_name(this))),
        Native::IsInState => Value::Bool(match this {
            Some(this) => vm.is_in_state(this, name_arg(&args[..], 0))?,
            None => false,
        }),
        Native::Sleep => {
            vm.script.latent.sleep = Some(f(0));
            return Ok(None);
        }
        Native::FinishInterpolation => {
            if let Some(this) = this {
                vm.finish_interpolation(this);
            }
            return Ok(None);
        }
        Native::SetTimer => {
            if let Some(this) = this {
                vm.set(this, Prop::TimerRate, Value::Float(f(0)));
                vm.set(this, Prop::TimerCounter, Value::Float(0.0));
                vm.set(this, Prop::bTimerLoop, Value::Bool(b(1)));
            }
            return Ok(None);
        }
        Native::Destroy => Value::Bool(match this {
            Some(this) => vm.destroy(this)?,
            None => false,
        }),
        Native::Spawn => {
            let class = match args.first() {
                Some(&Value::Object(Some(class))) => vm.class_of_object(class)?,
                _ => None,
            };
            let Some(class) = class else { return Ok(Some(Value::Object(None))) };
            let owner = match args.get(1) {
                Some(&Value::Object(owner)) => owner,
                _ => None,
            };
            let placement = [(3, Prop::Location), (4, Prop::Rotation)].map(|(index, property)| match args.get(index) {
                Some(value) if is_struct_value(value) => Some(value.clone()),
                _ => this.and_then(|this| vm.get(this, property)),
            });
            let instigator = this.and_then(|this| vm.object_of(this, Prop::Instigator));
            let actor = vm.spawn_actor(class, owner, instigator, placement)?;
            if let (Some(actor), Some(tag @ Value::Name(_))) = (actor, args.get(2)) {
                vm.set(actor, Prop::Tag, tag.clone());
            }
            Value::Object(actor)
        }
        Native::DynamicLoadObject => {
            let path = match s(0).to_owned() {
                name if name.len() > 4 && name.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("ini:")) => vm.ini_name(&name[4..]).unwrap_or(name),
                name => name,
            };
            let class_name = match args.get(1) {
                Some(&Value::Object(Some(class))) => vm.name(vm.object(class).name).to_owned(),
                _ => String::new(),
            };
            if class_name.eq_ignore_ascii_case("Class") {
                let path = match path.contains('.') {
                    true => path,
                    false => vm.class_package(&path).map_or(path.clone(), |package| format!("{package}.{path}")),
                };
                let stub = vm.stub(&path, "Class");
                match vm.class_of_object(stub)? {
                    Some(_) => Value::Object(Some(stub)),
                    None => {
                        vm.diagnostics.log.push(format!("DynamicLoadObject: class {path} not found"));
                        Value::Object(None)
                    }
                }
            } else if path.contains('.') {
                // An object of a loaded package exists or not (then None, like a failed load); one in a package not
                // loaded is assumed to exist, as the requested class.
                match vm.export_class(&path, &class_name) {
                    Some(Some(class)) => Value::Object(Some(vm.stub(&path, &class))),
                    Some(None) => {
                        if !b(2) {
                            vm.diagnostics.log.push(format!("DynamicLoadObject: {path} not found"));
                        }
                        Value::Object(None)
                    }
                    None => Value::Object(Some(vm.stub(&path, &class_name))),
                }
            } else {
                Value::Object(None)
            }
        }
        Native::SetLocation => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            if matches!(args.first(), Some(Value::Map)) {
                Value::Bool(true)
            } else {
                Value::Bool(vm.far_move_actor(this, vec3(args.first()), matches!(args.get(1), Some(Value::Bool(true))))?)
            }
        }
        Native::Move => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            vm.move_actor(this, vec3(args.first()))?;
            Value::Bool(true)
        }
        Native::SetBase => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            let base = match args.first() {
                Some(&Value::Object(base)) => base,
                _ => None,
            };
            let floor = match args.get(1) {
                Some(value) if is_struct_value(value) => vec3(Some(value)),
                _ => [0.0, 0.0, 1.0].into(),
            };
            vm.set_base(this, base, floor)?;
            Value::Bool(true)
        }
        Native::SetOwner => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            let owner = match args.first() {
                Some(&Value::Object(owner)) => owner,
                _ => None,
            };
            vm.set_owner(this, owner)?;
            Value::Bool(true)
        }
        Native::SetCollision | Native::SetCollisionSize | Native::SetDrawScale | Native::SetDrawScale3D | Native::SetStaticMesh | Native::SetDrawType
        | Native::SetRotation | Native::SetRelativeLocation | Native::SetRelativeRotation | Native::LinkMesh => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            set_actor_properties(vm, native, this, args)?;
            Value::Bool(true)
        }
        Native::IsA => {
            let class = name_arg(&args[..], 0);
            Value::Bool(this.is_some_and(|this| vm.is_a(this, class)))
        }
        Native::ClassIsChildOf => {
            let class = |vm: &mut Vm<L>, n: usize| match args.get(n) {
                Some(&Value::Object(Some(object))) => vm.class_of_object(object),
                _ => Ok(None),
            };
            let (test, parent) = (class(vm, 0)?, class(vm, 1)?);
            Value::Bool(match (test, parent) {
                (Some(test), Some(parent)) => vm.class_is_a(test, vm.class(parent).name),
                _ => false,
            })
        }
        Native::AttachToBone | Native::DetachFromBone => {
            let (Some(this), Some(&Value::Object(Some(attachment)))) = (this, args.first()) else {
                return Ok(Some(Value::Bool(false)));
            };
            // The bone first: `SetBase` keeps the offset only of an actor that is not on a bone.
            if native == Native::AttachToBone {
                // Only attaches to a bone or alias of a skeletal mesh; a loose bolt-on is destroyed a second later.
                // A mesh whose package is not loaded has its bones unknown, not missing, so only a read mesh refuses.
                let bone = name_arg(&args[..], 1);
                let wanted = vm.name(bone).to_ascii_lowercase();
                let matched = vm.skeletal_mesh(this).is_some_and(|mesh| {
                    vm.posed_mesh(mesh).is_none_or(|mesh| mesh.match_ref_bone(&wanted).is_some())
                });
                if !matched || vm.is_deleted(attachment) {
                    return Ok(Some(Value::Bool(false)));
                }
                vm.set(attachment, Prop::AttachmentBone, Value::Name(bone));
                vm.set_base(attachment, Some(this), [0.0, 0.0, 1.0].into())?;
            } else if vm.object_of(attachment, Prop::Base) == Some(this) {
                vm.set_base(attachment, None, [0.0, 0.0, 1.0].into())?;
            }
            Value::Bool(true)
        }
        Native::AddController | Native::RemoveController => {
            controller_list(vm, native, this);
            return Ok(None);
        }
        Native::PlaySound | Native::PlayOwnedSound => {
            play_sound(vm, this, args);
            return Ok(None);
        }
        Native::Disable | Native::Enable => {
            let (Some(this), Some(&Value::Name(probe))) = (this, args.first()) else { return Ok(None) };
            let Some(bit) = crate::states::probe_bit(vm.name(probe)) else { return Ok(None) };
            let disabled = vm.script.states.probes_disabled.entry(this).or_insert(0);
            if native == Native::Disable {
                *disabled |= 1 << bit;
            } else {
                *disabled &= !(1 << bit);
            }
            return Ok(None);
        }
        Native::ConsoleCommand => console_command(vm, s(0))?,
        Native::IsDemoBuild | Native::PlatformIsSteamDeck | Native::PlatformIsMacOS | Native::PlatformIsUnix | Native::IsSteamBuild | Native::SteamOwnsDLC => Value::Bool(false),
        Native::PlatformIsWindows => Value::Bool(true),
        Native::SteamGetWorkshopStatus | Native::GetBuildDate => Value::Str(String::new()),
        // The install's country, `[Shell.ShellRootWindow] InstalledCountry` (unverified).
        Native::GetCountryCode => Value::Str(vm.ini_name("Shell.ShellRootWindow.InstalledCountry").unwrap_or_else(|| "US".to_owned())),
        Native::SaveConfig | Native::StaticSaveConfig => {
            if let Some(this) = this {
                vm.save_config(this, native == Native::StaticSaveConfig);
            }
            return Ok(None);
        }
        Native::ResetConfig | Native::AbandonProjector => return Ok(None),
        Native::PlayMusicAttenuate | Native::PlayMusic => {
            play_music(vm, native, this, args)
        }
        Native::AttachProjector | Native::DetachProjector => {
            if let Some(this) = this {
                if native == Native::AttachProjector {
                    vm.attach_static_meshes(this);
                } else {
                    vm.detach_projected_actors(this);
                }
                vm.host.commands.push(HostCommand::Projector(if native == Native::AttachProjector { ProjectorEvent::Attach(this) } else { ProjectorEvent::Detach(this) }));
            }
            return Ok(None);
        }
        Native::StopAllMusic => {
            vm.host.commands.push(HostCommand::Music(MusicCommand::StopAll { fade_out: f(0) }));
            return Ok(None);
        }
        Native::FileSize => {
            let path = s(0).replace('\\', "/");
            let stem = std::path::Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            let size = vm.loader.find(&stem).and_then(|file| std::fs::metadata(file).ok()).map_or(-1, |m| m.len() as i32);
            Value::Int(size)
        }
        Native::OnlyAffectPawns => {
            if let Some(this) = this {
                vm.set(this, Prop::bOnlyAffectPawns, Value::Bool(b(0)));
            }
            return Ok(None);
        }
        // The VM has no `Entry` map, so the played level stands in (unverified).
        Native::GetEntryLevel => Value::Object(vm.level()),
        Native::GetLocalURL => Value::Str(vm.map_name.clone()),
        Native::StopMusic => {
            vm.host.commands.push(HostCommand::Music(MusicCommand::Stop { handle: i(0), fade_out: f(1) }));
            return Ok(None);
        }
        Native::StrLen | Native::TextSize | Native::DrawText | Native::DrawTextClipped | Native::DrawTile | Native::DrawTileClipped | Native::DrawActor | Native::DrawPortal
        | Native::Draw3DLine => return vm.canvas_native(native, this, args),
        // Stores `ViewTarget`. Retail then calls a virtual function, not analysed; `None` is taken to mean
        // the controller itself (unverified).
        Native::SetViewTarget => {
            if let Some(this) = this {
                let target = object(0).or(Some(this));
                vm.set(this, Prop::ViewTarget, Value::Object(target));
            }
            return Ok(None);
        }
        Native::Trace | Native::FastTrace => return vm.trace_native(native, this, args),
        Native::GetRenderBoundingSphere => match this {
            Some(this) => vm.render_bounding_sphere(this),
            None => return Ok(None),
        },
        Native::GetBoneCoords => match this {
            Some(this) => vm.get_bone_coords(this, name_arg(&args[..], 0)),
            None => return Ok(None),
        },
        Native::GetBoneRotation => match this {
            Some(this) => vm.get_bone_rotation(this, name_arg(&args[..], 0)),
            None => return Ok(None),
        },
        Native::SetTwistLook => {
            if let Some(this) = this {
                vm.set_twist_look(this, i(1));
            }
            return Ok(None);
        }
        // Ignores its arguments, as retail. System time: days since 2000-01-01 (X), seconds into the day (Y)
        // and milliseconds (Z).
        Native::GetMillisecondsFrom => Value::Vector(milliseconds_since_2000(vm, this).into()),
        Native::Initialize if this.is_some_and(|this| vm.is_a(this, "Interaction")) => {
            if let Some(this) = this {
                vm.event(this, "Initialized", Vec::new())?;
            }
            return Ok(None);
        }
        Native::MakeNoise => {
            if let Some(this) = this {
                vm.make_noise(this, f(0))?;
            }
            return Ok(None);
        }
        Native::LineOfSightTo => Value::Bool(match (this, object(0)) {
            (Some(this), Some(other)) => vm.line_of_sight_to(this, other, false),
            _ => false,
        }),
        Native::PlayerCanSeeMe => Value::Bool(this.is_some_and(|this| vm.player_can_see_me(this))),
        Native::Warp | Native::UnWarp => {
            if let Some(this) = this
                && args.len() >= 3
            {
                let coords = crate::volumes::coords_of(vm.get(this, "WarpCoords").as_ref());
                let carry = if native == Native::Warp { crate::volumes::warp } else { crate::volumes::unwarp };
                let (location, velocity, rotation) = carry(coords, vec3(args.first()), vec3(args.get(1)), rot3(args.get(2)));
                args[0] = Value::Vector(location);
                args[1] = Value::Vector(velocity);
                args[2] = Value::Rotator(rotation);
            }
            return Ok(None);
        }
        Native::Encompasses => Value::Bool(match (this, object(0)) {
            (Some(this), Some(other)) => {
                let location = vm.vec3_of(other, Prop::Location);
                vm.encompasses(this, location)
            }
            _ => false,
        }),
        Native::CanSee | Native::PickWallAdjust | Native::FindPathTowardNearest | Native::MoveSmooth | Native::GetAssistedAimTarget => return vm.ai_native(native, this, args),
        Native::MoveTo | Native::MoveToward | Native::MoveToWithRadius | Native::MoveTowardWithRadius | Native::FinishRotation | Native::WaitForLanding
        | Native::InLatentExecution | Native::ReachedDestination | Native::EndClimbLadder | Native::StopWaiting | Native::FindStairRotation | Native::WaitToSeeEnemy | Native::EAdjustJump => return vm.move_native(native, this, args),
        Native::FindPathToward | Native::FindPathTo | Native::FindRandomDest | Native::actorReachable | Native::pointReachable | Native::ClearPaths
        | Native::BuildPathNodeLists | Native::BuildHomeNodeLists => return vm.nav_native(native, this, args),
        Native::KAddImpulse | Native::KAddForce | Native::KAddTorque | Native::KWake | Native::KDisableKarma | Native::KSetMass | Native::KGetMass | Native::KSetFriction
        | Native::KGetFriction | Native::KSetRestitution | Native::KGetRestitution | Native::KSetDampingProps | Native::KGetDampingProps
        | Native::KSetImpactThreshold | Native::KGetImpactThreshold | Native::KSetStayUpright | Native::KSetBlockKarma | Native::KEnableCollision
        | Native::KDisableCollision | Native::KGetCOMPosition | Native::KSetSkelVel | Native::KFreezeRagdoll => return vm.karma_native(native, this, args),
        Native::SafeToGoIntoStasis | Native::GetGameMap | Native::PointToLineDist | Native::VectorsInFuzz | Native::ThickLineCylinderCollide
        | Native::FireHurtRadius | Native::ForceInit | Native::ForceSpawn | Native::SetProjectionNormal | Native::SetMaxParticles
        | Native::SetLiveParticleTimes | Native::SpawnParticleLength => return vm.game_native(native, this, args),
        Native::Floor | Native::Localize | Native::GetLanguage | Native::GetDisplayLanguage | Native::SetLanguage | Native::InterpCurveEval | Native::LoadFileToString
        | Native::SaveStringToFile | Native::FindFiles | Native::OpenLog | Native::Logf | Native::CloseLog | Native::Error | Native::GetMapName | Native::GetURLMap
        | Native::DoesMapExist | Native::GetAddressURL | Native::GetDefaultURL | Native::UpdateURL | Native::GetNextInt | Native::GetNextIntDesc | Native::IsSteamDeck
        | Native::IsWorkshopBuild | Native::SteamUnlockAchievement | Native::SteamLockAchievement | Native::SteamGetAchievement
        | Native::SteamGetStatFloat | Native::SteamGetStatInt | Native::SteamRequestStats | Native::SteamSetStatInt | Native::SteamSetStatFloat
        | Native::SteamResetStats | Native::SteamIndicateAchievementProgress | Native::SteamPurgeWorkshop | Native::SteamOwnsGame
        | Native::SteamViewStorePage | Native::CopyToClipboard | Native::PasteFromClipboard | Native::ResetKeyboard | Native::GetNetworkNumber
        | Native::GetPlayerNetworkAddress | Native::ParseKillMessage | Native::DeleteSave | Native::GetMapFileName | Native::LaunchDLC
        | Native::LaunchSocialNetwork | Native::GetNextSkin => return vm.util_native(native, this, args),
        Native::SetBoneRotation | Native::SetBoneDirection | Native::SetBoneLocation | Native::AttachActor | Native::DetachActor | Native::Kill
        | Native::SpawnParticle | Native::Pling | Native::GetCameraLocation | Native::ScreenToWorld | Native::PlayFlashbangSound
        | Native::AddSpawnProperty | Native::SetLodStaticMesh | Native::ClearLodStaticMesh | Native::DeleteLodStaticMesh | Native::GetCacheEntry
        | Native::MoveCacheEntry | Native::KSetCOMOffset | Native::KGetCOMOffset | Native::KSetInertiaTensor | Native::KGetInertiaTensor
        | Native::KGetConstraintForce | Native::KGetConstraintTorque | Native::KUpdateConstraintParams | Native::Play | Native::Pause | Native::IsPlaying
        | Native::IsPaused | Native::GetWidth | Native::GetHeight | Native::InitializeMovie | Native::StopNow | Native::StopAtEnd | Native::DrawColoredText | Native::ReplaceTexture
        | Native::GraphData => return vm.actor_native(native, this, args),
        Native::HurtRadius | Native::HurtRadiusEX => return vm.hurt_radius_native(native, this, args),
        Native::PickTarget => Value::Object(this.and_then(|this| vm.pick_target(this, args))),
        Native::ClientTravel => {
            vm.diagnostics.log.push(format!("ClientTravel {}", s(0)));
            let url = crate::TravelUrl::parse(s(0)).resolve(&vm.url_options, crate::ETravelType::from_byte(i(1) as u8)).text();
            vm.host.commands.push(HostCommand::Travel(url));
            vm.travel_items = b(2);
            return Ok(None);
        }
        Native::Travel => {
            vm.diagnostics.log.push(format!("Travel {}", s(0)));
            let url = crate::TravelUrl::parse(s(0)).resolve(&vm.url_options, crate::ETravelType::Absolute).text();
            vm.host.commands.push(HostCommand::Travel(url));
            vm.travel_items = false;
            return Ok(None);
        }
        Native::ShowSubtitle => {
            if let Some(this) = this
                && let Some(index) = crate::util_natives::subtitle_index(vm.get(this, "SubTag").as_ref(), s(0))
                && let Some(player) = vm.player()
            {
                vm.event(player, "CallSubtitle", vec![Value::Object(None), Value::Int(index), Value::Int(-1), Value::Object(None)])?;
            }
            return Ok(None);
        }
        Native::GetTotalSceneTime => Value::Float(this.map_or(0.0, |this| vm.total_scene_time(this))),
        Native::GetSoundDuration => {
            let duration = object(0).and_then(|sound| vm.sound_duration(sound)).unwrap_or(0.0);
            Value::Float(duration)
        }
        Native::SetPhysics => {
            if let Some(this) = this {
                vm.set_physics_byte(this, i(0) as u8, None)?;
            }
            return Ok(None);
        }
        Native::PlayAnim | Native::PlayAnimAt | Native::LoopAnim | Native::TweenAnim | Native::IsAnimating | Native::FinishAnim | Native::HasAnim
        | Native::StopAnimating | Native::FreezeAnimAt | Native::IsTweening | Native::AnimIsInGroup | Native::LinkSkelAnim | Native::GetDefaultAnim
        | Native::AnimBlendParams | Native::AnimBlendToAlpha | Native::GetAnimParams | Native::EnableChannelNotify | Native::SetBoneScale => {
            return vm.anim_native(native, this, args);
        }
        Native::Log | Native::Warn => {
            vm.diagnostics.log.push(s(0).to_owned());
            return Ok(None);
        }
        _ => return Err(Error::UnknownNative(name.to_owned())),
    };
    Ok(Some(value))
}

fn console_command<L: LoaderRef>(vm: &mut Vm<L>, command: &str) -> Result<Value, Error> {
    if let Some(answer) = vm.input_command(command) {
        return Ok(Value::Str(answer));
    }
    Ok(Value::Str(match command.to_ascii_lowercase().as_str() {
        "getping" | "getcurrentdisplaynumber" | "getrenderdeviceogl" => "0".to_owned(),
        "getcurrentres" => {
            let size = |property, default| match vm.media.canvas.and_then(|canvas| vm.get(canvas, property)) {
                Some(Value::Int(size)) if size > 0 => size,
                _ => default,
            };
            format!("{}x{}", size(Prop::SizeX, 1024), size(Prop::SizeY, 768))
        }
        "getfullscreen" => (if vm.is_fullscreen() { "True" } else { "False" }).to_owned(),
        "getcurrentcolordepth" => "32".to_owned(),
        "getdisplaycount" => "1".to_owned(),
        "flush" => String::new(),
        "togglefullscreen" => {
            let fullscreen = !vm.is_fullscreen();
            vm.request_window(None, fullscreen);
            String::new()
        }
        "setfullscreenwindowed" => {
            let fullscreen = vm.is_fullscreen();
            vm.request_window(None, fullscreen);
            String::new()
        }
        lower if lower.starts_with("setres ") => {
            vm.set_res(&command["setres ".len()..]);
            String::new()
        }
        lower if lower.starts_with("setrenderdeviceogl ") => String::new(),
        lower if ["brightness ", "contrast ", "gamma "].iter().any(|p| lower.starts_with(p)) => {
            let (name, value) = command.split_once(' ').unwrap_or_default();
            vm.set_command(&format!("{} {name} {value}", crate::VIEWPORT_MANAGER));
            String::new()
        }
        lower if lower == "cinematics" || lower.starts_with("cinematics ") => {
            let arg = lower["cinematics".len()..].trim();
            let on = &mut vm.host.client.cinematics;
            *on = if arg.is_empty() { !*on } else { arg.parse::<f64>().map_or(0, |v| v as i32) != 0 };
            String::new()
        }
        lower if lower == "cinematicsratio" || lower.starts_with("cinematicsratio ") => {
            vm.host.client.cinematics_ratio = Some(lower["cinematicsratio".len()..].trim().parse().unwrap_or(0.0));
            String::new()
        }
        lower if lower.starts_with("setmouse ") => {
            let mut numbers = lower["setmouse ".len()..].split_whitespace().map(|n| n.parse().unwrap_or(0));
            vm.host.commands.push(HostCommand::SetMouse([numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)]));
            String::new()
        }
        lower if lower.starts_with("getsupportedresolution") => vm.supported_resolution(lower),
        lower if lower.starts_with("getfiletimestr ") => vm.file_time_text(command["getfiletimestr ".len()..].trim()),
        "menucancel" => String::new(),
        lower if lower.starts_with("toggleime") => String::new(),
        "resetfirstrun" => {
            vm.save_setting("FirstRun", "FirstRun", "0");
            String::new()
        }
        lower if lower.starts_with("netspeed ") => {
            let speed: i32 = lower["netspeed ".len()..].trim().parse().unwrap_or(0);
            vm.set_command(&format!("Engine.Player ConfiguredInternetSpeed {speed}"));
            String::new()
        }
        "disconnect" => {
            vm.host.commands.push(HostCommand::Travel("Entry".to_owned()));
            String::new()
        }
        "showmouse" | "hidemouse" => {
            if let Some(viewport) = vm.viewport() {
                vm.set(viewport, Prop::bShowWindowsMouse, Value::Bool(command.eq_ignore_ascii_case("showmouse")));
            }
            String::new()
        }
        "exit" | "quit" => {
            vm.host.commands.push(HostCommand::Quit);
            String::new()
        }
        lower if lower.starts_with("open ") || lower.starts_with("start ") => {
            vm.host.commands.push(HostCommand::Travel(command.split_once(' ').map_or("", |(_, url)| url).trim().to_owned()));
            String::new()
        }
        lower if lower.starts_with("savegame ") => {
            let saved = match lower["savegame ".len()..].trim().parse() {
                Ok(slot) => vm.save_game(slot)?,
                Err(_) => false,
            };
            (if saved { "1" } else { "0" }).to_owned()
        }
        lower if lower.starts_with("checksavegame ") => {
            let found = lower["checksavegame ".len()..].trim().parse().is_ok_and(|slot| vm.save_exists(slot));
            (if found { "1" } else { "0" }).to_owned()
        }
        "getgmtime" => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs()).to_string(),
        lower if lower.starts_with("iskeybind ") => (if vm.is_key_bound(&command["iskeybind ".len()..]) { "1" } else { "0" }).to_owned(),
        lower if lower.starts_with("set ") => {
            vm.set_command(&command["set ".len()..]);
            String::new()
        }
        lower if lower.starts_with("get ") => vm.get_command(&command["get ".len()..])?,
        _ => {
            if !vm.player_console_exec(command)? {
                vm.diagnostics.log.push(format!("ConsoleCommand: {command}"));
            }
            String::new()
        }
    }))
}

fn set_actor_properties<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: ObjectId, args: &[Value]) -> Result<(), Error> {
    let properties: &[Prop] = match native {
        Native::SetCollision => &[Prop::bCollideActors, Prop::bBlockActors, Prop::bBlockPlayers],
        Native::SetCollisionSize => &[Prop::CollisionRadius, Prop::CollisionHeight],
        Native::SetDrawScale => &[Prop::DrawScale],
        Native::SetDrawScale3D => &[Prop::DrawScale3D],
        Native::SetStaticMesh => &[Prop::StaticMesh],
        Native::SetDrawType => &[Prop::DrawType],
        Native::SetRotation => &[Prop::Rotation],
        Native::SetRelativeLocation => &[Prop::RelativeLocation],
        Native::SetRelativeRotation => &[Prop::RelativeRotation],
        Native::LinkMesh => &[Prop::Mesh],
        _ => &[],
    };
    // Collision turned off untouches before the flags change, as retail.
    let collided = (native == Native::SetCollision).then(|| vm.flag_of(this, Prop::bCollideActors));
    // As retail, `SetRotation` is a zero-delta move: the actors based on it turn with it.
    let turned_from = (native == Native::SetRotation).then(|| vm.rotator_of(this, Prop::Rotation));
    if let Some(was) = collided {
        let now = match args.first() {
            Some(Value::Bool(now)) => *now,
            _ => was,
        };
        vm.set_collision_before(this, was, now)?;
    }
    for (&property, value) in properties.iter().zip(args) {
        if !matches!(value, Value::Map) {
            vm.set(this, property, value.clone());
        }
    }
    if matches!(native, Native::SetRotation | Native::SetDrawScale | Native::SetDrawScale3D) {
        vm.volume_moved(this);
    }
    // A changed primitive (scale, mesh, rotation) of a moving actor changes its reach: re-placed too (static ones keep
    // theirs).
    let collision = matches!(native, Native::SetCollision | Native::SetCollisionSize);
    let reshaped = matches!(native, Native::SetDrawScale | Native::SetDrawScale3D | Native::SetStaticMesh | Native::SetDrawType | Native::SetRotation | Native::LinkMesh);
    if collision || (reshaped && !vm.flag_of(this, Prop::bStatic)) {
        vm.relocate(this);
    }
    if let Some(old_rotation) = turned_from {
        vm.turn_attached(this, old_rotation)?;
    }
    if let Some(was) = collided {
        let now = vm.flag_of(this, Prop::bCollideActors);
        vm.set_collision_after(this, was, now)?;
    } else if collision {
        vm.update_touching(this)?;
    }
    Ok(())
}

fn controller_list<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>) {
    let Some(this) = this else { return };
    let Some(level) = vm.object_of(this, Prop::Level) else { return };
    let mut previous: Option<ObjectId> = None;
    let mut current = vm.object_of(level, Prop::ControllerList);
    let mut guard = 0;
    while let Some(controller) = current {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        let after = vm.object_of(controller, Prop::nextController);
        if controller == this {
            match previous {
                Some(previous) => vm.set(previous, Prop::nextController, Value::Object(after)),
                None => vm.set(level, Prop::ControllerList, Value::Object(after)),
            };
            break;
        }
        previous = Some(controller);
        current = after;
    }
    if native == Native::AddController {
        let first = vm.object_of(level, Prop::ControllerList);
        vm.set(this, Prop::nextController, Value::Object(first));
        vm.set(level, Prop::ControllerList, Value::Object(Some(this)));
    } else {
        vm.set(this, Prop::nextController, Value::Object(None));
    }
}

fn play_sound<L: LoaderRef>(vm: &mut Vm<L>, this: Option<ObjectId>, args: &[Value]) {
    let f = |n: usize| args.get(n).map_or(0.0, float);
    let b = |n: usize| args.get(n).is_some_and(truthy);
    let given = |n: usize| !matches!(args.get(n), None | Some(Value::Map));
    let Some(&Value::Object(Some(sound))) = args.first() else { return };
    let volume = (if given(2) { f(2) } else { actor_float(vm, this, Prop::TransientSoundVolume) } * vm.sound_volume()).clamp(0.0, 1.0);
    let radius = match if given(4) { f(4) } else { actor_float(vm, this, Prop::TransientSoundRadius) } {
        0.0 => AUDIO_DEFAULT_RADIUS,
        radius => radius,
    };
    let dilation = match vm.level().and_then(|level| vm.get(level, Prop::TimeDilation)) {
        Some(Value::Float(dilation)) => dilation,
        _ => 1.0,
    };
    let pitch = if given(5) { f(5) } else { 1.0 } * dilation;
    let slot = if given(1) { args.get(1).map_or(0, int) as u8 } else { 1 };
    let attenuate = !given(6) || b(6);
    let played = PlayedSound { actor: this, sound: vm.object(sound).path.clone(), slot, volume, radius, pitch, attenuate, no_override: given(3) && b(3), flashbang: None };
    vm.subtitle_insert_sound(played.actor, &played.sound);
    vm.host.commands.push(HostCommand::Sound(played));
}

fn play_music<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Value {
    let f = |n: usize| args.get(n).map_or(0.0, float);
    let given = |n: usize| !matches!(args.get(n), None | Some(Value::Map));
    let attenuate = native == Native::PlayMusicAttenuate;
    let override_volume = f(if attenuate { 5 } else { 2 });
    let music_volume = if override_volume > 0.0 { override_volume } else { vm.music_volume() };
    let volume = if attenuate { (if given(2) { f(2) } else { actor_float(vm, this, Prop::TransientSoundVolume) }) * music_volume } else { music_volume };
    let volume = volume.clamp(0.0, 1.0);
    if volume == 0.0 {
        return Value::Int(0);
    }
    vm.host.music_handle += 1;
    let radius = if attenuate { if given(3) { f(3) } else { actor_float(vm, this, Prop::TransientSoundRadius) } } else { 0.0 };
    let song = match args.first() {
        Some(Value::Str(song)) => song.clone(),
        _ => String::new(),
    };
    let play = MusicCommand::Play {
        handle: vm.host.music_handle,
        song,
        actor: this.filter(|_| attenuate),
        fade_in: f(1),
        volume,
        radius: if attenuate && radius <= 0.0 { 80.0 } else { radius },
        pitch: if attenuate && given(4) { f(4) } else { 1.0 },
    };
    vm.host.commands.push(HostCommand::Music(play));
    Value::Int(vm.host.music_handle)
}

fn actor_float<L: LoaderRef>(vm: &Vm<L>, this: Option<ObjectId>, property: Prop) -> f32 {
    this.map_or(0.0, |this| vm.float_of(this, property))
}

pub(crate) fn iterator_outs(name: &str) -> &'static [usize] {
    match name {
        "TraceActors" => &[2, 3],
        _ => &[],
    }
}

/// An actor the loop body spawns, added at the list's end, is reached.
/// For `ChildActors` and `BasedActors` this is (unverified).
pub(crate) fn walks_actor_list(name: &str) -> Option<&'static str> {
    ["AllActors", "DynamicActors", "ChildActors", "BasedActors", "RadiusActors", "VisibleActors"].into_iter().find(|&walk| walk == name)
}

pub(crate) fn iterate<L: LoaderRef>(vm: &mut Vm<L>, name: &str, this: Option<ObjectId>, args: &[Value]) -> Result<Vec<Value>, Error> {
    let class = match args.first() {
        Some(&Value::Object(Some(class))) => vm.object(class).name,
        _ => vm.script.lookups.class_names.object,
    };
    let all_objects = name == "AllObjects";
    let tested = std::cell::RefCell::new(vm.script.lookups.class_tests.remove(&class).unwrap_or_default());
    let of_class = |vm: &Vm<L>, candidates: &mut dyn Iterator<Item = ObjectId>, keep: &dyn Fn(ObjectId) -> bool| -> Vec<Value> {
        let mut tested = tested.borrow_mut();
        candidates
            .filter(|&object| {
                let of_class = vm.object(object).class.is_some_and(|c| *tested.entry(c).or_insert_with(|| vm.class_has_name(c, class)));
                of_class && !vm.is_deleted(object) && keep(object)
            })
            .map(|object| Value::Object(Some(object)))
            .collect()
    };
    let objects = |vm: &Vm<L>, keep: &dyn Fn(ObjectId) -> bool| -> Vec<Value> {
        if all_objects { of_class(vm, &mut vm.object_ids(), keep) } else { of_class(vm, &mut vm.actors(), keep) }
    };
    let radius = args.get(1).map_or(0.0, float);
    let centre = |vm: &Vm<L>| match args.get(2) {
        Some(location) if is_struct_value(location) => vec3(Some(location)),
        _ => vec3(this.and_then(|this| vm.get(this, Prop::Location))),
    };
    let within = |vm: &Vm<L>, centre: Vector, object: ObjectId| {
        let at = vm.vector(object, Prop::Location);
        (0..3).map(|i| (at[i] - centre[i]).powi(2)).sum::<f32>() <= radius * radius
    };
    let items = match name {
        "AllActors" | "DynamicActors" | "AllObjects" => {
            let tag = match args.get(1) {
                Some(&Value::Name(tag)) if tag != NameId::NONE => Some(tag),
                _ => None,
            };
            let dynamic = (name == "DynamicActors").then(|| vm.dynamic_actors());
            let keep = |object: ObjectId| tag.is_none_or(|tag| vm.get(object, Prop::Tag) == Some(Value::Name(tag)));
            match dynamic {
                Some(dynamic) => of_class(vm, &mut dynamic.into_iter(), &keep),
                None => objects(vm, &keep),
            }
        }
        "TouchingActors" => {
            let touching = match this.and_then(|this| vm.get(this, Prop::Touching)) {
                Some(Value::Array(list)) => list,
                _ => Vec::new(),
            };
            touching.into_iter().filter(|v| matches!(v, &Value::Object(Some(o)) if vm.is_a(o, class))).collect()
        }
        "ChildActors" => objects(vm, &|object| this.is_some() && vm.get(object, Prop::Owner) == Some(Value::Object(this))),
        "BasedActors" => objects(vm, &|object| this.is_some() && vm.get(object, Prop::Base) == Some(Value::Object(this))),
        // Centre strictly within `Radius` + the actor's own `CollisionRadius`
        // (a match of radius 5 finds a gas puddle of radius 100 around it).
        "RadiusActors" => {
            let centre = centre(vm);
            objects(vm, &|object| {
                let radius = radius + vm.get(object, Prop::CollisionRadius).as_ref().map_or(0.0, float);
                let at = vm.vec3_of(object, Prop::Location);
                (0..3).map(|i| (at[i] - centre[i]).powi(2)).sum::<f32>() < radius * radius
            })
        }
        "CollidingActors" | "VisibleCollidingActors" => {
            let at = centre(vm);
            let near = vm.colliders_near([at[0], at[1]], [at[0], at[1]], radius);
            let visible = name == "VisibleCollidingActors";
            let ignore_hidden = visible && args.get(3).is_some_and(truthy);
            let mut items = of_class(vm, &mut near.into_iter().map(|e| e.actor), &|object| {
                vm.flag_of(object, Prop::bCollideActors) && !(ignore_hidden && vm.flag_of(object, Prop::bHidden)) && within(vm, at, object)
            });
            if visible {
                vm.keep_line_of_sight(this, at, &mut items);
            }
            items
        }
        "VisibleActors" => {
            let at = centre(vm);
            let mut items = objects(vm, &|object| {
                let offset = vm.vector(object, Prop::Location) - at;
                !vm.flag_of(object, Prop::bHidden) && (radius == 0.0 || offset.dot(offset) < radius * radius)
            });
            vm.keep_line_of_sight(this, at, &mut items);
            items
        }
        "TraceActors" => vm.trace_actors_items(this, args),
        _ => {
            vm.script.lookups.class_tests.insert(class, tested.into_inner());
            return Err(Error::UnknownNative(name.to_owned()));
        }
    };
    vm.script.lookups.class_tests.insert(class, tested.into_inner());
    Ok(items)
}

pub(crate) fn is_struct_value(value: &Value) -> bool {
    matches!(value, Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))
}

pub(crate) fn vec3(value: Option<impl std::borrow::Borrow<Value>>) -> Vector {
    match value.as_ref().map(std::borrow::Borrow::borrow) {
        Some(&Value::Vector(v)) => v,
        Some(&Value::Rotator(r)) => Vector::new(r.pitch as f32, r.yaw as f32, r.roll as f32),
        Some(Value::Struct(members)) => [0, 1, 2].map(|i| members.get(i).map_or(0.0, float)).into(),
        _ => Vector::ZERO,
    }
}

pub(crate) fn object_arg(args: &[Value], n: usize) -> Option<ObjectId> {
    match args.get(n) {
        Some(&Value::Object(object)) => object,
        _ => None,
    }
}

pub(crate) fn optional_float(args: &[Value], n: usize, default: f32) -> f32 {
    match args.get(n) {
        None | Some(Value::Map) => default,
        Some(value) => float(value),
    }
}

pub(crate) fn bool_arg(args: &[Value], n: usize) -> bool {
    args.get(n).is_some_and(truthy)
}

pub(crate) fn name_arg(args: &[Value], n: usize) -> NameId {
    match args.get(n) {
        Some(&Value::Name(name)) => name,
        _ => NameId::NONE,
    }
}

pub(crate) fn text_arg(args: &[Value], n: usize) -> String {
    match args.get(n) {
        Some(Value::Str(text)) => text.clone(),
        _ => String::new(),
    }
}

pub(crate) fn vector_arg(args: &[Value], n: usize) -> Option<Vector> {
    match args.get(n) {
        Some(value @ (Value::Struct(_) | Value::Vector(_) | Value::Rotator(_))) => Some(vec3(Some(value))),
        _ => None,
    }
}

pub(crate) fn rot3(value: Option<impl std::borrow::Borrow<Value>>) -> Rotator {
    match value.as_ref().map(std::borrow::Borrow::borrow) {
        Some(&Value::Rotator(r)) => r,
        other => {
            let v = vec3(other);
            Rotator::new(v.x as i32, v.y as i32, v.z as i32)
        }
    }
}

pub(crate) fn system_time() -> [i32; 8] {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let small = |value: u32| value as i32;
    [
        now.year(),
        small(now.month()),
        small(now.weekday().num_days_from_sunday()),
        small(now.day()),
        small(now.hour()),
        small(now.minute()),
        small(now.second()),
        small((now.nanosecond() / 1_000_000).min(999)),
    ]
}

/// Runs before actors begin play and at the start of every level tick (unverified).
pub(crate) fn update_time<L: LoaderRef>(vm: &mut Vm<L>, level: ObjectId) {
    let properties = [Prop::Year, Prop::Month, Prop::DayOfWeek, Prop::Day, Prop::Hour, Prop::Minute, Prop::Second, Prop::Millisecond];
    for (property, value) in properties.into_iter().zip(system_time()) {
        vm.set(level, property, Value::Int(value));
    }
}

/// As the engine counts: the day of the month and every month up to and including the current one (30 days for April,
/// June, September, November; February 29 when the years since 2000 divide by 4, else 28; others 31), and 366 or 365
/// days for each year since 2000 alike.
fn milliseconds_since_2000<L: LoaderRef>(vm: &mut Vm<L>, level: Option<ObjectId>) -> [f32; 3] {
    let [year, month, _, day, hour, minute, second, millisecond] = system_time();
    if let Some(level) = level {
        update_time(vm, level);
    }
    const DAY: f64 = 86_400.0;
    let years = year - 2000;
    let mut total = f64::from(day) * DAY + f64::from(hour) * 3600.0 + f64::from(minute) * 60.0 + f64::from(second) + f64::from(millisecond) / 1000.0;
    for m in 1..=month {
        total += DAY * match m {
            4 | 6 | 9 | 11 => 30.0,
            2 if years % 4 == 0 => 29.0,
            2 => 28.0,
            _ => 31.0,
        };
    }
    for y in 1..=years {
        total += DAY * if y % 4 == 0 { 366.0 } else { 365.0 };
    }
    let whole_days = (total / DAY) as i32;
    let rest = total - f64::from(whole_days) * DAY;
    let seconds = rest as i32;
    [whole_days as f32, seconds as f32, ((rest - f64::from(seconds)) * 1000.0) as i32 as f32]
}

pub(crate) fn orthogonal_rotation(x: Vector, y: Vector, z: Vector) -> Rotator {
    let units = |angle: f32| (angle * 32768.0 / std::f32::consts::PI) as i32;
    let (pitch, yaw) = (units(x[2].atan2((x[0] * x[0] + x[1] * x[1]).sqrt())), units(x[1].atan2(x[0])));
    let unrolled = Rotator::new(pitch, yaw, 0).axes();
    let roll = units(z.dot(unrolled[1]).atan2(y.dot(unrolled[1])));
    [pitch, yaw, roll].into()
}

