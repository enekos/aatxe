// aatxe ui frontend — a pure reducer over the SSE event stream.
// Live mode replays the session JSONL then tails it; history mode feeds
// a past session's events through the same reducer. No framework.
(function () {
  "use strict";

  const fmt = window.AatxeChart.formatNs;

  // ---------- store ----------

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

  // ---------- live state ----------

  let live = newStore();
  let current = live; // what's rendered (live or a history store)
  let viewingHistory = false;
  let selected = { kind: null, id: null }; // agent | tournament | external
  let lastSeq = 0;
  let es = null;

  function connect() {
    if (es) es.close();
    es = new EventSource(`/api/events?since=${lastSeq}`);
    es.onopen = () => setConn("live");
    es.onerror = () => {
      setConn("lost");
      es.close();
      setTimeout(connect, 2000);
    };
    es.onmessage = (m) => {
      let ev;
      try { ev = JSON.parse(m.data); } catch { return; }
      lastSeq = Math.max(lastSeq, ev.seq || 0);
      reduce(live, ev);
      autoselect(ev);
      scheduleRender();
    };
  }

  // Select something sensible without stealing an explicit selection.
  function autoselect(ev) {
    if (selected.kind) return;
    if (ev.type === "agentSpawned") selected = { kind: "agent", id: ev.agentId };
    if (ev.type === "externalCompare") selected = { kind: "external", id: null };
  }

  function setConn(cls) {
    const el = document.getElementById("conn");
    el.className = "conn " + cls;
  }

  // ---------- render ----------

  let renderQueued = false;
  function scheduleRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      render();
    });
  }

  function esc(s) {
    return String(s ?? "").replace(/[&<>"]/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])
    );
  }

  function agentStatus(a) {
    if (a.failed) return "failed";
    if (a.exit) return "done";
    if (a.benching) return "benching";
    return "running";
  }

  function render() {
    renderHeader();
    renderRail();
    renderMain();
  }

  function renderHeader() {
    const s = current.session;
    const el = document.getElementById("session-meta");
    el.textContent = s
      ? `${s.sessionId} · ${s.repoRoot} · base ${s.baseRef}@${(s.baseSha || "").slice(0, 8)} · bench ${s.benchLabel}`
      : "waiting for session…";
    document.getElementById("history-banner").classList.toggle("hidden", !viewingHistory);
  }

  function renderRail() {
    const agents = document.getElementById("agent-list");
    agents.innerHTML = current.agentOrder.map((id) => {
      const a = current.agents.get(id);
      const sel = selected.kind === "agent" && selected.id === id ? "selected" : "";
      const iters = a.compares.length ? `#${a.compares[a.compares.length - 1].iteration}` : "";
      return `<li class="${sel}" data-kind="agent" data-id="${esc(id)}">
        <span class="dot ${agentStatus(a)}"></span>${esc(a.name)}
        <span class="sub">${iters}</span></li>`;
    }).join("") || '<li class="sub">none yet</li>';

    const ts = document.getElementById("tournament-list");
    ts.innerHTML = [...current.tournaments.values()].map((t) => {
      const sel = selected.kind === "tournament" && selected.id === t.id ? "selected" : "";
      const leader = t.standings[0];
      return `<li class="${sel}" data-kind="tournament" data-id="${esc(t.id)}">🏆 ${esc(t.id)}
        <span class="sub">${leader ? esc(leader.name) : t.agentIds.length + " agents"}</span></li>`;
    }).join("") || '<li class="sub">none yet</li>';

    const ext = document.getElementById("external-list");
    const n = current.external.length + current.ingested.length;
    const sel = selected.kind === "external" ? "selected" : "";
    ext.innerHTML = n
      ? `<li class="${sel}" data-kind="external" data-id="">perf-vs &amp; ingest <span class="sub">${n}</span></li>`
      : '<li class="sub">none yet</li>';
  }

  function renderMain() {
    const main = document.getElementById("main");
    if (selected.kind === "agent" && current.agents.has(selected.id)) {
      main.innerHTML = agentView(current.agents.get(selected.id));
      drawAgentCharts(current.agents.get(selected.id));
      wireAgentButtons();
      const t = main.querySelector(".transcript");
      if (t) t.scrollTop = t.scrollHeight;
    } else if (selected.kind === "tournament" && current.tournaments.has(selected.id)) {
      main.innerHTML = tournamentView(current.tournaments.get(selected.id));
    } else if (selected.kind === "external") {
      main.innerHTML = externalView();
    } else {
      main.innerHTML = `<div class="empty"><p>🐂 spawn an agent, or run <code>aatxe perf-vs</code> in a terminal and watch it appear.</p></div>` + noticesCard();
    }
  }

  function noticesCard() {
    if (!current.notices.length) return "";
    return `<div class="card"><h3>notices</h3><div class="notices">${
      current.notices.slice(-8).map((n) => `<div>${esc(n.message)}</div>`).join("")
    }</div></div>`;
  }

  // ---- agent view ----

  function benchSeries(a) {
    // bench name -> {points: [{x, y, verdict, label}], baseY}
    const series = new Map();
    for (const c of a.compares) {
      for (const d of c.report.diffs || []) {
        if (!d.head) continue;
        if (!series.has(d.name)) series.set(d.name, { points: [], baseY: null });
        const s = series.get(d.name);
        if (d.base) s.baseY = d.base.median;
        const deltaTxt = d.deltaPct != null ? ` (${(d.deltaPct * 100).toFixed(1)}%)` : "";
        s.points.push({
          x: c.iteration, y: d.head.median, verdict: d.verdict,
          label: `#${c.iteration}: ${fmt(d.head.median)}${deltaTxt} — ${d.verdict}`,
        });
      }
    }
    return series;
  }

  function agentView(a) {
    const status = agentStatus(a);
    const last = a.compares[a.compares.length - 1];
    let html = `<div class="card">
      <div class="agent-head">
        <span class="dot ${status}"></span>
        <span class="name">${esc(a.name)}</span>
        <span class="meta">${esc(a.id)} · ${esc(a.branch)}</span>
        ${a.exit ? `<span class="meta">exit ${a.exit.exitCode ?? "?"} · ${a.exit.iterations} iteration(s)</span>` : ""}
        <button class="mini" id="show-diff">diff</button>
      </div>
      <div class="task-text">${esc(a.task)}</div>
      ${a.failed ? `<div class="iter-failed">agent failed: ${esc(a.failed)}</div>` : ""}
    </div>`;

    html += `<div class="card"><h3>trajectories
      <span class="right">${a.benching ? "benching…" : last ? `iter #${last.iteration}` : "waiting for first edit"}</span></h3>
      <div class="charts" id="charts"></div>
      ${a.failures.slice(-3).map((f) => `<div class="iter-failed">iter #${f.iteration} bench failed: ${esc(f.error)}</div>`).join("")}
    </div>`;

    if (last) html += `<div class="card"><h3>latest compare
      <span class="right">${summaryChips(last.report.summary)}</span></h3>${compareTable(last.report)}</div>`;

    if (a.councilState) {
      let body;
      if (a.councilState === "running") body = '<div class="notices">council reviewing…</div>';
      else if (a.councilState === "failed") body = `<div class="iter-failed">council failed: ${esc(a.council.error)}</div>`;
      else body = `<div class="council-chips">
          <span class="badge crit">${a.council.critical} critical</span>
          <span class="badge removed">${a.council.major} major</span>
          <span class="badge neutral">${a.council.shippable} shippable</span>
        </div><pre class="council">${esc(a.council.markdown || "(empty)")}</pre>`;
      html += `<div class="card"><h3>council</h3>${body}</div>`;
    }

    html += `<div class="card"><h3>transcript <span class="right">${a.outputs.length} lines</span></h3>
      <div class="transcript">${a.outputs.map((o) =>
        `<div class="line k-${esc(o.kind)}">${esc(o.text)}</div>`).join("")}</div></div>`;

    html += `<div class="card hidden" id="diff-card"><h3>diff vs base</h3><pre class="diffview" id="diff-body">loading…</pre></div>`;
    return html;
  }

  function drawAgentCharts(a) {
    const grid = document.getElementById("charts");
    if (!grid) return;
    const series = benchSeries(a);
    if (series.size === 0) {
      grid.innerHTML = '<div class="notices">no benchmark data yet — first iteration runs after the agent\'s first edit</div>';
      return;
    }
    grid.innerHTML = [...series.keys()].map((name, i) => {
      const s = series.get(name);
      const lastPt = s.points[s.points.length - 1];
      const delta = s.baseY && lastPt ? (lastPt.y - s.baseY) / s.baseY : null;
      const cls = delta == null ? "" : delta > 0 ? "up" : "down";
      const dTxt = delta == null ? "" : `${delta > 0 ? "+" : ""}${(delta * 100).toFixed(1)}%`;
      return `<div class="chart-box"><div class="chart-title">${esc(name)}
        <span class="delta ${cls}">${dTxt}</span></div><div id="chart-${i}"></div></div>`;
    }).join("");
    let i = 0;
    for (const name of series.keys()) {
      window.AatxeChart.trajectory(document.getElementById(`chart-${i}`), series.get(name));
      i++;
    }
  }

  function wireAgentButtons() {
    const btn = document.getElementById("show-diff");
    if (!btn) return;
    btn.onclick = async () => {
      const card = document.getElementById("diff-card");
      card.classList.remove("hidden");
      const body = document.getElementById("diff-body");
      try {
        const r = await fetch(`/api/agents/${selected.id}/diff`);
        body.textContent = r.ok ? (await r.text()) || "(empty diff)" : `error: ${await r.text()}`;
      } catch (e) {
        body.textContent = `error: ${e}`;
      }
    };
  }

  // ---- compare table ----

  function summaryChips(s) {
    if (!s) return "";
    const chip = (n, cls, label) => (n ? `<span class="badge ${cls}">${n} ${label}</span> ` : "");
    return chip(s.regressions, "regression", "reg") + chip(s.improvements, "improvement", "imp") +
      chip(s.neutrals, "neutral", "neutral") + chip(s.new, "new", "new");
  }

  function compareTable(report) {
    const rows = (report.diffs || []).map((d) => {
      const base = d.base ? fmt(d.base.median) : "—";
      const head = d.head ? fmt(d.head.median) : "—";
      const delta = d.deltaPct != null ? `${d.deltaPct > 0 ? "+" : ""}${(d.deltaPct * 100).toFixed(1)}%` : "—";
      const p = d.pValue != null ? d.pValue.toFixed(4) : "—";
      return `<tr><td>${esc(d.name)}</td><td class="num">${base}</td><td class="num">${head}</td>
        <td class="num">${delta}</td><td class="num">${p}</td>
        <td><span class="badge ${esc(d.verdict)}">${esc(d.verdict)}</span></td></tr>`;
    }).join("");
    return `<table><thead><tr><th>bench</th><th class="num">base</th><th class="num">head</th>
      <th class="num">Δ median</th><th class="num">p</th><th>verdict</th></tr></thead>
      <tbody>${rows}</tbody></table>`;
  }

  // ---- tournament view ----

  function tournamentView(t) {
    const rows = t.standings.map((s) => {
      const first = s.rank === 1 ? "lb-first" : "";
      return `<tr class="${first}"><td class="lb-rank">#${s.rank}</td>
        <td>${esc(s.name)} <span class="sub">${esc(s.agentId)}</span></td>
        <td class="num">${s.score.toFixed(1)}</td>
        <td class="num">${s.improvements}</td><td class="num">${s.regressions}</td>
        <td class="num">${s.councilCritical ?? "…"}</td>
        <td class="num">${(s.medianDeltaSum * 100).toFixed(1)}%</td></tr>`;
    }).join("");
    const agents = t.agentIds.map((id) => {
      const a = current.agents.get(id);
      return a ? `<li data-kind="agent" data-id="${esc(id)}"><span class="dot ${agentStatus(a)}"></span>${esc(a.name)}</li>` : "";
    }).join("");
    return `<div class="card"><h3>tournament ${esc(t.id)}</h3>
      <div class="task-text">${esc(t.task)}</div></div>
      <div class="card"><h3>leaderboard</h3>
      ${t.standings.length ? `<table><thead><tr><th></th><th>agent</th><th class="num">score</th>
        <th class="num">imp</th><th class="num">reg</th><th class="num">crit</th><th class="num">net Δ</th></tr></thead>
        <tbody>${rows}</tbody></table>` : '<div class="notices">standings appear after the first benched iteration</div>'}
      </div>
      <div class="card"><h3>contestants</h3><ul class="nav-list">${agents}</ul></div>` + noticesCard();
  }

  // ---- external view ----

  function externalView() {
    const cmp = current.external.slice(-10).reverse().map((e) =>
      `<div class="card"><h3>${esc(e.source)}
        <span class="right">${summaryChips(e.report.summary)}</span></h3>${compareTable(e.report)}</div>`
    ).join("");
    const runs = current.ingested.slice(-10).reverse().map((e) => {
      const rows = (e.report.runs || []).map((r) =>
        `<tr><td>${esc(r.name)}</td><td class="num">${fmt(r.median)}</td><td class="num">${fmt(r.p95)}</td>
         <td class="num">${(r.cv * 100).toFixed(1)}%</td></tr>`).join("");
      return `<div class="card"><h3>${esc(e.source)} <span class="right">${esc(e.report.ref)}</span></h3>
        <table><thead><tr><th>bench</th><th class="num">median</th><th class="num">p95</th><th class="num">cv</th></tr></thead>
        <tbody>${rows}</tbody></table></div>`;
    }).join("");
    return (cmp + runs) || '<div class="empty"><p>nothing external yet — run <code>aatxe perf-vs</code> or POST a RunReport to /api/runs</p></div>';
  }

  // ---------- interactions ----------

  document.addEventListener("click", (e) => {
    const li = e.target.closest("li[data-kind]");
    if (li) {
      selected = { kind: li.dataset.kind, id: li.dataset.id || null };
      scheduleRender();
    }
  });

  document.getElementById("spawn").addEventListener("click", async () => {
    const task = document.getElementById("task").value.trim();
    const count = parseInt(document.getElementById("count").value, 10);
    const errEl = document.getElementById("spawn-error");
    errEl.textContent = "";
    if (!task) { errEl.textContent = "task is empty"; return; }
    const url = count > 1 ? "/api/tournaments" : "/api/agents";
    const body = count > 1 ? { task, count } : { task };
    try {
      const r = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!r.ok) { errEl.textContent = await r.text(); return; }
      const v = await r.json();
      selected = count > 1
        ? { kind: "tournament", id: v.tournamentId }
        : { kind: "agent", id: v.agentId };
      document.getElementById("task").value = "";
      scheduleRender();
    } catch (e2) {
      errEl.textContent = String(e2);
    }
  });

  // ---------- history ----------

  async function loadSessions() {
    const ul = document.getElementById("session-list");
    try {
      const sessions = await (await fetch("/api/sessions")).json();
      ul.innerHTML = sessions.slice(0, 12).map((s) => {
        const when = s.startedMs ? new Date(s.startedMs).toLocaleString() : "?";
        return `<li data-session="${esc(s.sessionId)}">${esc(s.sessionId)}
          <span class="sub">${esc(when)} · ${s.events}ev</span></li>`;
      }).join("") || '<li class="sub">none</li>';
    } catch {
      ul.innerHTML = '<li class="sub">failed to load</li>';
    }
  }

  document.getElementById("load-sessions").addEventListener("click", loadSessions);

  document.addEventListener("click", async (e) => {
    const li = e.target.closest("li[data-session]");
    if (!li) return;
    const id = li.dataset.session;
    if (current.session && id === live.session?.sessionId) {
      backToLive();
      return;
    }
    try {
      const events = await (await fetch(`/api/sessions/${id}/events`)).json();
      const store = newStore();
      for (const ev of events) reduce(store, ev);
      current = store;
      viewingHistory = true;
      selected = { kind: null, id: null };
      scheduleRender();
    } catch { /* leave current view */ }
  });

  function backToLive() {
    current = live;
    viewingHistory = false;
    selected = { kind: null, id: null };
    scheduleRender();
  }
  document.getElementById("back-to-live").addEventListener("click", backToLive);

  // ---------- boot ----------

  connect();
  loadSessions();
  render();
})();
