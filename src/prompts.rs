//! The prompt bank and the daily schedule.
//!
//! The bank is a plain text file, one prompt per line. Each day takes 7 prompts from a
//! seeded shuffle of the bank, so the schedule is the same on every server. The store
//! saves each day's prompts the first time they are dealt, so editing the bank never
//! changes a day that has already started.

use std::sync::OnceLock;

use crate::config::PROMPTS_PER_DAY;

const BANK_SRC: &str = include_str!("../data/prompts.txt");
const SCHEDULE_SEED: u64 = 0x5EA_F100_D1CE;

pub fn bank() -> &'static [&'static str] {
    static BANK: OnceLock<Vec<&'static str>> = OnceLock::new();
    BANK.get_or_init(|| parse(BANK_SRC))
}

fn parse(src: &str) -> Vec<&str> {
    src.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The prompts for a day, by days-since-epoch. Every prompt is used once per cycle
/// through the bank before any repeats.
pub fn for_day(bank: &[&str], day_index: i64) -> Vec<String> {
    let days_per_cycle = (bank.len() / PROMPTS_PER_DAY).max(1) as i64;
    let cycle = day_index.div_euclid(days_per_cycle) as u64;
    let slot = day_index.rem_euclid(days_per_cycle) as usize;

    let mut order: Vec<usize> = (0..bank.len()).collect();
    let mut rng = SplitMix64(SCHEDULE_SEED ^ cycle.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for i in (1..order.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }

    order
        .iter()
        .cycle()
        .skip(slot * PROMPTS_PER_DAY)
        .take(PROMPTS_PER_DAY)
        .map(|&i| bank[i].to_string())
        .collect()
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn bank_has_at_least_two_weeks_of_unique_prompts() {
        let bank = bank();
        let unique: HashSet<_> = bank.iter().map(|p| p.to_lowercase()).collect();
        assert_eq!(
            unique.len(),
            bank.len(),
            "duplicate prompt in data/prompts.txt"
        );
        assert!(bank.len() >= PROMPTS_PER_DAY * 14);
    }

    #[test]
    fn same_day_same_prompts() {
        assert_eq!(for_day(bank(), 20_000), for_day(bank(), 20_000));
        assert_ne!(for_day(bank(), 20_000), for_day(bank(), 20_001));
    }

    #[test]
    fn no_repeats_within_a_cycle() {
        let bank = bank();
        let days_per_cycle = (bank.len() / PROMPTS_PER_DAY) as i64;
        let start = 20_000 - 20_000 % days_per_cycle;
        let mut seen = HashSet::new();
        for day in start..start + days_per_cycle {
            for prompt in for_day(bank, day) {
                assert!(seen.insert(prompt.clone()), "{prompt} repeated");
            }
        }
    }
}
