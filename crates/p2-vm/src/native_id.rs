//! A native's group here is its route in [`crate::natives::call`]; `Operator` holds those [`crate::operators::operate`] answers.

macro_rules! natives {
    ($($group:ident: [$($name:ident),* $(,)?]),* $(,)?) => {
        #[expect(non_camel_case_types, reason = "named as the natives' declarations (`Add_IntInt`, `actorReachable`)")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum Native {
            $($($name,)*)*
        }

        pub(crate) enum NativeGroup {
            $($group),*
        }

        impl Native {
            #[cfg(test)]
            pub(crate) const ALL: &[Self] = &[$($(Self::$name,)*)*];

            pub(crate) fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($(stringify!($name) => Some(Self::$name),)*)*
                    _ => None,
                }
            }

            pub(crate) fn group(self) -> NativeGroup {
                match self {
                    $($(Self::$name)|* => NativeGroup::$group,)*
                }
            }
        }
    };
}

natives! {
    Operator: [
        Not_PreBool,
        EqualEqual_BoolBool,
        NotEqual_BoolBool,
        XorXor_BoolBool,
        AndAnd_BoolBool,
        OrOr_BoolBool,
        Subtract_PreInt,
        Complement_PreInt,
        Add_IntInt,
        Subtract_IntInt,
        Multiply_IntInt,
        Divide_IntInt,
        LessLess_IntInt,
        GreaterGreater_IntInt,
        GreaterGreaterGreater_IntInt,
        And_IntInt,
        Or_IntInt,
        Xor_IntInt,
        Less_IntInt,
        Greater_IntInt,
        LessEqual_IntInt,
        GreaterEqual_IntInt,
        EqualEqual_IntInt,
        NotEqual_IntInt,
        Min,
        Max,
        Clamp,
        Subtract_PreFloat,
        Add_FloatFloat,
        Subtract_FloatFloat,
        Multiply_FloatFloat,
        Divide_FloatFloat,
        Percent_FloatFloat,
        MultiplyMultiply_FloatFloat,
        Less_FloatFloat,
        Greater_FloatFloat,
        LessEqual_FloatFloat,
        GreaterEqual_FloatFloat,
        EqualEqual_FloatFloat,
        NotEqual_FloatFloat,
        ComplementEqual_FloatFloat,
        Abs,
        Sqrt,
        Square,
        Sin,
        Cos,
        Tan,
        Atan,
        Exp,
        Loge,
        FMin,
        FMax,
        FClamp,
        Lerp,
        FloatSpringInterp,
        VectorSpringInterp,
        EqualEqual_ObjectObject,
        NotEqual_ObjectObject,
        EqualEqual_NameName,
        NotEqual_NameName,
        Subtract_ColorColor,
        Add_ColorColor,
        Multiply_FloatColor,
        Multiply_ColorFloat,
        ClockwiseFrom_IntInt,
        Asin,
        Acos,
        Atan2,
        Log10,
        Ceil,
        Smerp,
        Subtract_PreVector,
        Add_VectorVector,
        Subtract_VectorVector,
        Multiply_VectorVector,
        Multiply_VectorFloat,
        Multiply_FloatVector,
        Divide_VectorFloat,
        Dot_VectorVector,
        Cross_VectorVector,
        EqualEqual_VectorVector,
        NotEqual_VectorVector,
        VSize,
        Normal,
        VLerp,
        MirrorVectorByNormal,
        GreaterGreater_VectorRotator,
        LessLess_VectorRotator,
        GetAxes,
        GetUnAxes,
        Invert,
        Normalize,
        RLerp,
        OrthoRotation,
        Add_RotatorRotator,
        Subtract_RotatorRotator,
        Multiply_RotatorFloat,
        Multiply_FloatRotator,
        Divide_RotatorFloat,
        EqualEqual_RotatorRotator,
        NotEqual_RotatorRotator,
        AddEqual_ByteByte,
        SubtractEqual_ByteByte,
        MultiplyEqual_ByteByte,
        DivideEqual_ByteByte,
        MultiplyEqual_VectorFloat,
        MultiplyEqual_VectorVector,
        DivideEqual_VectorFloat,
        AddEqual_VectorVector,
        SubtractEqual_VectorVector,
        MultiplyEqual_RotatorFloat,
        DivideEqual_RotatorFloat,
        AddEqual_RotatorRotator,
        SubtractEqual_RotatorRotator,
        AddEqual_IntInt,
        SubtractEqual_IntInt,
        MultiplyEqual_IntFloat,
        DivideEqual_IntFloat,
        AddEqual_FloatFloat,
        SubtractEqual_FloatFloat,
        MultiplyEqual_FloatFloat,
        DivideEqual_FloatFloat,
        ConcatEqual_StrStr,
        AtEqual_StrStr,
        AddAdd_PreInt,
        AddAdd_PreByte,
        AddAdd_Int,
        AddAdd_Byte,
        SubtractSubtract_PreInt,
        SubtractSubtract_PreByte,
        SubtractSubtract_Int,
        SubtractSubtract_Byte,
    ],
    Text: [
        SplitString,
        SplitStringSingle,
        Concat_StrStr,
        At_StrStr,
        EqualEqual_StrStr,
        NotEqual_StrStr,
        Less_StrStr,
        Greater_StrStr,
        LessEqual_StrStr,
        GreaterEqual_StrStr,
        ComplementEqual_StrStr,
        Len,
        Caps,
        Locs,
        Left,
        Right,
        Mid,
        Asc,
        Chr,
        InStr,
    ],
    Random: [
        RotRand,
        FRand,
        Rand,
        VRand,
    ],
    Reflect: [
        GetEnum,
        GetPropertyText,
        SetPropertyText,
        FindObject,
        DynamicLoadObject,
        IsA,
        ClassIsChildOf,
    ],
    State: [
        GotoState,
        GetStateName,
        IsInState,
        Sleep,
        FinishInterpolation,
        SetTimer,
        Disable,
        Enable,
        Initialize,
    ],
    Lifetime: [
        Destroy,
        Spawn,
    ],
    Placement: [
        SetLocation,
        Move,
        SetBase,
        SetCollision,
        SetCollisionSize,
        SetDrawScale,
        SetDrawScale3D,
        SetStaticMesh,
        SetDrawType,
        SetRotation,
        SetRelativeLocation,
        SetRelativeRotation,
        SetOwner,
        LinkMesh,
        AttachToBone,
        DetachFromBone,
        OnlyAffectPawns,
        SetPhysics,
        Warp,
        UnWarp,
    ],
    Sound: [
        PlaySound,
        PlayOwnedSound,
        PlayMusicAttenuate,
        PlayMusic,
        StopAllMusic,
        StopMusic,
        GetSoundDuration,
    ],
    Platform: [
        ConsoleCommand,
        IsDemoBuild,
        PlatformIsSteamDeck,
        PlatformIsMacOS,
        PlatformIsUnix,
        IsSteamBuild,
        SteamOwnsDLC,
        SteamOwnsGame,
        SteamViewStorePage,
        LaunchSocialNetwork,
        PlatformIsWindows,
        SteamGetWorkshopStatus,
        GetBuildDate,
        GetCountryCode,
        SaveConfig,
        StaticSaveConfig,
        ResetConfig,
        FileSize,
        GetEntryLevel,
        GetLocalURL,
        GetMillisecondsFrom,
        ClientTravel,
        Travel,
        Log,
        Warn,
    ],
    Sense: [
        AddController,
        RemoveController,
        MakeNoise,
        LineOfSightTo,
        PlayerCanSeeMe,
        Encompasses,
        PickTarget,
    ],
    Mesh: [
        GetBoneCoords,
        GetRenderBoundingSphere,
        GetBoneRotation,
        SetTwistLook,
    ],
    Presentation: [
        AbandonProjector,
        AttachProjector,
        DetachProjector,
        SetViewTarget,
        ShowSubtitle,
        GetTotalSceneTime,
    ],
    Canvas: [
        StrLen,
        TextSize,
        DrawText,
        DrawTextClipped,
        DrawTile,
        DrawTileClipped,
        DrawActor,
        DrawPortal,
        Draw3DLine,
    ],
    Trace: [
        Trace,
        FastTrace,
    ],
    Iterator: [
        AllActors,
        DynamicActors,
        AllObjects,
        TouchingActors,
        ChildActors,
        BasedActors,
        RadiusActors,
        CollidingActors,
        VisibleCollidingActors,
        VisibleActors,
        TraceActors,
    ],
    Ai: [
        CanSee,
        PickWallAdjust,
        FindPathTowardNearest,
        MoveSmooth,
        GetAssistedAimTarget,
    ],
    Move: [
        MoveTo,
        MoveToward,
        MoveToWithRadius,
        MoveTowardWithRadius,
        FinishRotation,
        WaitForLanding,
        InLatentExecution,
        ReachedDestination,
        EndClimbLadder,
        StopWaiting,
        FindStairRotation,
        WaitToSeeEnemy,
        EAdjustJump,
    ],
    Nav: [
        FindPathToward,
        FindPathTo,
        FindRandomDest,
        actorReachable,
        pointReachable,
        ClearPaths,
        BuildPathNodeLists,
        BuildHomeNodeLists,
    ],
    Karma: [
        KAddImpulse,
        KAddForce,
        KAddTorque,
        KWake,
        KDisableKarma,
        KSetMass,
        KGetMass,
        KSetFriction,
        KGetFriction,
        KSetRestitution,
        KGetRestitution,
        KSetDampingProps,
        KGetDampingProps,
        KSetImpactThreshold,
        KGetImpactThreshold,
        KSetStayUpright,
        KSetBlockKarma,
        KEnableCollision,
        KDisableCollision,
        KGetCOMPosition,
        KSetSkelVel,
        KFreezeRagdoll,
    ],
    Game: [
        SafeToGoIntoStasis,
        GetGameMap,
        PointToLineDist,
        VectorsInFuzz,
        ThickLineCylinderCollide,
        FireHurtRadius,
        ForceInit,
        ForceSpawn,
        SetProjectionNormal,
        SetMaxParticles,
        SetLiveParticleTimes,
        SpawnParticleLength,
    ],
    Util: [
        Floor,
        Localize,
        GetLanguage,
        GetDisplayLanguage,
        SetLanguage,
        InterpCurveEval,
        LoadFileToString,
        SaveStringToFile,
        FindFiles,
        OpenLog,
        Logf,
        CloseLog,
        Error,
        GetMapName,
        GetURLMap,
        DoesMapExist,
        GetAddressURL,
        GetDefaultURL,
        UpdateURL,
        GetNextInt,
        GetNextIntDesc,
        IsSteamDeck,
        IsWorkshopBuild,
        SteamUnlockAchievement,
        SteamLockAchievement,
        SteamGetAchievement,
        SteamGetStatFloat,
        SteamGetStatInt,
        SteamRequestStats,
        SteamSetStatInt,
        SteamSetStatFloat,
        SteamResetStats,
        SteamIndicateAchievementProgress,
        SteamPurgeWorkshop,
        CopyToClipboard,
        PasteFromClipboard,
        ResetKeyboard,
        GetNetworkNumber,
        GetPlayerNetworkAddress,
        ParseKillMessage,
        DeleteSave,
        GetMapFileName,
        LaunchDLC,
        GetNextSkin,
    ],
    HurtRadius: [
        HurtRadius,
        HurtRadiusEX,
    ],
    Anim: [
        PlayAnim,
        PlayAnimAt,
        LoopAnim,
        TweenAnim,
        IsAnimating,
        FinishAnim,
        HasAnim,
        StopAnimating,
        FreezeAnimAt,
        IsTweening,
        AnimIsInGroup,
        LinkSkelAnim,
        GetDefaultAnim,
        AnimBlendParams,
        AnimBlendToAlpha,
        GetAnimParams,
        EnableChannelNotify,
        SetBoneScale,
    ],
    Actor: [
        AttachActor,
        DetachActor,
        Kill,
        SpawnParticle,
        Pling,
        GetCameraLocation,
        ScreenToWorld,
        PlayFlashbangSound,
        AddSpawnProperty,
        GetCacheEntry,
        MoveCacheEntry,
        DrawColoredText,
        ReplaceTexture,
        GraphData,
    ],
    Bone: [
        SetBoneRotation,
        SetBoneDirection,
        SetBoneLocation,
    ],
    Lod: [
        SetLodStaticMesh,
        ClearLodStaticMesh,
        DeleteLodStaticMesh,
    ],
    KarmaState: [
        KSetCOMOffset,
        KGetCOMOffset,
        KSetInertiaTensor,
        KGetInertiaTensor,
        KGetConstraintForce,
        KGetConstraintTorque,
        KUpdateConstraintParams,
    ],
    Movie: [
        Play,
        Pause,
        IsPlaying,
        IsPaused,
        GetWidth,
        GetHeight,
        InitializeMovie,
        StopNow,
        StopAtEnd,
    ],
}

impl Native {
    pub(crate) fn is_fast(self) -> bool {
        matches!(
            self,
            Self::Less_IntInt
                | Self::Greater_IntInt
                | Self::LessEqual_IntInt
                | Self::GreaterEqual_IntInt
                | Self::EqualEqual_IntInt
                | Self::NotEqual_IntInt
                | Self::Add_IntInt
                | Self::Subtract_IntInt
                | Self::Multiply_IntInt
                | Self::Divide_IntInt
                | Self::Subtract_PreInt
                | Self::Less_FloatFloat
                | Self::Greater_FloatFloat
                | Self::LessEqual_FloatFloat
                | Self::GreaterEqual_FloatFloat
                | Self::EqualEqual_FloatFloat
                | Self::NotEqual_FloatFloat
                | Self::Add_FloatFloat
                | Self::Subtract_FloatFloat
                | Self::Multiply_FloatFloat
                | Self::Divide_FloatFloat
                | Self::Subtract_PreFloat
                | Self::Not_PreBool
                | Self::AndAnd_BoolBool
                | Self::OrOr_BoolBool
                | Self::EqualEqual_BoolBool
                | Self::NotEqual_BoolBool
                | Self::XorXor_BoolBool
                | Self::EqualEqual_ObjectObject
                | Self::NotEqual_ObjectObject
                | Self::EqualEqual_NameName
                | Self::NotEqual_NameName
                | Self::VSize
                | Self::Add_VectorVector
                | Self::Subtract_VectorVector
                | Self::Multiply_VectorFloat
                | Self::Multiply_FloatVector
                | Self::AddEqual_FloatFloat
                | Self::SubtractEqual_FloatFloat
                | Self::MultiplyEqual_FloatFloat
                | Self::DivideEqual_FloatFloat
                | Self::AddEqual_IntInt
                | Self::SubtractEqual_IntInt
                | Self::AddAdd_PreInt
                | Self::AddAdd_PreByte
                | Self::AddAdd_Int
                | Self::AddAdd_Byte
                | Self::SubtractSubtract_PreInt
                | Self::SubtractSubtract_PreByte
                | Self::SubtractSubtract_Int
                | Self::SubtractSubtract_Byte
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_to_their_native() {
        assert_eq!(Native::from_name("Add_IntInt"), Some(Native::Add_IntInt));
        assert_eq!(Native::from_name("actorReachable"), Some(Native::actorReachable));
        assert_eq!(Native::from_name("AddEqual_IntInt"), Some(Native::AddEqual_IntInt));
        assert_eq!(Native::from_name("NoSuchNative"), None);
    }

    #[test]
    fn iterator_names_resolve_to_their_native() {
        let names = [
            "AllActors",
            "DynamicActors",
            "AllObjects",
            "TouchingActors",
            "ChildActors",
            "BasedActors",
            "RadiusActors",
            "CollidingActors",
            "VisibleCollidingActors",
            "VisibleActors",
            "TraceActors",
        ];
        for name in names {
            let native = Native::from_name(name);
            assert!(native.is_some_and(|native| matches!(native.group(), NativeGroup::Iterator) && format!("{native:?}") == name), "{name}");
        }
    }
}
