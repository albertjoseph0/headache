//! Jev (TypeSafe's System One model) via the Vercel AI Gateway.
//!
//! One request per new answer asks every judgment we might need, in parallel:
//!
//! | id               | type    | used for                                          |
//! |------------------|---------|---------------------------------------------------|
//! | `fits_prompt`    | boolean | does the answer satisfy the prompt at all?        |
//! | `restates`       | boolean | did they just echo the category ("fruit")?        |
//! | `one_thing`      | boolean | "name ONE thing", not a list                      |
//! | `commonness`     | score   | prior for rarity before the school has answered   |
//! | `same_as`        | choice  | which existing answer (if any) this is a synonym of |
//!
//! Code owns the policy: thresholds, scoring, and what gets counted.

use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Value, json};

pub const NONE_OF_THESE: &str = "none_of_these";

pub const COMMONNESS_LEVELS: [&str; 5] = [
    "Almost nobody would say it: obscure, surprising, or very specific",
    "A few people would say it: unusual but sensible",
    "Some people would say it: reasonable, but not among the first answers that come to mind",
    "Many people would say it: one of the usual answers",
    "Most people would say it: the obvious answer people give first",
];

/// What we ask Jev about one answer.
#[derive(Debug, Clone)]
pub struct JudgeInput {
    pub prompt: String,
    pub answer: String,
    /// Existing answers in the school this one might be a synonym of.
    pub candidates: Vec<String>,
}

/// Jev's raw judgments. Probabilities, not decisions.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    pub fits_prompt: f64,
    pub restates: f64,
    pub one_thing: f64,
    /// Probability per commonness level, rarest first.
    pub commonness: Vec<f64>,
    /// Best candidate and its probability, if candidates were offered.
    pub same_as: Option<(String, f64)>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Judge: Send + Sync {
    fn judge<'a>(&'a self, input: &'a JudgeInput) -> BoxFuture<'a, anyhow::Result<Judgement>>;
}

pub fn request_body(model: &str, input: &JudgeInput) -> Value {
    let mut questions = json!({
        "fits_prompt": {
            "type": "boolean",
            "instructions": "Does `answer` satisfy `prompt`?",
            "criteria": {
                "true": "`answer` names a real thing that meets every condition in `prompt`. Misspellings, nicknames, brand names, informal names, and shorthand that leaves out the category word (like \"tomato\" for a soup) are fine.",
                "false": "`answer` breaks a condition of `prompt`, or is gibberish, a joke, a refusal, or an instruction instead of an answer."
            }
        },
        "restates": {
            "type": "boolean",
            "instructions": "Does `answer` just repeat the category asked for in `prompt` instead of naming a specific example of it?"
        },
        "one_thing": {
            "type": "boolean",
            "instructions": "Does `answer` name a single thing rather than list two or more separate things?",
            "criteria": {
                "true": "One thing, even if its name has several words, like \"macaroni and cheese\" or \"fish and chips\".",
                "false": "Two or more separate things joined by \"and\", \"or\", commas or slashes, like \"apples and oranges\"."
            }
        },
        "commonness": {
            "type": "score",
            "instructions": "If a large crowd each had 25 seconds to answer `prompt`, how many of them would say `answer` or the same thing under another name?",
            "criteria": COMMONNESS_LEVELS,
        }
    });

    if !input.candidates.is_empty() {
        let mut criteria = serde_json::Map::new();
        for candidate in &input.candidates {
            criteria.insert(candidate.clone(), Value::Null);
        }
        criteria.insert(
            NONE_OF_THESE.into(),
            json!("`answer` names something different from every listed answer"),
        );
        questions["same_as"] = json!({
            "type": "choice",
            "instructions": {
                "question": "As answers to `prompt`, which listed answer names the same thing as `answer`?",
                "same_thing": "A spelling variant, plural, abbreviation, nickname, translation, other name, or shorthand that leaves out the category word (like \"tomato\" for \"tomato soup\" when naming a soup) for exactly the same thing.",
                "not_the_same_thing": "A related, similar, broader, or narrower thing, such as a variety, a sibling in the same category, or a product made from it."
            },
            "criteria": criteria,
        });
    }

    json!({
        "model": model,
        "state": { "prompt": input.prompt, "answer": input.answer },
        "questions": questions,
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Boolean {
        probability: f64,
    },
    Score {
        #[serde(default)]
        probabilities: HashMap<String, f64>,
    },
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: HashMap<String, f64>,
    },
}

#[derive(Debug, Deserialize)]
struct EvaluateResponse {
    answers: HashMap<String, Answer>,
}

pub fn parse_response(body: &str) -> anyhow::Result<Judgement> {
    let response: EvaluateResponse =
        serde_json::from_str(body).context("unexpected evaluate response")?;
    let answers = &response.answers;

    let boolean = |id: &str| match answers.get(id) {
        Some(Answer::Boolean { probability }) => Ok(*probability),
        other => bail!("missing boolean answer {id}: {other:?}"),
    };

    let commonness = match answers.get("commonness") {
        Some(Answer::Score { probabilities }) => (0..COMMONNESS_LEVELS.len())
            .map(|level| {
                probabilities
                    .get(&level.to_string())
                    .copied()
                    .unwrap_or(0.0)
            })
            .collect(),
        other => bail!("missing score answer commonness: {other:?}"),
    };

    let same_as = match answers.get("same_as") {
        Some(Answer::Choice {
            choice,
            probabilities,
        }) => {
            let p = probabilities.get(choice).copied().unwrap_or(1.0);
            Some((choice.clone(), p))
        }
        Some(other) => bail!("unexpected same_as answer: {other:?}"),
        None => None,
    };

    Ok(Judgement {
        fits_prompt: boolean("fits_prompt")?,
        restates: boolean("restates")?,
        one_thing: boolean("one_thing")?,
        commonness,
        same_as,
    })
}

/// Calls `POST {gateway}/v1/evaluate`.
pub struct GatewayJudge {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
    model: String,
}

impl GatewayJudge {
    pub fn new(url: String, api_key: Option<String>, model: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()?;
        Ok(Self {
            http,
            url,
            api_key,
            model,
        })
    }

    async fn call(&self, input: &JudgeInput) -> anyhow::Result<Judgement> {
        let body = request_body(&self.model, input);
        let mut last_error = None;

        // Two retries with short backoff: a player is waiting on this.
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * 3u64.pow(attempt - 1))).await;
            }
            let mut request = self.http.post(&self.url).json(&body);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        return parse_response(&text);
                    }
                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    last_error = Some(anyhow::anyhow!("gateway returned {status}: {text}"));
                    if !retryable {
                        break;
                    }
                }
                Err(error) => last_error = Some(error.into()),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("evaluate failed")))
    }
}

impl Judge for GatewayJudge {
    fn judge<'a>(&'a self, input: &'a JudgeInput) -> BoxFuture<'a, anyhow::Result<Judgement>> {
        Box::pin(self.call(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(candidates: &[&str]) -> JudgeInput {
        JudgeInput {
            prompt: "Name a fruit that's red".into(),
            answer: "pomegranate".into(),
            candidates: candidates.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn same_as_is_only_asked_when_there_are_candidates() {
        let body = request_body("typesafe-ai/jev", &input(&[]));
        assert!(body["questions"].get("same_as").is_none());
        assert_eq!(body["state"]["answer"], "pomegranate");

        let body = request_body("typesafe-ai/jev", &input(&["strawberry", "cherry"]));
        let criteria = body["questions"]["same_as"]["criteria"]
            .as_object()
            .unwrap();
        assert_eq!(criteria.len(), 3);
        assert!(criteria.contains_key(NONE_OF_THESE));
        assert!(criteria["strawberry"].is_null());
    }

    #[test]
    fn parses_gateway_answers() {
        let body = r#"{
            "answers": {
                "fits_prompt": {"type": "boolean", "probability": 0.95},
                "restates": {"type": "boolean", "probability": 0.02},
                "one_thing": {"type": "boolean", "probability": 0.98},
                "commonness": {"type": "score", "score": 2.23,
                    "probabilities": {"0": 0, "1": 0.14, "2": 0.5, "3": 0.34, "4": 0.02}},
                "same_as": {"type": "choice", "choice": "none_of_these",
                    "probabilities": {"none_of_these": 0.89, "cherry": 0.11}}
            },
            "model": "typesafe-ai/jev",
            "usage": {"inputTokens": 451, "outputTokens": 103}
        }"#;
        let judgement = parse_response(body).unwrap();
        assert_eq!(judgement.fits_prompt, 0.95);
        assert_eq!(judgement.commonness, vec![0.0, 0.14, 0.5, 0.34, 0.02]);
        assert_eq!(judgement.same_as, Some((NONE_OF_THESE.to_string(), 0.89)));
    }

    #[test]
    fn missing_answers_are_errors() {
        assert!(parse_response(r#"{"answers": {}}"#).is_err());
    }
}
