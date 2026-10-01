//! Rarity → points.
//!
//! An answer's worth is the share of the school (everyone else who played today's
//! prompt) that said the same thing. Early in the day there is no school yet, so we
//! start from Jev's estimate of how common the answer is and let real answers take
//! over as they arrive.

use crate::config::MAX_POINTS_PER_PROMPT;

/// Share of players assumed to give an answer at each of Jev's commonness levels,
/// rarest first. These are tuning knobs, not model outputs.
pub const LEVEL_SHARES: [f64; 5] = [0.003, 0.012, 0.04, 0.12, 0.30];

/// How many real answers the prior is worth. With 20 others in the school, Jev's
/// estimate and the real counts carry equal weight.
pub const PRIOR_WEIGHT: f64 = 20.0;

/// At or above this share an answer floats: zero points.
pub const FLOAT_SHARE: f64 = 0.25;
/// At or below this share an answer scores the maximum.
pub const DEEP_SHARE: f64 = 0.01;

/// Expected share from Jev's probabilities over the commonness levels.
pub fn prior_share(level_probabilities: &[f64]) -> f64 {
    let total: f64 = level_probabilities.iter().sum();
    if level_probabilities.len() != LEVEL_SHARES.len() || total <= 0.0 {
        return LEVEL_SHARES[2];
    }
    level_probabilities
        .iter()
        .zip(LEVEL_SHARES)
        .map(|(p, share)| p * share)
        .sum::<f64>()
        / total
}

/// Share of the school that gave this answer, counting only *other* players so the
/// first person to dive isn't punished for matching themselves.
pub fn estimated_share(same_by_others: i64, total_others: i64, prior: f64) -> f64 {
    (same_by_others as f64 + PRIOR_WEIGHT * prior) / (total_others as f64 + PRIOR_WEIGHT)
}

/// Log-scale between FLOAT_SHARE (0 points) and DEEP_SHARE (max points).
pub fn points_for_share(share: f64) -> i64 {
    let share = share.clamp(1e-9, 1.0);
    let depth = (share / FLOAT_SHARE).ln() / (DEEP_SHARE / FLOAT_SHARE).ln();
    (depth.clamp(0.0, 1.0) * MAX_POINTS_PER_PROMPT as f64).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obvious_answers_float_and_rare_ones_sink() {
        assert_eq!(points_for_share(0.40), 0);
        assert_eq!(points_for_share(FLOAT_SHARE), 0);
        assert_eq!(points_for_share(DEEP_SHARE), MAX_POINTS_PER_PROMPT);
        assert_eq!(points_for_share(0.0001), MAX_POINTS_PER_PROMPT);
        assert_eq!(points_for_share(0.05), 5);
    }

    #[test]
    fn prior_is_probability_weighted() {
        assert_eq!(prior_share(&[1.0, 0.0, 0.0, 0.0, 0.0]), LEVEL_SHARES[0]);
        assert_eq!(prior_share(&[0.0, 0.0, 0.0, 0.0, 1.0]), LEVEL_SHARES[4]);
        let mixed = prior_share(&[0.0, 0.0, 0.5, 0.5, 0.0]);
        assert!((mixed - 0.08).abs() < 1e-9);
        assert_eq!(prior_share(&[]), LEVEL_SHARES[2]);
    }

    #[test]
    fn first_diver_is_scored_on_the_prior() {
        assert_eq!(estimated_share(0, 0, 0.003), 0.003);
    }

    #[test]
    fn the_school_overrides_a_wrong_prior() {
        // Jev thought it was rare, but 400 of 1000 divers said it.
        let share = estimated_share(400, 1000, 0.003);
        assert!(share > 0.38);
        assert_eq!(points_for_share(share), 0);
        // Jev thought it was obvious, but nobody else said it.
        let share = estimated_share(0, 1000, 0.30);
        assert!(points_for_share(share) >= 8);
    }
}
