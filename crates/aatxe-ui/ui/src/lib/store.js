// aatxe ui state — a pure reducer over the SSE event stream, wrapped in
// Svelte stores. Live mode replays the session JSONL then tails it; history
// mode feeds a past session's events through the same reducer.
//
// This is a direct port of the original framework-free app.js reducer; the
// reduce()/ensureAgent()/ensureTournament() logic is byte-for-byte the same so
// the rendered state matches the old dashboard exactly.
import { writable, get } from "svelte/store";

function newStore() {
  return {
    session: null,
    agents: new Map(), // id -> agent view-model
    agentOrder: [],
    tournaments: new Map(), // id -> {task, agentIds, standings}
    external: [], // {source, report, tsMs}
    ingested: [], // {source, report, tsMs}
    notices: [],
  };
}

function ensureAgent(store, id) {
  if (!store.agents.has(id)) {
    store.agents.set(id, {
      id, name: id, task: "", branch: "", worktree: "",
      tournamentId: null, outputs: [], compares: [], failures: [],
      council: null, councilState: null, exit: null, failed: null,
      benching: false,
    });
    store.agentOrder.push(id);
  }
  return store.agents.get(id);
}

function ensureTournament(store, id) {
  if (!store.tournaments.has(id)) {
    store.tournaments.set(id, { id, task: "", agentIds: [], standings: [] });
  }
  return store.tournaments.get(id);
}

function reduce(store, ev) {
  switch (ev.type) {
    case "sessionStarted":
      store.session = ev;
      break;
    case "runIngested":
      store.ingested.push(ev);
      break;
    case "externalCompare":
      store.external.push(ev);
      break;
    case "agentSpawned": {
      const a = ensureAgent(store, ev.agentId);
      Object.assign(a, {
        name: ev.name, task: ev.task, branch: ev.branch,
        worktree: ev.worktree, tournamentId: ev.tournamentId,
      });
      if (ev.tournamentId) {
        const t = ensureTournament(store, ev.tournamentId);
        if (!t.agentIds.includes(ev.agentId)) t.agentIds.push(ev.agentId);
      }
      break;
    }
    case "agentOutput": {
      const a = ensureAgent(store, ev.agentId);
      a.outputs.push({ kind: ev.kind, text: ev.text, tsMs: ev.tsMs });
      if (a.outputs.length > 800) a.outputs.splice(0, a.outputs.length - 800);
      break;
    }
    case "iterationStarted":
      ensureAgent(store, ev.agentId).benching = true;
      break;
    case "iterationCompare": {
      const a = ensureAgent(store, ev.agentId);
      a.benching = false;
      a.compares.push({ iteration: ev.iteration, report: ev.report, tsMs: ev.tsMs });
      break;
    }
    case "iterationFailed": {
      const a = ensureAgent(store, ev.agentId);
      a.benching = false;
      a.failures.push({ iteration: ev.iteration, error: ev.error });
      break;
    }
    case "councilStarted":
      ensureAgent(store, ev.agentId).councilState = "running";
      break;
    case "councilVerdict": {
      const a = ensureAgent(store, ev.agentId);
      a.councilState = "done";
      a.council = ev;
      break;
    }
    case "councilFailed": {
      const a = ensureAgent(store, ev.agentId);
      a.councilState = "failed";
      a.council = { error: ev.error };
      break;
    }
    case "agentExited":
      ensureAgent(store, ev.agentId).exit = ev;
      break;
    case "agentFailed":
      ensureAgent(store, ev.agentId).failed = ev.error;
      break;
    case "tournamentStarted": {
      const t = ensureTournament(store, ev.tournamentId);
      t.task = ev.task;
      for (const id of ev.agentIds) if (!t.agentIds.includes(id)) t.agentIds.push(id);
      break;
    }
    case "tournamentStandings":
      ensureTournament(store, ev.tournamentId).standings = ev.standings;
      break;
    case "notice":
      store.notices.push(ev);
      if (store.notices.length > 50) store.notices.shift();
      break;
  }
}

// ---------- stores ----------

let live = newStore();
let current = live; // what's rendered: the live store, or a history snapshot

// `view` always holds a *fresh* shallow wrapper of `current`. The reducer
// mutates `current` in place, so we publish a new top-level object on every
// change — Svelte's fine-grained reactivity is reference-based, and a new
// wrapper is what makes dependent `$derived`s recompute. Components look the
// agent/tournament they render up out of `$view` by id (never holding a stale
// nested reference), so in-place mutations always surface.
export const view = writable({ ...current });
export const selected = writable({ kind: null, id: null }); // agent | tournament | external
export const viewingHistory = writable(false);
export const connection = writable("connecting"); // connecting | live | lost
export const sessions = writable([]);

let lastSeq = 0;
let es = null;

function publish() {
  view.set({ ...current });
}

// Select something sensible without stealing an explicit selection.
function autoselect(ev) {
  const sel = get(selected);
  if (sel.kind) return;
  if (ev.type === "agentSpawned") selected.set({ kind: "agent", id: ev.agentId });
  if (ev.type === "externalCompare") selected.set({ kind: "external", id: null });
}

export function connect() {
  if (es) es.close();
  es = new EventSource(`/api/events?since=${lastSeq}`);
  es.onopen = () => connection.set("live");
  es.onerror = () => {
    connection.set("lost");
    es.close();
    setTimeout(connect, 2000);
  };
  es.onmessage = (m) => {
    let ev;
    try { ev = JSON.parse(m.data); } catch { return; }
    lastSeq = Math.max(lastSeq, ev.seq || 0);
    reduce(live, ev);
    autoselect(ev);
    if (current === live) publish();
  };
}

export async function loadSessions() {
  try {
    const list = await (await fetch("/api/sessions")).json();
    sessions.set(list);
  } catch {
    sessions.set(null); // null = failed to load
  }
}

export async function viewSession(id) {
  if (live.session && id === live.session.sessionId) {
    backToLive();
    return;
  }
  try {
    const events = await (await fetch(`/api/sessions/${id}/events`)).json();
    const store = newStore();
    for (const ev of events) reduce(store, ev);
    current = store;
    viewingHistory.set(true);
    selected.set({ kind: null, id: null });
    publish();
  } catch { /* leave current view */ }
}

export function backToLive() {
  current = live;
  viewingHistory.set(false);
  selected.set({ kind: null, id: null });
  publish();
}

// Spawn a single agent or a tournament. Returns nothing; sets selection and
// throws a string error message the caller surfaces inline.
export async function spawn(task, count) {
  const url = count > 1 ? "/api/tournaments" : "/api/agents";
  const body = count > 1 ? { task, count } : { task };
  const r = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(await r.text());
  const v = await r.json();
  selected.set(count > 1
    ? { kind: "tournament", id: v.tournamentId }
    : { kind: "agent", id: v.agentId });
}

export function liveSessionId() {
  return live.session ? live.session.sessionId : null;
}
