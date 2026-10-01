//! Game rules. Everything a player can see in "How to play" lives here.

pub const GAME_NAME: &str = "Headache";

pub const PROMPTS_PER_DAY: usize = 7;
pub const SECONDS_PER_PROMPT: i64 = 25;
/// Extra time the server accepts past the visible deadline, to absorb network latency.
pub const GRACE_MS: i64 = 3_000;

pub const MAX_POINTS_PER_PROMPT: i64 = 10;
pub const METRES_PER_POINT: i64 = 10;
/// A perfect dive: every prompt answered with something almost nobody else said.
pub const TRENCH_METRES: i64 = PROMPTS_PER_DAY as i64 * MAX_POINTS_PER_PROMPT * METRES_PER_POINT;

pub const MAX_ANSWER_CHARS: usize = 60;

pub fn prompt_ms() -> i64 {
    SECONDS_PER_PROMPT * 1_000
}
