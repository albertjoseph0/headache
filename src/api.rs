//! HTTP: three JSON endpoints and the static frontend.

use std::{path::Path, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use tower_http::{
    compression::CompressionLayer, services::ServeDir, set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    config::{
        GAME_NAME, MAX_POINTS_PER_PROMPT, METRES_PER_POINT, PROMPTS_PER_DAY, SECONDS_PER_PROMPT,
        TRENCH_METRES,
    },
    game::{DiveState, Game, GameError, TurnView},
};

const PLAYER_COOKIE: &str = "headache_player";
const MAX_RAW_ANSWER_BYTES: usize = 400;

pub fn router(game: Arc<Game>, static_dir: impl AsRef<Path>) -> Router {
    let api = Router::new()
        .route("/today", get(today))
        .route("/descend", post(descend))
        .route("/answer", post(answer))
        .layer(DefaultBodyLimit::max(4 * 1024))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(game);

    Router::new()
        .nest("/api", api)
        .route("/healthz", get(|| async { "ok" }))
        .fallback_service(ServeDir::new(static_dir))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
                 connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

/// Rules the frontend renders, so they're defined once (in `config`).
#[derive(Serialize)]
struct Rules {
    name: &'static str,
    prompts: usize,
    seconds: i64,
    max_points: i64,
    metres_per_point: i64,
    trench: i64,
}

const RULES: Rules = Rules {
    name: GAME_NAME,
    prompts: PROMPTS_PER_DAY,
    seconds: SECONDS_PER_PROMPT,
    max_points: MAX_POINTS_PER_PROMPT,
    metres_per_point: METRES_PER_POINT,
    trench: TRENCH_METRES,
};

#[derive(Serialize)]
struct TodayResponse {
    rules: Rules,
    state: DiveState,
}

async fn today(
    State(game): State<Arc<Game>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Result<(CookieJar, Json<TodayResponse>), ApiError> {
    let (jar, player) = player(jar, &headers);
    let state = game.state(&player, now_ms())?;
    Ok((
        jar,
        Json(TodayResponse {
            rules: RULES,
            state,
        }),
    ))
}

async fn descend(
    State(game): State<Arc<Game>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Result<(CookieJar, Json<DiveState>), ApiError> {
    let (jar, player) = player(jar, &headers);
    Ok((jar, Json(game.descend(&player, now_ms())?)))
}

#[derive(Deserialize)]
struct AnswerRequest {
    day: String,
    idx: usize,
    answer: String,
}

#[derive(Serialize)]
struct AnswerResponse {
    turn: TurnView,
    state: DiveState,
}

async fn answer(
    State(game): State<Arc<Game>>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<AnswerRequest>,
) -> Result<(CookieJar, Json<AnswerResponse>), ApiError> {
    // Take the time before any slow work: the clock stops when the answer arrives.
    let now = now_ms();
    if request.answer.len() > MAX_RAW_ANSWER_BYTES {
        return Err(ApiError(StatusCode::BAD_REQUEST, "answer too long".into()));
    }
    let (jar, player) = player(jar, &headers);
    let (turn, state) = game
        .answer(&player, &request.day, request.idx, &request.answer, now)
        .await?;
    Ok((jar, Json(AnswerResponse { turn, state })))
}

/// Anonymous player id in a long-lived cookie. Clearing cookies starts a new diver;
/// that's an accepted limit of a no-login daily game.
fn player(jar: CookieJar, headers: &HeaderMap) -> (CookieJar, String) {
    if let Some(id) = jar
        .get(PLAYER_COOKIE)
        .and_then(|c| Uuid::parse_str(c.value()).ok())
    {
        return (jar, id.to_string());
    }
    let id = Uuid::new_v4().to_string();
    let https = headers
        .get("x-forwarded-proto")
        .is_some_and(|proto| proto.as_bytes().eq_ignore_ascii_case(b"https"));
    let cookie = Cookie::build((PLAYER_COOKIE, id.clone()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(https)
        .max_age(cookie::time::Duration::days(400))
        .build();
    (jar.add(cookie), id)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

struct ApiError(StatusCode, String);

impl From<GameError> for ApiError {
    fn from(error: GameError) -> Self {
        match error {
            GameError::NotOpen | GameError::UnknownDay => {
                ApiError(StatusCode::CONFLICT, error.to_string())
            }
            GameError::Store(error) => {
                tracing::error!(%error, "store error");
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "something went wrong".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}
