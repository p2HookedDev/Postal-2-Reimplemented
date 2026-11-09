use crate::bolton::BoltonHook;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptHook {
    /// Ours: the hit is kept for the corpse's throw and pushes a loose bolt-on.
    TakeDamage,
    /// Ours: bullet holes on movers and Karma props, which retail ammo leaves bare.
    ProcessTraceHit,
    /// Ours: bolt-ons are thrown off as bodies; retail keeps them on.
    Bolton(BoltonHook),
}

/// Every script function the engine runs code around: its name, fewest parameters and hook.
const HOOKS: [(&str, usize, ScriptHook); 6] = [
    ("TakeDamage", 5, ScriptHook::TakeDamage),
    ("ProcessTraceHit", 4, ScriptHook::ProcessTraceHit),
    ("DropBoltons", 0, ScriptHook::Bolton(BoltonHook::DropBoltons)),
    ("DestroyHeadBoltons", 0, ScriptHook::Bolton(BoltonHook::DestroyHeadBoltons)),
    ("PopOffHead", 0, ScriptHook::Bolton(BoltonHook::PopOffHead)),
    ("PinataStyleExplodeEffects", 0, ScriptHook::Bolton(BoltonHook::PinataStyleExplodeEffects)),
];

impl ScriptHook {
    pub(crate) fn of(function: &str, params: usize) -> Option<Self> {
        HOOKS.iter().find(|&&(name, least, _)| params >= least && name.eq_ignore_ascii_case(function)).map(|&(_, _, hook)| hook)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_by_name_and_parameters() {
        assert_eq!(ScriptHook::of("dropboltons", 0), Some(ScriptHook::Bolton(BoltonHook::DropBoltons)));
        assert_eq!(ScriptHook::of("TakeDamage", 5), Some(ScriptHook::TakeDamage));
        assert_eq!(ScriptHook::of("TakeDamage", 4), None);
        assert_eq!(ScriptHook::of("ProcessTraceHit", 4), Some(ScriptHook::ProcessTraceHit));
        assert_eq!(ScriptHook::of("Touch", 1), None);
    }
}
