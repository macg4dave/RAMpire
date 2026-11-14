//! What a reading means for a person at a glance: good, a warning, bad,
//! moving and so on. The readings name a tone; each theme gives it a colour
//! and each icon set a symbol, so nothing is shown by colour alone.

/// The meaning of a figure or an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Ordinary text.
    Normal,
    /// A figure itself.
    Value,
    /// Secondary text.
    Dim,
    /// Done, healthy, steady.
    Good,
    /// Needs attention, held back, partial.
    Warn,
    /// Failed, refused, latched.
    Bad,
    /// Moving: a grant, a resize under way.
    Info,
    /// Something unusual worth noticing.
    Accent,
}

/// The tone of a share of something used: green, then yellow from 70 %,
/// red from 90 %.
pub fn load(fraction: f64) -> Tone {
    if fraction >= 0.9 {
        Tone::Bad
    } else if fraction >= 0.7 {
        Tone::Warn
    } else {
        Tone::Good
    }
}

/// The tone of a request's outcome as the journal names it.
pub fn outcome(outcome: &str) -> Tone {
    match outcome {
        "Completed" => Tone::Good,
        "Partial" | "PoolEmpty" => Tone::Warn,
        _ => Tone::Bad,
    }
}

/// The tone of host memory available against the controller's floor.
pub fn floor(available: u64, floor: u64) -> Tone {
    if available < floor {
        Tone::Bad
    } else if available < floor + floor / 4 {
        Tone::Warn
    } else {
        Tone::Good
    }
}
