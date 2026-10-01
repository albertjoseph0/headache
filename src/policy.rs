//! Decisions made from Jev's probabilities. Thresholds were checked against
//! `cargo run --example jev_eval`; re-run it after changing any question or number here.

use crate::jev::{Judgement, NONE_OF_THESE};

/// Rejecting a fair answer hurts more than letting a weak one through (there are no
/// second chances), so validity thresholds lean lenient.
pub const FITS_MIN: f64 = 0.35;
pub const RESTATES_MAX: f64 = 0.5;
pub const ONE_THING_MIN: f64 = 0.35;
/// Merging two different answers costs a player points, so matching leans strict.
pub const SAME_AS_MIN: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    DoesNotFit,
    JustTheCategory,
    MoreThanOneThing,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Rejection::DoesNotFit => "doesn't fit the prompt",
            Rejection::JustTheCategory => "that's the category, not an example",
            Rejection::MoreThanOneThing => "one thing only",
        }
    }
}

pub fn rejection(j: &Judgement) -> Option<Rejection> {
    if j.fits_prompt < FITS_MIN {
        Some(Rejection::DoesNotFit)
    } else if j.restates > RESTATES_MAX {
        Some(Rejection::JustTheCategory)
    } else if j.one_thing < ONE_THING_MIN {
        Some(Rejection::MoreThanOneThing)
    } else {
        None
    }
}

/// The existing answer this one should be counted with, if Jev is sure enough.
pub fn same_as(j: &Judgement) -> Option<String> {
    match &j.same_as {
        Some((choice, p)) if choice != NONE_OF_THESE && *p >= SAME_AS_MIN => Some(choice.clone()),
        _ => None,
    }
}
