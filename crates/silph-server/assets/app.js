"use strict";

// The dashboard: one page, a sidebar of selectable hosts, and a grid of charts
// showing every selected host together over one shared time window.

import {
  formatAge,
  formatClock,
  formatDuration,
  formatStamp,
  fromLocalInput,
  toLocalInput,
} from "/assets/format.js";
import {
  MAX_WINDOW_MS,
  MIN_WINDOW_MS,
  RANGES,
  assignHostColors,
  commit,
  isOwnHash,
  hostColor,
  isLive,
  onChange,
  panBy,
  readHash,
  seriesColor,
  setAbsolute,
  setHosts,
  setRelative,
  state,
  timeWindow,
  toggleHost,
  zoomBy,
} from "/assets/state.js";
import {
  applyWindow,
  createChart,
  destroyChart,
  focusHost,
  isGesturing,
  setWindowHandler,
} from "/assets/chart.js";

const el = (id) => document.getElementById(id);
const view = el("view");
const hostList = el("host-list");
const rangeBar = el("range-bar");
const statusEl = el("status");
const sidebar = el("sidebar");
const scrim = el("scrim");

const REFRESH_MS = 30e3;
/** Roughly how many buckets to ask for; fewer on phones keeps touch smooth. */
const targetBuckets = () => (window.innerWidth < 700 ? 150 : 320);

// Related metrics rendered together on one chart. `capacity: true` marks a
// "how much exists" series (drawn as a dashed reference line, no fill) as
// opposed to "how much is in use". Instanced metrics (e.g. per mount point)
// fan out into one series per instance.
const GROUPS = [
  {
    title: "CPU usage",
    unit: "percent",
    metrics: [{ name: "cpu_usage_percent", label: "usage" }],
  },
  {
    title: "Memory",
    unit: "bytes",
    metrics: [
      { name: "memory_used", label: "used" },
      { name: "memory_total", label: "total", capacity: true },
    ],
  },
  {
    title: "Swap",
    unit: "bytes",
    metrics: [
      { name: "memory_swap_used", label: "used" },
      { name: "memory_swap_total", label: "total", capacity: true },
    ],
  },
  {
    title: "Disk usage",
    unit: "percent",
    metrics: [{ name: "disk_used_percent" }],
  },
  {
    title: "Disk space",
    unit: "bytes",
    metrics: [
      { name: "disk_used", label: "used" },
      { name: "disk_total", label: "total", capacity: true },
    ],
  },
  {
    title: "Temperature",
    unit: "celsius",
    metrics: [{ name: "temperature_celsius" }],
  },
];

// Stored but not charted: the Memory chart (used vs. total) already shows it.
const HIDDEN_METRICS = new Set(["memory_used_percent"]);

/** Server config; the query step is aligned to the scrape interval. */
let serverConfig = { scrape_interval_ms: 15000 };
/** Last /api/hosts response, for the sidebar. */
let knownHosts = [];
/** One entry per rendered panel: {group, card, plot, u, signature, state}. */
let panels = [];
/** Element holding the "no data for ..." footnote under the grid. */
let gridNote = null;
/** Bumped whenever a rebuild invalidates in-flight loads. */
let generation = 0;
let refreshTimer = null;

// --- fetching --------------------------------------------------------------

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    const detail = (await response.text()).trim();
    throw new Error(detail || `${url}: ${response.status}`);
  }
  return response.json();
}

function queryStep(win) {
  // Round the step up to a multiple of the scrape interval so every bucket
  // spans at least one sample slot; otherwise empty buckets alias into
  // periodic gaps in the charts. Gaps then only mean genuinely missed scrapes.
  // scrape_interval_ms is a u128 on the wire; past Number.MAX_SAFE_INTEGER it
  // parses imprecisely (or as Infinity), so clamp -- any such interval is
  // effectively "huge" and the step math must stay finite.
  const interval = Math.max(
    1000,
    Math.min(Number(serverConfig.scrape_interval_ms), Number.MAX_SAFE_INTEGER)
  );
  const span = win.end - win.start;
  return Math.ceil(Math.max(1, span / targetBuckets()) / interval) * interval;
}

/**
 * Loads every metric of one group, for every selected host, in a single
 * request, and flattens the response into uPlot columns plus per-series
 * display definitions. Returns null when nothing in the group has data.
 */
async function loadPanelData(group, win) {
  const params = new URLSearchParams({
    host: state.hosts.join(","),
    metric: group.metrics.map((m) => m.name).join(","),
    start: String(win.start),
    end: String(win.end),
    step: String(queryStep(win)),
  });
  const result = await fetchJson("/api/query?" + params);
  if (result.series.length === 0) return null;

  const metricIndex = new Map(group.metrics.map((m, i) => [m.name, i]));
  // Order by the user's host order, then instance, then the group's own metric
  // order, so the legend reads the same way on every refresh.
  const series = [...result.series].sort(
    (a, b) =>
      state.hosts.indexOf(a.host) - state.hosts.indexOf(b.host) ||
      (a.instance ?? "").localeCompare(b.instance ?? "") ||
      metricIndex.get(a.metric) - metricIndex.get(b.metric)
  );
  // Hue comes from the host, so a host looks the same on every chart and in
  // every selection. Within a host, instances fan out by their rank in that
  // host's own sorted instance list rather than by a hash of the name: two
  // mounts like "/" and "/boot" hash to neighbouring hues and would come out
  // the same colour.
  const ranks = instanceRanks(series);
  const primaries = group.metrics.filter((m) => !m.capacity);
  const multiHost = state.hosts.length > 1;
  const defs = series.map((s) => {
    const metric = group.metrics[metricIndex.get(s.metric)];
    const hostRanks = ranks.get(s.host);
    return {
      host: s.host,
      label: seriesLabel(group, metric, s, multiHost),
      color: seriesColor(
        s.host,
        hostRanks?.get(s.instance) ?? 0,
        hostRanks?.size ?? 1,
        Math.max(0, primaries.indexOf(metric))
      ),
      capacity: !!metric.capacity,
    };
  });
  return {
    data: [result.t.map((ms) => ms / 1000)].concat(series.map((s) => s.values)),
    defs,
  };
}

/** Per host, each of its instances mapped to its position in sorted order. */
function instanceRanks(series) {
  const byHost = new Map();
  for (const s of series) {
    if (s.instance == null) continue;
    if (!byHost.has(s.host)) byHost.set(s.host, new Set());
    byHost.get(s.host).add(s.instance);
  }
  const ranks = new Map();
  for (const [host, instances] of byHost) {
    ranks.set(host, new Map([...instances].sort().map((name, i) => [name, i])));
  }
  return ranks;
}

/**
 * Legend text: host, instance and metric, but only the parts that actually
 * distinguish this series from its neighbours on the same chart.
 */
function seriesLabel(group, metric, s, multiHost) {
  const parts = [];
  if (multiHost) parts.push(s.host);
  if (s.instance) parts.push(s.instance);
  if (group.metrics.length > 1 || parts.length === 0) {
    parts.push(metric.label ?? metric.name);
  }
  return parts.join(" · ");
}

// --- panels ----------------------------------------------------------------

function message(text, className) {
  const p = document.createElement("p");
  p.className = "empty " + (className ?? "");
  p.textContent = text;
  return p;
}

function buildPanels() {
  for (const panel of panels) destroyChart(panel.u);
  panels = [];
  gridNote = null;
  view.replaceChildren();

  if (state.hosts.length === 0) {
    view.appendChild(message("Select a host to chart.", "muted"));
    return;
  }
  const grid = document.createElement("div");
  grid.className = "grid";
  gridNote = document.createElement("p");
  gridNote.className = "muted small grid-note";
  view.append(grid, gridNote);

  panels = GROUPS.filter((g) =>
    g.metrics.some((m) => !HIDDEN_METRICS.has(m.name))
  ).map((group) => {
    const card = document.createElement("section");
    card.className = "chart";
    const head = document.createElement("header");
    const title = document.createElement("h2");
    title.textContent = group.title;
    const unit = document.createElement("span");
    unit.className = "unit";
    unit.textContent = group.unit;
    head.append(title, unit);
    const plot = document.createElement("div");
    plot.className = "plot loading";
    card.append(head, plot);
    grid.appendChild(card);
    return { group, card, plot, u: null, signature: null, state: "loading" };
  });
}

/** Moves a panel out of chart mode and into a placeholder (or hides it). */
function setPlaceholder(panel, name, node) {
  panel.state = name;
  destroyChart(panel.u);
  panel.u = null;
  panel.signature = null;
  panel.card.hidden = name === "empty";
  panel.plot.classList.remove("loading");
  panel.plot.replaceChildren(...(node ? [node] : []));
}

async function loadPanel(panel, win, gen) {
  try {
    const payload = await loadPanelData(panel.group, win);
    if (gen !== generation) return;
    if (!payload) {
      setPlaceholder(panel, "empty");
      return;
    }
    const signature = payload.defs.map((d) => d.host + d.label + d.color).join("|");
    if (panel.u && panel.signature === signature) {
      // Same series set: swap the numbers in place, no rebuild, no flicker.
      panel.u.setData(payload.data);
    } else {
      destroyChart(panel.u);
      panel.card.hidden = false;
      panel.plot.classList.remove("loading");
      panel.plot.replaceChildren();
      panel.u = createChart(panel.plot, panel.group, payload);
      panel.signature = signature;
      panel.state = "chart";
    }
    // The x range is the requested window, not the data's extent, so every
    // chart lines up even when one host's series stops short.
    applyWindow(panel.u, win.start, win.end);
  } catch (e) {
    if (gen !== generation) return;
    // A refresh that fails keeps the last good chart on screen; only a panel
    // with nothing to show yet surfaces the error.
    if (!panel.u) setPlaceholder(panel, "error", message(e.message, "error"));
  }
}

async function refresh() {
  clearTimeout(refreshTimer);
  refreshTimer = null;
  const gen = generation;
  if (state.hosts.length === 0) {
    renderStatus();
    return;
  }
  const win = timeWindow();
  await Promise.all(panels.map((panel) => loadPanel(panel, win, gen)));
  if (gen !== generation) return;
  const empty = panels.filter((p) => p.state === "empty").map((p) => p.group.title);
  if (gridNote) {
    gridNote.textContent = empty.length
      ? `No data in this window for: ${empty.join(", ")}.`
      : "";
  }
  renderStatus();
  scheduleRefresh();
}

function scheduleRefresh() {
  clearTimeout(refreshTimer);
  // Only a live (relative) window moves on its own; a pinned window would
  // re-fetch identical data forever.
  if (!isLive()) return;
  refreshTimer = setTimeout(() => {
    // A gesture in flight owns the x scales, and a hidden tab need not fetch;
    // in both cases wait out another interval.
    if (isGesturing() || document.hidden) scheduleRefresh();
    else refresh();
  }, REFRESH_MS);
}

// --- sidebar ---------------------------------------------------------------

async function loadHosts() {
  knownHosts = await fetchJson("/api/hosts");
  assignHostColors(knownHosts.map((h) => h.name));
  renderHostList();
}

function renderHostList() {
  const selected = new Set(state.hosts);
  hostList.replaceChildren();
  if (knownHosts.length === 0) {
    hostList.appendChild(message("no hosts scraped yet", "muted small"));
  }
  for (const host of knownHosts) {
    const item = document.createElement("li");
    const label = document.createElement("label");
    label.className = "host" + (selected.has(host.name) ? " selected" : "");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = selected.has(host.name);
    box.onchange = () => {
      toggleHost(host.name);
      commit();
    };
    const swatch = document.createElement("span");
    swatch.className = "swatch";
    swatch.style.background = hostColor(host.name);
    const name = document.createElement("span");
    name.className = "host-name";
    name.textContent = host.name;
    const dot = document.createElement("span");
    dot.className = "dot " + (host.up ? "up" : "down");
    dot.title = host.up ? "up" : "down";
    const age = document.createElement("span");
    age.className = "muted small host-age";
    age.textContent = formatAge(host.last_scrape_ms);
    label.append(box, swatch, name, dot, age);
    // Pointing at a host isolates its lines everywhere: with more hosts on one
    // chart than colour alone can separate, this is what names a series.
    label.onpointerenter = () => focusHost(host.name);
    label.onpointerleave = () => focusHost(null);
    item.appendChild(label);
    if (host.error) {
      const err = document.createElement("p");
      err.className = "error small host-error";
      err.textContent = host.error;
      item.appendChild(err);
    }
    hostList.appendChild(item);
  }
  const up = knownHosts.filter((h) => h.up).length;
  el("host-summary").textContent =
    `${state.hosts.length}/${knownHosts.length} selected · ${up} up`;
  el("menu-count").textContent = String(state.hosts.length);
}

// --- range bar -------------------------------------------------------------

let customOpen = false;

function renderRangeBar() {
  rangeBar.replaceChildren();
  // The picker hangs off the top bar, not the range bar: on a phone the range
  // bar scrolls horizontally and would clip a popover inside it.
  el("topbar").querySelector(".picker")?.remove();

  const quick = document.createElement("div");
  quick.className = "seg quick";
  for (const range of RANGES) {
    const button = document.createElement("button");
    button.textContent = range.label;
    button.className = isLive() && state.range.ms === range.ms ? "active" : "";
    button.onclick = () => {
      setRelative(range.ms);
      commit();
    };
    quick.appendChild(button);
  }

  const nav = document.createElement("div");
  nav.className = "seg nav";
  const navButton = (glyph, title, fn) => {
    const button = document.createElement("button");
    button.textContent = glyph;
    button.title = title;
    button.setAttribute("aria-label", title);
    button.onclick = () => {
      fn();
      commit();
    };
    nav.appendChild(button);
  };
  navButton("←", "pan back", () => panBy(-0.5));
  navButton("−", "zoom out", () => zoomBy(2));
  navButton("+", "zoom in", () => zoomBy(0.5));
  navButton("→", "pan forward", () => panBy(0.5));

  const custom = document.createElement("button");
  custom.className = "chip" + (customOpen ? " active" : "");
  custom.textContent = "custom";
  custom.setAttribute("aria-expanded", String(customOpen));
  custom.onclick = () => {
    customOpen = !customOpen;
    renderRangeBar();
  };

  rangeBar.append(quick, nav, custom);
  if (!isLive()) {
    const back = document.createElement("button");
    back.className = "chip";
    back.textContent = "live";
    back.title = "keep this window width, but end it now";
    back.onclick = () => {
      setRelative(Math.max(MIN_WINDOW_MS, state.range.to - state.range.from));
      commit();
    };
    rangeBar.appendChild(back);
  }
  if (customOpen) el("topbar").appendChild(customPicker());
}

function windowLabel() {
  const win = timeWindow();
  return `${formatStamp(win.start)} – ${formatStamp(win.end)}`;
}

function customPicker() {
  const win = timeWindow();
  const box = document.createElement("form");
  box.className = "picker";
  const field = (labelText, value) => {
    const wrap = document.createElement("label");
    wrap.className = "field";
    const text = document.createElement("span");
    text.textContent = labelText;
    const input = document.createElement("input");
    input.type = "datetime-local";
    input.value = toLocalInput(value);
    wrap.append(text, input);
    box.appendChild(wrap);
    return input;
  };
  const from = field("from", win.start);
  const to = field("to", win.end);
  const apply = document.createElement("button");
  apply.type = "submit";
  apply.className = "chip primary";
  apply.textContent = "apply";
  const error = document.createElement("span");
  error.className = "error small";
  box.append(apply, error);
  box.onsubmit = (e) => {
    e.preventDefault();
    const start = fromLocalInput(from.value);
    const end = fromLocalInput(to.value);
    if (start == null || end == null) {
      error.textContent = "enter both times";
    } else if (end - start < MIN_WINDOW_MS) {
      error.textContent = "window must be at least a minute";
    } else if (end - start > MAX_WINDOW_MS) {
      error.textContent = "window must be under a year";
    } else {
      customOpen = false;
      setAbsolute(start, end);
      commit();
    }
  };
  return box;
}

function renderStatus() {
  const win = timeWindow();
  const span = formatDuration(win.end - win.start);
  statusEl.textContent = isLive()
    ? `${span} · updated ${formatClock(new Date())}`
    : windowLabel();
}

// --- drawer ----------------------------------------------------------------

function setDrawer(open) {
  document.body.classList.toggle("drawer-open", open);
  scrim.hidden = !open;
}

// --- wiring ----------------------------------------------------------------

/** Host list signature, to tell a selection change from a range change. */
let renderedHosts = null;

function applyState() {
  renderHostList();
  renderRangeBar();
  renderStatus();
  const signature = state.hosts.join(" ");
  if (signature !== renderedHosts) {
    renderedHosts = signature;
    generation++;
    buildPanels();
  }
  refresh();
}

async function start() {
  const hadHosts = readHash();
  setWindowHandler((from, to) => {
    setAbsolute(from, to);
    // Drags and pinches would otherwise bury the previous view under dozens
    // of history entries.
    commit({ replace: true });
  });
  onChange(applyState);

  el("menu").onclick = () =>
    setDrawer(!document.body.classList.contains("drawer-open"));
  el("sidebar-close").onclick = () => setDrawer(false);
  scrim.onclick = () => setDrawer(false);
  el("select-all").onclick = () => {
    setHosts(knownHosts.map((h) => h.name));
    commit();
  };
  el("select-none").onclick = () => {
    setHosts([]);
    commit();
  };
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") setDrawer(false);
  });
  window.addEventListener("hashchange", () => {
    // Only a real navigation (back/forward, an edited or pasted URL) is state
    // the app has not already applied.
    if (isOwnHash()) return;
    readHash();
    applyState();
  });
  // A tab hidden through several refresh ticks comes back stale.
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && isLive() && !refreshTimer) refresh();
  });

  try {
    const [config, hosts] = await Promise.all([
      fetchJson("/api/config"),
      fetchJson("/api/hosts"),
    ]);
    serverConfig = config;
    knownHosts = hosts;
    assignHostColors(knownHosts.map((h) => h.name));
  } catch (e) {
    view.replaceChildren(message(e.message, "error"));
    return;
  }
  // Drop hosts the config no longer has: a stale link would otherwise 404
  // every query and show nothing at all.
  const names = new Set(knownHosts.map((h) => h.name));
  setHosts(state.hosts.filter((name) => names.has(name)));
  if (!hadHosts && state.hosts.length === 0) {
    setHosts(knownHosts.map((h) => h.name));
  }
  el("sidebar-status").textContent = `scrape every ${formatDuration(
    Number(serverConfig.scrape_interval_ms)
  )}`;
  setDrawer(false);
  commit({ replace: true });
  // Host status moves on its own clock: it is cheap, and stays useful with a
  // pinned window where no chart refresh is running.
  setInterval(() => loadHosts().catch(() => {}), REFRESH_MS);
}

start();
