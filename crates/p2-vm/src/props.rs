use std::cell::{Cell, RefCell};
use std::sync::Arc;

use crate::memory::{Field, Shape};
use crate::object::Span;
use crate::{ClassId, NameId, ObjectStore};

macro_rules! properties {
    ($($name:ident),* $(,)?) => {
        #[allow(non_camel_case_types, reason = "named as the scripts declare them")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Prop { $($name),* }

        const PROPERTY_NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

properties!(
    Year, Month, DayOfWeek, Day, Hour, Minute, Second, Millisecond,
    bDeleteMe, bCollideActors, bCollideWorld, bBlockActors, bBlockPlayers, bBlockNonZeroExtentTraces,
    bBlockZeroExtentTraces, bIgnoreOutOfWorld, bWorldGeometry, bStatic, bNoDelete, bUseCylinderCollision, bOnlyAffectPawns, bProjTarget,
    bAllowZThrow, bMeleeDamage, bMeleeWeapon,
    bAlwaysTick, CollisionRadius, CollisionHeight, Touching, Owner, Base, bCollideWhenPlacing, bSkipEncroachmentCheck,
    Location, Rotation, Velocity, Acceleration, OldAcceleration, Physics, Brush, Mesh, StaticMesh, DrawType, DrawScale,
    DrawScale3D, PrePivot, LastRenderTime,
    TimerRate, TimerCounter, bTimerLoop, LifeSpan,
    Controller, Player, bStasis,
    bClassBlocker, bExactClassesOnly, bAllowOnlySpecifiedClasses, BlockedClasses, Priority, HeadVolume,
    TimeSeconds,

    AccelRate, AdjustLoc, AirControl, AirSpeed, Alertness, Anchor, AnimAction, Attach, Attached, AttachmentBone,
    AvgPhysicsTime, bAdjacentZoneHearing, bAdjusting, bAdvancedTactics, bAllowStasis, BaseEyeHeight,
    BaseMovementRate, BasePos, BaseRot, bBehindView, bBlockKarma, bBounce, bCanFly, bCanJump, bCanStrafe,
    bCanWalkOffLedges, bCrawler, bDoTorsoTwist, bEnemyInfoValid, bFixedRotationDir, bForceNoStrafing, bHidden,
    bIgnoreTimeDilation, bInitializeAnimation, bInterpolating, bIsCrouched, bIsDeathCrawling, bIsPlayer, bIsWalking,
    bJustTeleported, bKAllowRotate, bKStayUpright, BlendChangeTime, bLOSflag, bLOSHearing, bMovable,
    bNeverUseStrafing, bNoVelocityUpdate, bPendingStasis, bPhysicsAnimUpdate, bPlayedDeath, bPreparingMove,
    bPushable, BreathTime, bReducedSpeed, bRotateToDesired, bSameZoneHearing, bSliderStasis, bSoftKillZ, bSpecialMove, bStartup,
    bTearOff, bTryToUncrouch, bUpdateEyeheight, bWantsToCrouch, bWasCrouched, bWasDeathCrawling, bWasOnGround, bWasWalking, ClimbDir,
    ConstantAcceleration, ControllerList, CrouchHeight, CrouchRadius, CurrentPath, DesiredRotation, DesiredSpeed,
    Destination, DestinationOffset, Disabled, EffectClass, Emitters, Enemy, Floor, FocalPoint, Focus, Game, Gravity,
    GroundFriction, GroundSpeed, Health, HearingThreshold, Instigator, KarmaMaxSpeed, KeyNum, KillZ,
    KMass, KLinearDamping, KAngularDamping, KFriction, KRestitution, GroundPitchTime,
    KImpactThreshold, KParams, KSkeleton, KStartEnabled, LastSeeingPos, LastSeenPos, LastSeenTime, LatentFloat,
    Level, Mass, MaxDesiredSpeed, MaxSpeed, MeleeRange, MinHitWall, MovementAnims, MovementBlendStartTime,
    MoverGlideType, MoveTarget, MoveTimer, NetMode, nextController, NextPathRadius, NextSwitchCountdown, NextURL,
    OffsetLocation, OffsetRotation, OldPhysics, OldPos, OldRot, OldRotYaw, OldZ, OnLadder, PathCheckTimeActor,
    PathCheckTimePoint, Pauser, Pawn, PhysAlpha, PhysicsVolume, Radius, RelativeLocation, RelativeRotation, Role,
    RotationRate, RouteGoal, SerpentineDist, SerpentineTime, Sound, SoundDampening, Tag, TargetRadius,
    TerminalVelocity, TimeDilation, TimeSecondsAlways, TimeTillStasis, UncrouchTime, ViewTarget, Volume,
    WalkFriction, WalkingPct, WaterSpeed, Weapon, ZoneVelocity,
    AmbientGlow, bUnlit, MaxLights, Skins,
    FovAngle,
    Visibility, SightRadius, PeripheralVision, AutoAim, NavigationPointList, bTransientEndPoint, nextNavigationPoint,
    myHUD, Viewport, Font, Style, bCenter, CurX, CurY, CurYL, OrgX, OrgY, ClipX, ClipY, FontScaleX, FontScaleY, SpaceX,
    DrawColor, Console, InteractionMaster, BaseMenu,
    bWindowsMouseAvailable, WindowsMouseX, WindowsMouseY, bShowWindowsMouse, bWindowVisible, GlobalInteractions, LocalInteractions,
    EyeHeight, DefaultFOV, bTeamGame, PlayerReplicationInfo, Team, ProjectionNormal, SightCounter, bAmbientCreature,
    RealProjectionNormal, MaxParticles, StartLocationOffset, LineStart, LineEnd, Actor, PlayerInput, Outer, Duration, IntPoint, PathStyle, PathLength, SampleLocations, PathVelocity, bConstantPathVelocity,
    TotalSceneTime, CurrentTime, PctStarting, PctEnding, PctDuration, Delay, Status, SubActions, bIsRunning,
    SceneSpeed, CurrentAction, bIsSceneStarted, GameSpeed, Viewer, bLetPlayerSkip, bWantsToSkip, PctSceneComplete,
    bLetPlayerFastForward, MatineeFastForwardSpeed, NextSceneTag, bLooping, OldPawn, CamOrientation, CameraShake,
    FlashFog, bFadeOut, EventName, CameraEffect, StartAlpha, EndAlpha, Alpha, DisableAfterDuration, Prop, bValue,
    MinAlpha, MaxAlpha, FadeInTimePercent, FadeOutTimePercent, OverlayMaterial, OverlaySizeFactor, OverlayXPos,
    OverlayYPos, OverlayStyle, PathList, End, RouteCache, bBlocked, bCanCrouch, MaxFallSpeed, ExtraCost, RouteDist,
    bCanWalk, NextNode, HomeNodeLists, PathNodeList, Buoyancy, bWaterVolume, bBounceVelocity, FluidFriction, bNoPhysicalLadder,
    LadderSpeed, bTrailerSameRotation, bTrailerPrePivot, Inventory, bTravel, ForceType,
    bOwnerNoSee, bOnlyOwnerSee, bDynamicLight,
    LightType, LightBrightness, LightHue, LightSaturation, LightRadius, LightCone, LightPeriod, LightPhase, LightEffect, bCorona,
    noise1spot, noise1time, noise1loudness, noise2spot, noise2time, noise2loudness,
    SubtitleManager, SubSound, InvalidSounds, PendingSounds, InvalidTime,
    bCanSwim, bCanOpenDoors, bCanDoSpecial, bCanClimbLadders,
    ForwardStrafeBias, BackwardStrafeBias, CameraEffects,
    FootRot, TurnDir, HeadBone, SpineBone1, SpineBone2, FootTurning, FootStill, ViewPitch,
    AmbientSound, SoundVolume, SoundPitch, SoundRadius, ZoneSounds,
    Region,
    TransientSoundVolume, TransientSoundRadius, ActiveParticles, TurnRightAnim, TurnLeftAnim, PhysRate, KeyPos, KeyRot,
    LookDir, SelectedItem, CheatManager, AdminManager, SizeX, SizeY, ForceRadius, ForceScale, NotifyName, DestroyTag,
    Bone, StaticMeshInstance, AntiPortal,
    Texture,
    Actions, StartControlPoint, EndControlPoint, FluidXSize, FluidYSize, FluidGridSpacing, FluidGridType, ProjTexture, FOV, MaxTraceDistance,
    Material,
    ForcedVisibilityZoneTag, bAcceptsProjectors, AmbientHue, AmbientSaturation, AmbientBrightness, bDistanceFog,
    DistanceFogColor, DistanceFogStart, DistanceFogEnd, FrameBufferBlendingOp, MaterialBlendingOp, bProjectBSP,
    bProjectTerrain, bProjectStaticMesh, bProjectActor, bProjectOnUnlit, ProjectTag, bDisturbFluidSurface, SkyZone,
    bTerrainZone, Brightness, MinCoronaSize, MaxCoronaSize,
    FluidHeightScale, FluidSpeed, FluidDamping, FluidNoiseFrequency, FluidNoiseStrength, UTiles, UOffset, VTiles, VOffset,
    AlphaCurveScale, AlphaHeightScale, AlphaMax, RippleVelocityFactor, WarmUpTime, UpdateRate, ClampBitmap, FluidInfo,
    Frequency, Phase, Strength,
);

/// Every property the host reads to draw an actor: its frame, its look and a restyle. A write that changes one moves
/// the object's `drawn` stamp, and in debug builds the host's reads fail on any other property.
pub(crate) const DRAWN: &[Prop] = &[
    Prop::Location, Prop::Rotation, Prop::DrawScale, Prop::DrawScale3D, Prop::PrePivot, Prop::Velocity, Prop::Owner,
    Prop::Controller, Prop::bIsPlayer, Prop::TimeDilation, Prop::bDeleteMe, Prop::bHidden, Prop::bOwnerNoSee,
    Prop::bOnlyOwnerSee, Prop::Base, Prop::AttachmentBone, Prop::RelativeLocation, Prop::RelativeRotation, Prop::Emitters,
    Prop::Disabled, Prop::Mesh, Prop::StaticMesh, Prop::Skins, Prop::DrawType, Prop::bUnlit, Prop::AmbientGlow,
    Prop::MaxLights, Prop::StaticMeshInstance, Prop::bCollideActors, Prop::bBlockActors, Prop::ForcedVisibilityZoneTag,
    Prop::bStatic, Prop::bAcceptsProjectors, Prop::Texture, Prop::Style,
];

macro_rules! kinds {
    ($($name:ident),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Kind { $($name),* }

        const KIND_NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

kinds!(
    Actor, Pawn, Controller, PlayerController, Brush, Mover, Projectile, KConstraint, Volume, BlockingVolume,
    SceneManager, FPSPawn, NavigationPoint, AIController, Decoration, LevelInfo, KarmaParamsSkel, PhysicsVolume,
    FluidPuddle, Emitter, Projector, Movie, CameraTitle,
    ActionMoveCamera, SubActionOrientation, SubActionFade, SubActionFOV, SubActionSceneSpeed, SubActionGameSpeed, SubActionCameraShake, SubActionTrigger, SubActionCameraEffect, SubActionTitle, SubActionSceneProps,
);

#[derive(Debug, Default)]
pub(crate) struct Resolved {
    slots: Box<[u32]>,
    words: Box<[(u32, Shape)]>,
    kinds: u64,
    drawn_words: Box<[bool]>,
    drawn_heaps: Box<[bool]>,
}

impl Resolved {
    pub(crate) fn new(slot_of: impl Fn(&str) -> Option<usize>, fields: &[Field], name: &str, super_kinds: u64) -> Self {
        let slots: Box<[u32]> = PROPERTY_NAMES.iter().map(|&p| slot_of(p).map_or(u32::MAX, |i| i as u32)).collect();
        let words = slots
            .iter()
            .map(|&slot| match fields.get(slot as usize) {
                Some(field) if field.dim == 1 => (field.word, field.shape),
                _ => (0, Shape::Map),
            })
            .collect();
        let own = KIND_NAMES.iter().position(|k| k.eq_ignore_ascii_case(name)).map_or(0, |bit| 1 << bit);
        let (drawn_words, drawn_heaps) = drawn_flags(&slots, fields);
        Self { slots, words, kinds: super_kinds | own, drawn_words, drawn_heaps }
    }

    pub(crate) fn kinds(&self) -> u64 {
        self.kinds
    }

    /// Whether a write at `span` may change a property on [`DRAWN`]. Where the answer is unclear (a span past the
    /// class's memory, an empty one) it is yes: an extra read of the actor, never a missed one.
    pub(crate) fn drawn(&self, span: Span) -> bool {
        let flagged = |flags: &[bool], at: u32, len: u32| len > 0 && flags.get(at as usize).is_none_or(|&flag| flag);
        (span.words == 0 && span.heaps == 0) || flagged(&self.drawn_words, span.word, span.words) || flagged(&self.drawn_heaps, span.heap, span.heaps)
    }
}

/// One flag per word and per heap slot of a class's memory, set over every element and member of a [`DRAWN`] property.
fn drawn_flags(slots: &[u32], fields: &[Field]) -> (Box<[bool]>, Box<[bool]>) {
    let spans = || fields.iter().map(Span::of);
    let mut words = vec![false; spans().map(|s| s.word + s.words).max().unwrap_or(0) as usize];
    let mut heaps = vec![false; spans().map(|s| s.heap + s.heaps).max().unwrap_or(0) as usize];
    for field in DRAWN.iter().filter_map(|&p| fields.get(slots[p as usize] as usize)) {
        let span = Span::of(field);
        if let Some(flags) = words.get_mut(span.word as usize..(span.word + span.words) as usize) {
            flags.fill(true);
        }
        if let Some(flags) = heaps.get_mut(span.heap as usize..(span.heap + span.heaps) as usize) {
            flags.fill(true);
        }
    }
    (words.into(), heaps.into())
}

pub trait PropertyName: Copy {
    fn slot_in(self, store: &ObjectStore, class: ClassId) -> Option<usize>;

    #[doc(hidden)]
    fn field_in(self, store: &ObjectStore, class: ClassId) -> Option<Field> {
        store.memory.get(store.class(class).mem).fields.get(self.slot_in(store, class)?).copied()
    }

    #[doc(hidden)]
    fn is_resolved(self) -> bool {
        false
    }

    #[doc(hidden)]
    fn prop(self) -> Option<Prop> {
        None
    }

    #[doc(hidden)]
    #[inline]
    fn word_in(self, store: &ObjectStore, class: ClassId) -> Option<(u32, Shape)> {
        self.field_in(store, class).filter(|f| f.dim == 1).map(|f| (f.word, f.shape))
    }
}

impl<T: AsRef<str> + ?Sized> PropertyName for &T {
    fn slot_in(self, store: &ObjectStore, class: ClassId) -> Option<usize> {
        store.property_index(class, self.as_ref())
    }
}

impl PropertyName for Prop {
    fn slot_in(self, store: &ObjectStore, class: ClassId) -> Option<usize> {
        match store.class(class).resolved.slots[self as usize] {
            u32::MAX => None,
            slot => Some(slot as usize),
        }
    }

    fn is_resolved(self) -> bool {
        true
    }

    fn prop(self) -> Option<Prop> {
        Some(self)
    }

    #[inline]
    fn word_in(self, store: &ObjectStore, class: ClassId) -> Option<(u32, Shape)> {
        match store.class(class).resolved.words[self as usize] {
            (_, Shape::Map) => None,
            found => Some(found),
        }
    }
}

/// Valid for the class it was resolved in and its subclasses only: in another class the slot is some other property, or
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(pub(crate) usize);

impl PropertyName for Slot {
    fn slot_in(self, _store: &ObjectStore, _class: ClassId) -> Option<usize> {
        Some(self.0)
    }
}

pub(crate) type SlotList = Arc<[(Box<str>, Option<Slot>)]>;

pub(crate) struct Ordered<'a> {
    recorded: Option<&'a [(Box<str>, Option<Slot>)]>,
    next: Cell<usize>,
    recording: RefCell<Vec<(Box<str>, Option<Slot>)>>,
}

impl<'a> Ordered<'a> {
    pub(crate) fn new(recorded: Option<&'a [(Box<str>, Option<Slot>)]>) -> Self {
        Self { recorded, next: Cell::new(0), recording: RefCell::new(Vec::new()) }
    }

    pub(crate) fn slot(&self, name: &str, resolve: impl FnOnce(&str) -> Option<Slot>) -> Option<Slot> {
        let i = self.next.get();
        self.next.set(i + 1);
        match self.recorded {
            Some(list) => match list.get(i) {
                Some((recorded, slot)) if **recorded == *name => *slot,
                _ => resolve(name),
            },
            None => {
                let slot = resolve(name);
                self.recording.borrow_mut().push((name.into(), slot));
                slot
            }
        }
    }

    pub(crate) fn into_recorded(self) -> Option<SlotList> {
        self.recorded.is_none().then(|| self.recording.into_inner().into())
    }
}

pub trait ClassName: Copy {
    fn is_base_of(self, store: &ObjectStore, class: ClassId) -> bool;
}

impl<T: AsRef<str> + ?Sized> ClassName for &T {
    fn is_base_of(self, store: &ObjectStore, class: ClassId) -> bool {
        store.class_is_named(class, self.as_ref())
    }
}

impl ClassName for NameId {
    fn is_base_of(self, store: &ObjectStore, class: ClassId) -> bool {
        store.class_has_name(class, self)
    }
}

impl ClassName for Kind {
    fn is_base_of(self, store: &ObjectStore, class: ClassId) -> bool {
        store.class(class).resolved.kinds & (1 << self as u32) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::{Field, Ordered, Resolved, Shape, Slot, Span};

    #[test]
    fn drawn_flags_cover_each_element_and_the_heap_of_a_drawn_property() {
        let field = |word, heap, dim, shape, words, heaps| Field { word, heap, dim, shape, words, heaps };
        let fields = [
            field(0, 0, 1, Shape::Float, 1, 0),
            field(1, 0, 3, Shape::Float, 1, 0),
            field(4, 0, 1, Shape::Str, 0, 1),
            field(4, 1, 1, Shape::Int, 1, 0),
        ];
        let names = ["TimerCounter", "Location", "Skins", "MoveTimer"];
        let slot_of = |name: &str| names.iter().position(|n| *n == name);
        let resolved = Resolved::new(slot_of, &fields, "Thing", 0);
        let scalar = |word, heap| resolved.drawn(Span::place(word, heap, (1, 0), 1));
        assert!(!scalar(0, 0));
        assert!(scalar(2, 0));
        assert!(scalar(3, 0));
        assert!(!scalar(4, 1));
        assert!(resolved.drawn(Span::place(4, 0, (0, 1), 1)));
        assert!(scalar(99, 0));
    }

    fn resolve(name: &str) -> Option<Slot> {
        name.strip_prefix('P').and_then(|i| i.parse().ok()).map(Slot)
    }

    #[test]
    fn records_then_replays_in_order() {
        let first = Ordered::new(None);
        let asked: Vec<_> = ["P3", "Q", "P7"].iter().map(|name| first.slot(name, resolve)).collect();
        assert_eq!(asked, [Some(Slot(3)), None, Some(Slot(7))]);
        let list = first.into_recorded().expect("recording");
        let replay = Ordered::new(Some(&list[..]));
        let asked: Vec<_> = ["P3", "Q", "P7"].iter().map(|name| replay.slot(name, |_| panic!("resolved"))).collect();
        assert_eq!(asked, [Some(Slot(3)), None, Some(Slot(7))]);
        assert!(replay.into_recorded().is_none());
    }

    #[test]
    fn other_order_or_more_names_resolve_by_name() {
        let first = Ordered::new(None);
        first.slot("P1", resolve);
        let list = first.into_recorded().expect("recording");
        let replay = Ordered::new(Some(&list[..]));
        assert_eq!(replay.slot("P2", resolve), Some(Slot(2)));
        assert_eq!(replay.slot("P5", resolve), Some(Slot(5)));
    }
}
