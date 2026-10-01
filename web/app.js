// Headache: the client. The server owns the clock and the scoring; this file only
// renders state and sends answers. Other divers' answers are user text, so everything
// is rendered with textContent, never innerHTML.

const $ = (selector) => document.querySelector(selector);
const SCREENS = ["loading", "surface", "prompt", "reveal", "results", "error"];
const URGENT_MS = 5_000;

let rules = null;
let state = null;
let clockOffset = 0; // server time minus local time
let timerFrame = 0;
let submitting = false;
let countdownTimer = 0;

// ── Server ──────────────────────────────────────────────────────────────────

async function call(path, body) {
  const init = body === undefined
    ? {}
    : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
  const response = await fetch(`/api/${path}`, init);
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(data.error || `HTTP ${response.status}`);
    error.status = response.status;
    throw error;
  }
  return data;
}

function adopt(next) {
  state = next;
  clockOffset = next.server_now - Date.now();
  setDepth(next.depth);
}

const serverNow = () => Date.now() + clockOffset;

async function load() {
  show("loading");
  try {
    const today = await call("today");
    rules = today.rules;
    adopt(today.state);
    route();
  } catch (error) {
    fail(error);
  }
}

// ── Screens ─────────────────────────────────────────────────────────────────

function show(id) {
  for (const screen of SCREENS) $(`#${screen}`).hidden = screen !== id;
  window.scrollTo({ top: 0 });
}

function route() {
  if (state.status === "finished") return showResults();
  if (state.current) return showPrompt();
  if (state.status === "diving") return showReveal(state.turns.at(-1));
  showSurface();
}

function showSurface() {
  $("#title").textContent = rules.name.toUpperCase();
  $("#dive-number").textContent = `DIVE #${state.dive_number}`;
  const lines = [
    [`${rules.prompts} PROMPTS A DAY.`, "SAME FOR EVERYONE."],
    [`${rules.seconds} SECONDS`, "TO NAME ONE THING."],
    ["RARE ANSWERS SCORE BIG.", "OBVIOUS ONES FLOAT."],
    ["THE PICK THAT FEELS CLEVER?", "THE SCHOOL THOUGHT OF IT TOO."],
    [`EVERY POINT SINKS YOU ${rules.metres_per_point} METRES.`, `${rules.trench} IS THE TRENCH.`],
    ["ONE DIVE PER DAY.", "NO SECOND CHANCES."],
  ];
  const list = $("#howto");
  list.replaceChildren(
    ...lines.map(([lead, rest]) => {
      const item = document.createElement("li");
      const dim = document.createElement("span");
      dim.textContent = ` ${rest}`;
      item.append(lead, dim);
      return item;
    }),
  );
  show("surface");
}

async function descend() {
  const buttons = [$("#dive"), $("#next")];
  buttons.forEach((b) => (b.disabled = true));
  try {
    adopt(await call("descend", {}));
    route();
  } catch (error) {
    fail(error);
  } finally {
    buttons.forEach((b) => (b.disabled = false));
  }
}

function showPrompt() {
  const { idx, prompt } = state.current;
  $("#prompt-count").textContent = `PROMPT ${idx + 1} / ${rules.prompts}`;
  $("#prompt-text").textContent = prompt;
  const input = $("#answer");
  input.value = "";
  input.disabled = false;
  $("#submit").disabled = false;
  $("#submit").textContent = "GO";
  show("prompt");
  input.focus();
  startTimer();
}

function startTimer() {
  cancelAnimationFrame(timerFrame);
  const total = rules.seconds * 1000;
  const tick = () => {
    if (!state.current) return;
    const left = Math.max(0, state.current.deadline - serverNow());
    $("#timer-bar").style.transform = `scaleX(${left / total})`;
    $("#timer-num").textContent = Math.ceil(left / 1000);
    $("#timer").classList.toggle("urgent", left <= URGENT_MS);
    if (left <= 0) return submit(); // whatever is typed goes in
    timerFrame = requestAnimationFrame(tick);
  };
  tick();
}

async function submit(event) {
  event?.preventDefault();
  if (submitting || !state.current) return;
  submitting = true;
  cancelAnimationFrame(timerFrame);
  const input = $("#answer");
  input.disabled = true;
  $("#submit").disabled = true;
  $("#submit").textContent = "…";
  try {
    const { turn, state: next } = await call("answer", {
      day: state.day,
      idx: state.current.idx,
      answer: input.value,
    });
    adopt(next);
    showReveal(turn);
  } catch (error) {
    // 409: the prompt closed under us (another tab, or very late). Resync.
    if (error.status === 409) return load();
    fail(error);
  } finally {
    submitting = false;
  }
}

function showReveal(turn) {
  const answered = state.turns.length;
  $("#reveal-count").textContent = `PROMPT ${turn.idx + 1} / ${rules.prompts} · ${state.depth} M`;
  $("#reveal-answer").textContent = turn.answer ? `“${turn.answer.toUpperCase()}”` : "";

  const points = $("#reveal-points");
  points.className = "reveal-points";
  if (turn.outcome === "counted" && turn.points > 0) {
    points.textContent = `+${turn.points}`;
    $("#reveal-detail").textContent = `${shareText(turn.share)} OF THE SCHOOL SAID IT. YOU SINK ${turn.points * rules.metres_per_point} M.`;
  } else if (turn.outcome === "counted") {
    points.textContent = "FLOATED";
    points.classList.add("floated");
    $("#reveal-detail").textContent = `${shareText(turn.share)} OF THE SCHOOL SAID IT TOO.`;
  } else {
    points.textContent = turn.outcome === "timeout" ? "TIME" : "NOPE";
    points.classList.add("missed");
    $("#reveal-detail").textContent = (turn.reason || "didn't count").toUpperCase();
  }

  const last = answered >= rules.prompts;
  $("#next").textContent = last ? "SURFACE" : "DESCEND ↓";
  $("#next").onclick = last ? () => load() : descend;
  show("reveal");
  $("#next").focus();
}

function shareText(share) {
  if (share == null) return "SOME";
  const pct = share * 100;
  if (pct < 1) return "UNDER 1%";
  return `${pct < 10 ? "ONLY " : ""}~${Math.round(pct)}%`;
}

function showResults() {
  const { results } = state;
  $("#results-dive").textContent = `DIVE #${state.dive_number} · ${rules.name.toUpperCase()}`;
  $("#results-depth").textContent = state.depth;
  requestAnimationFrame(() => {
    $("#trench-fill").style.width = `${Math.min(100, (state.depth / rules.trench) * 100)}%`;
  });

  const others = results.divers - 1;
  $("#results-rank").textContent =
    others <= 0
      ? "FIRST DIVER DOWN TODAY. THE SCHOOL IS STILL GATHERING."
      : `DEEPER THAN ${Math.round((results.shallower / others) * 100)}% OF ${others} OTHER DIVER${others === 1 ? "" : "S"} TODAY.`;

  const template = $("#log-entry");
  $("#log").replaceChildren(
    ...state.turns.map((turn, i) => {
      const entry = template.content.firstElementChild.cloneNode(true);
      entry.querySelector(".log-prompt").textContent = turn.prompt;
      entry.querySelector(".log-answer").textContent = turn.answer || "—";
      const points = entry.querySelector(".log-points");
      if (turn.outcome === "counted") {
        points.textContent = turn.points > 0 ? `+${turn.points}` : "FLOATED";
        points.classList.toggle("zero", turn.points === 0);
      } else {
        points.textContent = (turn.reason || turn.outcome).toUpperCase();
        points.classList.add("missed");
      }
      entry.querySelector(".school").replaceChildren(
        ...(results.school[i] || []).map((row) => {
          const item = document.createElement("li");
          item.classList.toggle("yours", row.yours);
          item.style.setProperty("--share", `${Math.max(2, row.share * 100)}%`);
          const name = document.createElement("span");
          name.textContent = row.answer;
          const pct = document.createElement("span");
          pct.textContent = `${Math.round(row.share * 100)}%`;
          item.append(name, pct);
          return item;
        }),
      );
      return entry;
    }),
  );

  startCountdown();
  show("results");
}

function shareSummary() {
  const marks = state.turns
    .map((t) => {
      if (t.outcome !== "counted") return "⬛";
      if (t.points === 0) return "🫧";
      if (t.points <= 3) return "🐟";
      if (t.points <= 6) return "🐙";
      if (t.points <= 9) return "🦑";
      return "🦈";
    })
    .join("");
  return `${rules.name.toUpperCase()} #${state.dive_number}\n${state.depth} M / ${rules.trench}\n${marks}\n${location.origin}`;
}

async function share() {
  const text = shareSummary();
  const button = $("#share");
  try {
    if (navigator.share && matchMedia("(pointer: coarse)").matches) {
      await navigator.share({ text });
    } else {
      await navigator.clipboard.writeText(text);
      button.textContent = "COPIED";
      setTimeout(() => (button.textContent = "SHARE"), 1600);
    }
  } catch {
    /* share sheet dismissed */
  }
}

function startCountdown() {
  clearInterval(countdownTimer);
  const update = () => {
    const left = state.next_dive_at - serverNow();
    if (left <= 0) {
      clearInterval(countdownTimer);
      return load();
    }
    const s = Math.floor(left / 1000);
    const hms = [Math.floor(s / 3600), Math.floor(s / 60) % 60, s % 60]
      .map((n) => String(n).padStart(2, "0"))
      .join(":");
    $("#next-dive").textContent = `NEXT DIVE IN ${hms}`;
  };
  update();
  countdownTimer = setInterval(update, 1000);
}

function fail(error) {
  $("#error-detail").textContent = String(error.message || error).toUpperCase();
  show("error");
}

// ── Water ───────────────────────────────────────────────────────────────────

function setDepth(metres) {
  const trench = rules?.trench || 700;
  const d = Math.min(1, Math.max(0, metres / trench));
  document.documentElement.style.setProperty("--d", d.toFixed(4));
  document.documentElement.style.setProperty("--murk", Math.pow(d, 0.55).toFixed(4));
  $("#gauge-label").textContent = `${metres} M`;
  $("#trench-label").textContent = trench;
}

function releaseBubbles() {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const layer = $(".bubbles");
  for (let i = 0; i < 16; i++) {
    const bubble = document.createElement("span");
    const size = 4 + Math.random() * 14;
    bubble.className = "bubble";
    bubble.style.left = `${Math.random() * 100}%`;
    bubble.style.width = bubble.style.height = `${size}px`;
    bubble.style.animationDuration = `${9 + Math.random() * 12}s`;
    bubble.style.animationDelay = `${-Math.random() * 20}s`;
    bubble.style.setProperty("--drift", `${(Math.random() - 0.5) * 80}px`);
    layer.append(bubble);
  }
}

// ── Wire up ─────────────────────────────────────────────────────────────────

$("#dive").addEventListener("click", descend);
$("#answer-form").addEventListener("submit", submit);
$("#share").addEventListener("click", share);
$("#retry").addEventListener("click", load);
document.addEventListener("visibilitychange", () => {
  // The clock kept running while the tab was hidden; catch up with the server.
  if (document.visibilityState === "visible" && state?.current && !submitting) startTimer();
});

releaseBubbles();
load();
