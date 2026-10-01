# Headache

A daily word game played underwater.

- 7 prompts a day, same for everyone.
- 25 seconds to name one thing.
- Rare answers score big. Obvious ones float.
- The pick that feels clever? The school thought of it too.
- Every point sinks you 10 metres. 700 is the trench.
- One dive per day. No second chances.

## Stack

| Part | Choice |
| --- | --- |
| Server | Rust: [axum](https://docs.rs/axum), tokio, SQLite ([rusqlite](https://docs.rs/rusqlite)), reqwest |
| Frontend | Plain HTML, CSS and JavaScript in `web/`, served by the Rust server. No framework, no build step. |
| Judging | [Jev](https://docs.typesafe.ai), TypeSafe's System One model, through the [Vercel AI Gateway](https://vercel.com/docs/ai-gateway/modalities/evaluation) (`POST /v1/evaluate`, model `typesafe-ai/jev`) |

## Run it

```sh
cp .env.example .env              # add your AI Gateway key
export $(grep -v '^#' .env | xargs)
cargo run
# → http://localhost:8080
```

Without `AI_GATEWAY_API_KEY` the server still runs, but every answer is counted unjudged (see [When Jev is down](#when-jev-is-down)).

Your dive is tied to an anonymous cookie. To play again on the same day while developing, clear the `headache_player` cookie or open a private window.

## How Jev is used

The server owns the game: the clock, the prompts, the scoring and what counts. Jev answers narrow questions about one answer at a time. All of them go out together in a single request of about 300–500 ms (`src/jev.rs`):

| Question | Type | Why |
| --- | --- | --- |
| `fits_prompt` | boolean | Does the answer satisfy the prompt? ("banana" is not a dinosaur.) Shorthand such as "tomato" for a soup is allowed. |
| `restates` | boolean | Did they just echo the category ("fruit" for "Name a fruit")? |
| `one_thing` | boolean | "Name ONE thing": "glue and honey" doesn't count, "fish and chips" does. |
| `commonness` | score, 5 levels | How many people would say this. Used as a prior before the school has answered. |
| `same_as` | choice | Which existing answer, if any, this one is another name for ("NYC" → "new york city", "t rex" → "tyrannosaurus rex"). |

Code turns those probabilities into decisions (`src/policy.rs`):

- Answers are rejected only on clear evidence (`fits_prompt < 0.35`, `restates > 0.5`, `one_thing < 0.35`), because a wrongly rejected answer can't be retried.
- Answers are merged only when Jev is fairly sure (`same_as ≥ 0.6`), because a wrong merge costs the player points.

### Scoring (`src/scoring.rs`)

```
share  = (others who said it + 20 × Jev prior) / (others who played + 20)
points = 0 at ≥25% share … 10 at ≤1% share   (log scale)
depth  = points × 10 m
```

The first diver of the day is scored on Jev's prior alone. By about 20 divers the real school carries as much weight as the prior, and after that it dominates. That's how "the school thought of it too" becomes true.

### Keeping model calls rare

Answers are normalized in code first, so case, accents, punctuation, leading articles and plurals don't matter (`src/normalize.rs`). Exact and plural matches against the school never reach Jev, and every verdict is cached per prompt. Only the first person to type a new answer pays for a model call; everyone after that gets a database lookup of about 1 ms.

### When Jev is down

If the gateway fails after two retries, the answer is counted as a fresh, middling-rarity answer and the verdict is not cached. Players aren't punished for an outage, and the next person to type that answer gets judged properly.

### Tuning the questions

`examples/jev_eval.rs` runs the real questions against labelled cases: valid and invalid answers, synonym pairs, and rarity ordering. Re-run it after changing any question wording or threshold:

```sh
AI_GATEWAY_API_KEY=... cargo run --example jev_eval
```

The current wording scores 45/45 on validity and 22/22 on matching, and its rarity priors come out in the expected order.

## Prompts

`data/prompts.txt` has one prompt per line. Each UTC day deals 7 prompts from a seeded shuffle, and every prompt is used once before any repeats. A day's prompts are saved to the database the first time they're dealt, so editing the file never changes a day that has already started.

Good prompts have a few obvious answers and a long tail of fair rare ones. Avoid prompts that need counting letters or precise numbers: Jev reads meaning, not character counts.

## API

| Endpoint | |
| --- | --- |
| `GET /api/today` | Rules plus your dive state for today |
| `POST /api/descend` | Reveal the next prompt and start its clock. Calling it again returns the same prompt and deadline. |
| `POST /api/answer` `{day, idx, answer}` | Judge the answer to the open prompt |
| `GET /healthz` | Liveness |

The server is the only clock that counts: an answer must arrive within 25 s plus 3 s grace of the prompt being revealed.

## Configuration

| Variable | Default |
| --- | --- |
| `AI_GATEWAY_API_KEY` | (required for judging) |
| `AI_GATEWAY_URL` | `https://ai-gateway.vercel.sh/v1/evaluate` |
| `JEV_MODEL` | `typesafe-ai/jev` |
| `PORT` | `8080` |
| `DATABASE_PATH` | `headache.db` |
| `STATIC_DIR` | `web` |
| `RUST_LOG` | `headache=info,tower_http=info` |

## Deploy

This is one long-running process with a SQLite file. Run it anywhere that gives a container a persistent disk (Fly.io, Railway, Render, a VPS) and mount the disk at `/data`:

```sh
docker build -t headache .
docker run -p 8080:8080 -v headache-data:/data -e AI_GATEWAY_API_KEY=... headache
```

Run a single instance, since SQLite is the shared state. Put it behind HTTPS; the player cookie is marked `Secure` when the proxy sends `X-Forwarded-Proto: https`. Vercel's serverless functions aren't a good fit for a stateful server, but the game calls Jev through the Vercel AI Gateway from any host.

## Tests

```sh
cargo test              # rules, scoring, normalization, store, full game flow (fake judge)
cargo clippy --all-targets
```

## Layout

```
src/
  main.rs       server startup and config
  api.rs        HTTP routes, player cookie, static files
  game.rs       the dive: reveal, clock, judge, score, results
  jev.rs        Jev questions and the gateway client
  policy.rs     thresholds that turn Jev's probabilities into decisions
  scoring.rs    rarity → points
  normalize.rs  answer clean-up and matching keys
  prompts.rs    prompt bank and daily schedule
  store.rs      SQLite
  day.rs        UTC days, dive numbers
  config.rs     game rules
data/prompts.txt
web/            index.html, style.css, app.js, fonts (Anton, SIL OFL)
examples/jev_eval.rs
```

## Known limits

- **Identity is an anonymous cookie.** Clearing cookies gives you another dive. Add accounts if that matters.
- **Early divers are scored mostly on Jev's estimate,** because the school is still small. It's calibrated against real answers, but it's still an estimate.
- **No rate limiting yet.** Add it at the proxy or with a tower layer before a public launch.
