//! The dive: reveal a prompt, start the clock, judge the answer, sink the diver.
//!
//! The server is the only clock that counts. A prompt's clock starts when the player
//! descends to it, and an answer is accepted until the deadline plus a small grace.

use std::sync::Arc;

use serde::Serialize;

use crate::{
    config::{GRACE_MS, METRES_PER_POINT, PROMPTS_PER_DAY, prompt_ms},
    day,
    jev::{Judge, JudgeInput},
    normalize, policy, prompts,
    scoring::{self, LEVEL_SHARES},
    store::{Cluster, Outcome, Store, Turn, Verdict},
};

/// Most-common answers offered to Jev when checking for a synonym. Rare answers that
/// fall outside this list stay separate, which costs nothing: both are rare anyway.
const MAX_CANDIDATES: usize = 200;
/// An unanswered prompt is closed this long after the answer window, leaving time for
/// an in-flight answer to be judged before anything else settles the turn.
const SETTLE_AFTER_MS: i64 = 20_000;
const SCHOOL_SHOWN: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("that prompt isn't open")]
    NotOpen,
    #[error("that dive is over")]
    UnknownDay,
    #[error(transparent)]
    Store(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Surface,
    Diving,
    Finished,
}

#[derive(Debug, Serialize)]
pub struct DiveState {
    pub day: String,
    pub dive_number: i64,
    pub server_now: i64,
    pub next_dive_at: i64,
    pub status: Status,
    pub points: i64,
    pub depth: i64,
    pub turns: Vec<TurnView>,
    pub current: Option<Current>,
    pub results: Option<Results>,
}

#[derive(Debug, Serialize)]
pub struct Current {
    pub idx: usize,
    pub prompt: String,
    pub deadline: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnView {
    pub idx: usize,
    pub prompt: String,
    pub answer: Option<String>,
    pub outcome: Outcome,
    pub reason: Option<String>,
    pub points: i64,
    /// Estimated share of the school that said the same thing, when counted.
    pub share: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Results {
    /// Finished dives today, including this one.
    pub divers: i64,
    /// How many of them ended shallower than this one.
    pub shallower: i64,
    /// Per prompt: the most common answers, plus yours.
    pub school: Vec<Vec<SchoolEntry>>,
}

#[derive(Debug, Serialize)]
pub struct SchoolEntry {
    pub answer: String,
    pub share: f64,
    pub yours: bool,
}

pub struct Game {
    store: Arc<Store>,
    judge: Arc<dyn Judge>,
}

impl Game {
    pub fn new(store: Arc<Store>, judge: Arc<dyn Judge>) -> Self {
        Self { store, judge }
    }

    pub fn state(&self, player: &str, now: i64) -> Result<DiveState, GameError> {
        let date = day::date_for(now);
        let day = day::key(date);
        let prompts = self.prompts_for(date)?;
        self.settle_expired(&day, player, now)?;
        self.view(&day, date, &prompts, player, now)
    }

    /// Reveal the next prompt and start its clock. Refreshing mid-prompt returns the same
    /// prompt with the same deadline: no second chances.
    pub fn descend(&self, player: &str, now: i64) -> Result<DiveState, GameError> {
        let date = day::date_for(now);
        let day = day::key(date);
        let prompts = self.prompts_for(date)?;
        self.settle_expired(&day, player, now)?;
        let turns = self.store.turns(&day, player)?;
        let has_open = turns.iter().any(|t| t.outcome.is_none());
        if !has_open && turns.len() < PROMPTS_PER_DAY {
            self.store.open_turn(&day, player, turns.len(), now)?;
        }
        self.view(&day, date, &prompts, player, now)
    }

    /// Judge an answer to the open prompt. `day` comes from the client so an answer
    /// typed just before midnight UTC still lands on the dive it belongs to.
    pub async fn answer(
        &self,
        player: &str,
        day_key: &str,
        idx: usize,
        raw: &str,
        now: i64,
    ) -> Result<(TurnView, DiveState), GameError> {
        let today = day::date_for(now);
        let date = [today, today.pred_opt().unwrap()]
            .into_iter()
            .find(|d| day::key(*d) == day_key)
            .ok_or(GameError::UnknownDay)?;
        let prompts = self
            .store
            .known_day(day_key)?
            .ok_or(GameError::UnknownDay)?;

        let turns = self.store.turns(day_key, player)?;
        let turn = turns
            .iter()
            .find(|t| t.idx == idx)
            .ok_or(GameError::NotOpen)?;
        if turn.outcome.is_none() {
            let (settled, joins) = self
                .judge_turn(day_key, &prompts[idx], turn, raw, now)
                .await?;
            // If something else settled the turn meanwhile, its result stands.
            self.store
                .settle_turn(day_key, player, &settled, joins.as_ref(), now)?;
        }

        let turns = self.store.turns(day_key, player)?;
        let turn = turns
            .iter()
            .find(|t| t.idx == idx)
            .ok_or(GameError::NotOpen)?;
        let view = turn_view(turn, &prompts).ok_or(GameError::NotOpen)?;
        Ok((view, self.view(day_key, date, &prompts, player, now)?))
    }

    /// The settled turn, and the school group it joins if it counted.
    async fn judge_turn(
        &self,
        day: &str,
        prompt: &str,
        turn: &Turn,
        raw: &str,
        now: i64,
    ) -> Result<(Turn, Option<Cluster>), GameError> {
        let display = normalize::display(raw);
        let key = normalize::key(raw);
        let late = now > turn.revealed_at + prompt_ms() + GRACE_MS;
        let mut settled = Turn {
            answer: (!display.is_empty()).then(|| display.clone()),
            ..turn.clone()
        };

        if late || key.is_empty() {
            settled.outcome = Some(Outcome::Timeout);
            settled.reason = Some(if late { "too late" } else { "no answer" }.into());
            return Ok((settled, None));
        }

        match self.verdict(day, turn.idx, prompt, &display, &key).await? {
            Verdict::Rejected { reason } => {
                settled.outcome = Some(Outcome::Rejected);
                settled.reason = Some(reason);
                Ok((settled, None))
            }
            Verdict::Counted { canonical, prior } => {
                let school = self.store.school(day, turn.idx)?;
                let total: i64 = school.iter().map(|c| c.count).sum();
                let same = school
                    .iter()
                    .find(|c| c.canonical == canonical)
                    .map_or(0, |c| c.count);
                let share = scoring::estimated_share(same, total, prior);
                settled.outcome = Some(Outcome::Counted);
                settled.canonical = Some(canonical.clone());
                settled.share = Some(share);
                settled.points = scoring::points_for_share(share);
                let joins = Cluster {
                    canonical,
                    label: display,
                    count: 1,
                    prior,
                };
                Ok((settled, Some(joins)))
            }
        }
    }

    /// Decide whether an answer counts and which group it joins: from cache, from an
    /// exact or plural match in the school, or from Jev.
    async fn verdict(
        &self,
        day: &str,
        idx: usize,
        prompt: &str,
        shown: &str,
        key: &str,
    ) -> Result<Verdict, GameError> {
        if let Some(cached) = self.store.verdict(day, idx, key)? {
            return Ok(cached);
        }

        let school = self.store.school(day, idx)?;
        let variants = normalize::variants(key);
        if let Some(cluster) = school
            .iter()
            .find(|c| c.canonical == key || variants.contains(&c.canonical))
        {
            let verdict = Verdict::Counted {
                canonical: cluster.canonical.clone(),
                prior: cluster.prior,
            };
            self.store.put_verdict(day, idx, key, &verdict)?;
            return Ok(verdict);
        }

        let input = JudgeInput {
            prompt: prompt.to_string(),
            answer: shown.to_string(),
            candidates: school
                .iter()
                .take(MAX_CANDIDATES)
                .map(|c| c.canonical.clone())
                .collect(),
        };
        let judgement = match self.judge.judge(&input).await {
            Ok(judgement) => judgement,
            Err(error) => {
                // Never punish a diver for our outage: count it as a fresh, middling
                // answer and don't cache, so the next diver who says it gets judged.
                tracing::warn!(%error, prompt, answer = shown, "Jev unavailable; counting answer unjudged");
                return Ok(Verdict::Counted {
                    canonical: key.to_string(),
                    prior: LEVEL_SHARES[2],
                });
            }
        };

        let verdict = match policy::rejection(&judgement) {
            Some(rejection) => Verdict::Rejected {
                reason: rejection.as_str().to_string(),
            },
            None => match policy::same_as(&judgement)
                .and_then(|canonical| school.iter().find(|c| c.canonical == canonical))
            {
                Some(cluster) => Verdict::Counted {
                    canonical: cluster.canonical.clone(),
                    prior: cluster.prior,
                },
                None => Verdict::Counted {
                    canonical: key.to_string(),
                    prior: scoring::prior_share(&judgement.commonness),
                },
            },
        };
        self.store.put_verdict(day, idx, key, &verdict)?;
        Ok(verdict)
    }

    fn prompts_for(&self, date: chrono::NaiveDate) -> Result<Vec<String>, GameError> {
        Ok(self.store.day_prompts(&day::key(date), || {
            prompts::for_day(prompts::bank(), day::index(date))
        })?)
    }

    /// Close a prompt whose clock ran out with no answer (closed tab, lost connection).
    fn settle_expired(&self, day: &str, player: &str, now: i64) -> Result<(), GameError> {
        for turn in self.store.turns(day, player)? {
            if turn.outcome.is_none()
                && now > turn.revealed_at + prompt_ms() + GRACE_MS + SETTLE_AFTER_MS
            {
                let timeout = Turn {
                    outcome: Some(Outcome::Timeout),
                    reason: Some("time ran out".into()),
                    ..turn
                };
                self.store.settle_turn(day, player, &timeout, None, now)?;
            }
        }
        Ok(())
    }

    fn view(
        &self,
        day: &str,
        date: chrono::NaiveDate,
        prompts: &[String],
        player: &str,
        now: i64,
    ) -> Result<DiveState, GameError> {
        let turns = self.store.turns(day, player)?;
        let views: Vec<TurnView> = turns.iter().filter_map(|t| turn_view(t, prompts)).collect();
        let points: i64 = views.iter().map(|t| t.points).sum();
        let current = turns.iter().find(|t| t.outcome.is_none()).map(|t| Current {
            idx: t.idx,
            prompt: prompts[t.idx].clone(),
            deadline: t.revealed_at + prompt_ms(),
        });
        let status = if views.len() == PROMPTS_PER_DAY {
            Status::Finished
        } else if turns.is_empty() {
            Status::Surface
        } else {
            Status::Diving
        };
        let results = match status {
            Status::Finished => Some(self.results(day, &turns, points)?),
            _ => None,
        };
        Ok(DiveState {
            day: day.to_string(),
            dive_number: day::dive_number(date),
            server_now: now,
            next_dive_at: day::next_dive_at_ms(now),
            status,
            points,
            depth: points * METRES_PER_POINT,
            turns: views,
            current,
            results,
        })
    }

    fn results(&self, day: &str, turns: &[Turn], points: i64) -> Result<Results, GameError> {
        let (divers, shallower) = self.store.rank(day, points, PROMPTS_PER_DAY)?;
        let mut school = Vec::new();
        for turn in turns {
            let clusters = self.store.school(day, turn.idx)?;
            let total = clusters.iter().map(|c| c.count).sum::<i64>().max(1) as f64;
            let mine = turn.canonical.as_deref();
            let mut entries: Vec<SchoolEntry> = clusters
                .iter()
                .take(SCHOOL_SHOWN)
                .map(|c| SchoolEntry {
                    answer: c.label.clone(),
                    share: c.count as f64 / total,
                    yours: Some(c.canonical.as_str()) == mine,
                })
                .collect();
            if let Some(c) = clusters
                .iter()
                .skip(SCHOOL_SHOWN)
                .find(|c| Some(c.canonical.as_str()) == mine)
            {
                entries.push(SchoolEntry {
                    answer: c.label.clone(),
                    share: c.count as f64 / total,
                    yours: true,
                });
            }
            school.push(entries);
        }
        Ok(Results {
            divers,
            shallower,
            school,
        })
    }
}

fn turn_view(turn: &Turn, prompts: &[String]) -> Option<TurnView> {
    Some(TurnView {
        idx: turn.idx,
        prompt: prompts.get(turn.idx)?.clone(),
        answer: turn.answer.clone(),
        outcome: turn.outcome?,
        reason: turn.reason.clone(),
        points: turn.points,
        share: turn.share,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jev::{BoxFuture, Judgement, NONE_OF_THESE};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers by rule, counting calls. `synonyms` maps an answer to the existing
    /// answer Jev should match it with.
    struct FakeJudge {
        calls: AtomicUsize,
        invalid: Vec<&'static str>,
        synonyms: Vec<(&'static str, &'static str)>,
        down: bool,
    }

    impl FakeJudge {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                invalid: vec![],
                synonyms: vec![],
                down: false,
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Judge for FakeJudge {
        fn judge<'a>(&'a self, input: &'a JudgeInput) -> BoxFuture<'a, anyhow::Result<Judgement>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = input.answer.to_lowercase();
            let result = if self.down {
                Err(anyhow::anyhow!("gateway down"))
            } else {
                let same = self
                    .synonyms
                    .iter()
                    .find(|(a, b)| *a == answer && input.candidates.iter().any(|c| c == b))
                    .map(|(_, b)| b.to_string());
                Ok(Judgement {
                    fits_prompt: if self.invalid.contains(&answer.as_str()) {
                        0.02
                    } else {
                        0.97
                    },
                    restates: 0.03,
                    one_thing: 0.98,
                    // Everything looks "rare" to the fake model.
                    commonness: vec![1.0, 0.0, 0.0, 0.0, 0.0],
                    same_as: (!input.candidates.is_empty())
                        .then(|| (same.unwrap_or(NONE_OF_THESE.into()), 0.95)),
                })
            };
            Box::pin(async move { result })
        }
    }

    const T0: i64 = 1_790_830_800_000; // 2026-10-01 05:00 UTC

    fn game(judge: FakeJudge) -> (Game, Arc<FakeJudge>) {
        let judge = Arc::new(judge);
        let store = Arc::new(Store::open_in_memory().unwrap());
        (Game::new(store, judge.clone()), judge)
    }

    async fn answer(game: &Game, player: &str, raw: &str, now: i64) -> TurnView {
        let state = game.descend(player, now).unwrap();
        let current = state.current.expect("an open prompt");
        game.answer(player, &state.day, current.idx, raw, now + 1_000)
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn a_full_dive_reaches_the_results() {
        let (game, _) = game(FakeJudge::new());
        assert_eq!(game.state("p", T0).unwrap().status, Status::Surface);

        let mut now = T0;
        for i in 0..PROMPTS_PER_DAY {
            let turn = answer(&game, "p", &format!("answer {i}"), now).await;
            assert_eq!(turn.outcome, Outcome::Counted);
            assert_eq!(turn.points, 10, "first diver, rare prior");
            now += 30_000;
        }

        let state = game.state("p", now).unwrap();
        assert_eq!(state.status, Status::Finished);
        assert_eq!(state.depth, 700);
        assert!(state.current.is_none());
        let results = state.results.unwrap();
        assert_eq!((results.divers, results.shallower), (1, 0));
        assert!(results.school[0][0].yours);
    }

    #[tokio::test]
    async fn the_school_makes_a_popular_answer_float() {
        let (game, _) = game(FakeJudge::new());
        for player in 0..30 {
            answer(&game, &format!("p{player}"), "Apple", T0).await;
        }
        let late_comer = answer(&game, "late", "apple", T0).await;
        assert_eq!(late_comer.outcome, Outcome::Counted);
        assert_eq!(late_comer.points, 0);
        assert!(late_comer.share.unwrap() > 0.5);
    }

    #[tokio::test]
    async fn repeat_answers_skip_the_model() {
        let (game, judge) = game(FakeJudge::new());
        answer(&game, "a", "Apple", T0).await;
        answer(&game, "b", "the apple!", T0).await;
        answer(&game, "c", "Apples", T0).await;
        assert_eq!(judge.calls(), 1);
    }

    #[tokio::test]
    async fn synonyms_join_the_existing_answer() {
        let (game, judge) = game(FakeJudge {
            synonyms: vec![("nyc", "new york city")],
            ..FakeJudge::new()
        });
        answer(&game, "a", "New York City", T0).await;
        answer(&game, "b", "NYC", T0).await;
        assert_eq!(judge.calls(), 2);
        let school = game.store.school(&day::key(day::date_for(T0)), 0).unwrap();
        assert_eq!(school.len(), 1);
        assert_eq!(
            (school[0].label.as_str(), school[0].count),
            ("New York City", 2)
        );
    }

    #[tokio::test]
    async fn invalid_answers_score_nothing_and_stay_out_of_the_school() {
        let (game, _) = game(FakeJudge {
            invalid: vec!["sofa"],
            ..FakeJudge::new()
        });
        let turn = answer(&game, "a", "Sofa", T0).await;
        assert_eq!(turn.outcome, Outcome::Rejected);
        assert_eq!(turn.points, 0);
        assert_eq!(
            turn.reason.as_deref(),
            Some(policy::Rejection::DoesNotFit.as_str())
        );
        assert!(
            game.store
                .school(&day::key(day::date_for(T0)), 0)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn late_and_blank_answers_time_out() {
        let (game, judge) = game(FakeJudge::new());
        let state = game.descend("a", T0).unwrap();
        let late = T0 + prompt_ms() + GRACE_MS + 1;
        let (turn, _) = game
            .answer("a", &state.day, 0, "apple", late)
            .await
            .unwrap();
        assert_eq!(turn.outcome, Outcome::Timeout);

        let blank = answer(&game, "b", "   ", T0).await;
        assert_eq!(blank.outcome, Outcome::Timeout);
        assert_eq!(judge.calls(), 0);
    }

    #[tokio::test]
    async fn refreshing_keeps_the_same_clock() {
        let (game, _) = game(FakeJudge::new());
        let first = game.descend("a", T0).unwrap().current.unwrap();
        let again = game.descend("a", T0 + 10_000).unwrap().current.unwrap();
        assert_eq!((first.idx, first.deadline), (again.idx, again.deadline));
    }

    #[tokio::test]
    async fn abandoned_prompts_time_out_and_the_dive_moves_on() {
        let (game, _) = game(FakeJudge::new());
        game.descend("a", T0).unwrap();
        let much_later = T0 + 120_000;
        let state = game.state("a", much_later).unwrap();
        assert_eq!(state.turns[0].outcome, Outcome::Timeout);
        let next = game.descend("a", much_later).unwrap().current.unwrap();
        assert_eq!(next.idx, 1);
    }

    #[tokio::test]
    async fn answering_twice_keeps_the_first_result() {
        let (game, _) = game(FakeJudge::new());
        let state = game.descend("a", T0).unwrap();
        let (first, _) = game
            .answer("a", &state.day, 0, "plum", T0 + 1_000)
            .await
            .unwrap();
        let (second, _) = game
            .answer("a", &state.day, 0, "cherry", T0 + 2_000)
            .await
            .unwrap();
        assert_eq!(second.answer.as_deref(), Some("plum"));
        assert_eq!(first.points, second.points);
        assert_eq!(game.store.school(&state.day, 0).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_outage_counts_the_answer_without_caching_it() {
        let (game, judge) = game(FakeJudge {
            down: true,
            ..FakeJudge::new()
        });
        let turn = answer(&game, "a", "plum", T0).await;
        assert_eq!(turn.outcome, Outcome::Counted);
        assert!(
            game.store
                .verdict(&day::key(day::date_for(T0)), 0, "plum")
                .unwrap()
                .is_none()
        );
        assert_eq!(judge.calls(), 1);
    }

    #[tokio::test]
    async fn unknown_days_and_closed_prompts_are_refused() {
        let (game, _) = game(FakeJudge::new());
        let state = game.descend("a", T0).unwrap();
        assert!(matches!(
            game.answer("a", "1999-01-01", 0, "x", T0).await,
            Err(GameError::UnknownDay)
        ));
        assert!(matches!(
            game.answer("a", &state.day, 3, "x", T0).await,
            Err(GameError::NotOpen)
        ));
    }
}
