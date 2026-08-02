"use strict";

const view = document.getElementById("view");
const controls = document.getElementById("controls");
const crumb = document.getElementById("crumb");
const statusEl = document.getElementById("status");

const RANGES = [
  { label: "1h", ms: 3600e3 },
  { label: "6h", ms: 6 * 3600e3 },
  { label: "24h", ms: 24 * 3600e3 },
  { label: "7d", ms: 7 * 24 * 3600e3 },
];
let rangeMs = RANGES[0].ms;
const REFRESH_MS = 30e3;

// Bumped on every route change; in-flight async work compares against it and
// bails instead of touching a page that no longer exists.
let epoch = 0;
let refreshTimer = null;

// One entry per chart card: { group, plot, u } where u is null until the
// metric first has data (it is created lazily on refresh if data appears).
let charts = [];

const PALETTE = ["#5ab0f7", "#4fc26a", "#e0b25d", "#e05d5d", "#b58af7", "#5de0d3"];
const CAPACITY_COLOR = "#8494a5";

// Related metrics rendered together on one chart. `capacity: true` marks a
// "how much exists" series (drawn as a dashed reference line, no fill) as
// opposed to "how much is in use". Instanced metrics (e.g. per mount point)
// fan out into one series per instance, colored per instance; a capacity
// series shares its instance's color.
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
      { name: "disk_used" },
      { name: "disk_total", capacity: true },
    ],
  },
];

// Stored but not charted: the Memory chart (used vs. total) already shows it.
const HIDDEN_METRICS = new Set(["memory_used_percent"]);

// --- formatting ------------------------------------------------------------

const timeHM = new Intl.DateTimeFormat([], {
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});
const monthDay = new Intl.DateTimeFormat([], { month: "short", day: "numeric" });
const legendTime = new Intl.DateTimeFormat([], {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});
const clockTime = new Intl.DateTimeFormat([], {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

function fmtBytes(value, decimals) {
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let x = Math.abs(value);
  let i = 0;
  while (x >= 1024 && i < units.length - 1) { x /= 1024; i++; }
  const s = x.toFixed(decimals).replace(/\.0+$/, "");
  return (value < 0 ? "-" : "") + s + " " + units[i];
}

/** Hover-legend values: full precision. */
function formatValue(value, unit) {
  if (value == null) return "--";
  if (unit === "percent") return value.toFixed(1) + "%";
  if (unit === "bytes") return fmtBytes(value, 2);
  return value.toFixed(1);
}

/** Axis ticks: compact. */
function formatTick(value, unit) {
  if (value == null) return "";
  if (unit === "percent") return Math.round(value * 10) / 10 + "%";
  if (unit === "bytes") return fmtBytes(value, 1);
  return String(Math.round(value * 100) / 100);
}

function formatAge(ms) {
  if (ms == null) return "never";
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 60) return seconds + "s ago";
  if (seconds < 3600) return Math.round(seconds / 60) + "m ago";
  return Math.round(seconds / 3600) + "h ago";
}

// --- fetch helpers ---------------------------------------------------------

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status}`);
  return response.json();
}

function queryWindow(config) {
  const end = Date.now();
  const start = end - rangeMs;
  // Round the step up to a multiple of the scrape interval so every bucket
  // spans at least one sample slot; otherwise empty buckets alias into
  // periodic gaps in the charts. Gaps then only mean genuinely missed scrapes.
  // scrape_interval_ms is a u128 on the wire; past Number.MAX_SAFE_INTEGER it
  // parses imprecisely (or as Infinity), so clamp — any such interval is
  // effectively "huge" and the step math must stay finite.
  const interval = Math.max(
    1000,
    Math.min(Number(config.scrape_interval_ms), Number.MAX_SAFE_INTEGER)
  );
  const step = Math.ceil(Math.max(1, rangeMs / 300) / interval) * interval;
  return { start, end, step };
}

// --- chart plumbing --------------------------------------------------------

const chartByEl = new Map();
const resizeObserver = new ResizeObserver((entries) => {
  for (const entry of entries) {
    const u = chartByEl.get(entry.target);
    if (u) u.setSize({ width: entry.contentRect.width, height: chartHeight() });
  }
});

function chartHeight() {
  return window.innerWidth < 640 ? 170 : 220;
}

function teardown() {
  clearTimeout(refreshTimer);
  refreshTimer = null;
  resizeObserver.disconnect();
  chartByEl.clear();
  for (const chart of charts) chart.u?.destroy();
  charts = [];
  crumb.textContent = "";
  statusEl.textContent = "";
  controls.replaceChildren();
  view.replaceChildren();
}

/**
 * Fetches every metric in a group over one shared time window and flattens
 * the results into uPlot columns plus per-series display definitions.
 * Returns null when no member has any data.
 */
async function loadGroupData(host, group, win) {
  const results = await Promise.all(
    group.metrics.map((m) =>
      fetchJson(
        `/api/query?host=${encodeURIComponent(host)}&metric=${m.name}` +
          `&start=${win.start}&end=${win.end}&step=${win.step}`
      )
    )
  );
  // All queries share start/end/step, so the server returns an identical
  // bucket axis for each; the first non-empty result's axis serves for all.
  let t = null;
  const defs = [];
  const columns = [];
  const colors = new Map();
  const colorFor = (key) => {
    if (!colors.has(key)) colors.set(key, PALETTE[colors.size % PALETTE.length]);
    return colors.get(key);
  };
  group.metrics.forEach((metric, i) => {
    const series = [...results[i].series].sort((a, b) =>
      (a.instance ?? "").localeCompare(b.instance ?? "")
    );
    for (const s of series) {
      t ??= results[i].t;
      defs.push({
        label:
          s.instance != null
            ? s.instance + (metric.capacity ? " total" : "")
            : metric.label ?? metric.name,
        color:
          metric.capacity && s.instance == null
            ? CAPACITY_COLOR
            : colorFor(s.instance ?? metric.name),
        capacity: !!metric.capacity,
      });
      columns.push(s.values);
    }
  });
  if (t == null) return null;
  return { data: [t.map((ms) => ms / 1000)].concat(columns), defs };
}

const gradientFill = (color) => (u) => {
  const { top, height } = u.bbox;
  // uPlot also calls fill accessors while building the legend, before the
  // plot area exists; a plain color then keeps the legend marker working.
  if (!Number.isFinite(top) || !Number.isFinite(height)) return color + "3d";
  const grad = u.ctx.createLinearGradient(0, top, 0, top + height);
  grad.addColorStop(0, color + "3d");
  grad.addColorStop(1, color + "00");
  return grad;
};

/**
 * Sizes the y axis to its widest tick label so values like "16.0 GiB" or
 * "100%" are never clipped. uPlot re-invokes this until the size converges;
 * returning the current size after the first cycle prevents oscillation.
 */
function axisAutoSize(u, values, axisIdx, cycleNum) {
  const axis = u.axes[axisIdx];
  if (cycleNum > 1) return axis._size;
  let size = axis.ticks.size + axis.gap;
  const longest = (values ?? []).reduce((a, v) => (v.length > a.length ? v : a), "");
  if (longest) {
    u.ctx.font = axis.font[0];
    // measureText works in canvas pixels; the returned size must be CSS px.
    size += u.ctx.measureText(longest).width / devicePixelRatio;
  }
  return Math.ceil(size);
}

function xAxisValues(u, ticks) {
  return ticks.map((t) => {
    const d = new Date(t * 1000);
    // Label midnight ticks with the date so multi-day ranges stay readable.
    return d.getHours() === 0 && d.getMinutes() === 0
      ? monthDay.format(d)
      : timeHM.format(d);
  });
}

function makeChart(plot, group, payload) {
  // Fill under the line only when a single "in use" series owns the chart;
  // overlapping fills from several series just turn to mud.
  const primaries = payload.defs.filter((d) => !d.capacity).length;
  const series = [
    {
      label: "time",
      value: (_, v) => (v == null ? "--" : legendTime.format(v * 1000)),
    },
  ].concat(
    payload.defs.map((d) => ({
      label: d.label,
      stroke: d.color,
      width: d.capacity ? 1 : 1.5,
      dash: d.capacity ? [6, 6] : undefined,
      fill: !d.capacity && primaries === 1 ? gradientFill(d.color) : undefined,
      points: { show: false },
      value: (_, v) => formatValue(v, group.unit),
    }))
  );
  const axisStyle = {
    stroke: "#8494a5",
    grid: { stroke: "#232c36", width: 1 },
    ticks: { stroke: "#232c36" },
  };
  const opts = {
    width: plot.clientWidth,
    height: chartHeight(),
    // Right padding keeps the last x-axis label from being clipped at the
    // canvas edge.
    padding: [10, 14, 0, 4],
    series,
    axes: [
      { ...axisStyle, values: xAxisValues },
      {
        ...axisStyle,
        values: (_, ticks) => ticks.map((v) => formatTick(v, group.unit)),
        size: axisAutoSize,
        gap: 8,
      },
    ],
    scales:
      group.unit === "percent"
        ? { y: { range: [0, 100] } }
        : { y: { range: (_, min, max) => [0, max > 0 ? max * 1.05 : 1] } },
    cursor: {
      // One hover cursor shared across every chart on the page.
      sync: { key: "silph" },
      focus: { prox: 24 },
      points: { size: 5 },
    },
    focus: { alpha: 0.4 },
  };
  const u = new uPlot(opts, payload.data, plot);
  chartByEl.set(plot, u);
  resizeObserver.observe(plot);
  return u;
}

function setUpdated() {
  statusEl.textContent = "updated " + clockTime.format(new Date());
}

// --- host list -------------------------------------------------------------

function statusCell(host) {
  const td = document.createElement("td");
  const pill = document.createElement("span");
  pill.className = "pill " + (host.up ? "up" : "down");
  pill.textContent = host.up ? "up" : "down";
  td.appendChild(pill);
  if (host.error) {
    const err = document.createElement("div");
    err.className = "error small";
    err.textContent = host.error;
    td.appendChild(err);
  }
  return td;
}

async function renderHostList() {
  const myEpoch = epoch;
  const hosts = await fetchJson("/api/hosts");
  if (myEpoch !== epoch) return;

  const panel = document.createElement("div");
  panel.className = "panel";
  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>host</th><th>status</th><th>last scrape</th></tr></thead>";
  const tbody = document.createElement("tbody");
  if (hosts.length === 0) {
    tbody.innerHTML =
      '<tr><td colspan="3" class="muted empty">no hosts scraped yet</td></tr>';
  }
  for (const host of hosts) {
    const row = document.createElement("tr");
    row.className = "host";
    row.onclick = () => (location.hash = "#/host/" + encodeURIComponent(host.name));
    const name = document.createElement("td");
    name.className = "host-name";
    const dot = document.createElement("span");
    dot.className = "dot " + (host.up ? "up" : "down");
    name.append(dot, host.name);
    const age = document.createElement("td");
    age.className = "muted";
    age.textContent = formatAge(host.last_scrape_ms);
    row.append(name, statusCell(host), age);
    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  panel.appendChild(table);
  view.replaceChildren(panel);
  setUpdated();
  refreshTimer = setTimeout(route, REFRESH_MS);
}

// --- per-host charts -------------------------------------------------------

function renderRangePicker() {
  const seg = document.createElement("div");
  seg.className = "seg";
  for (const range of RANGES) {
    const button = document.createElement("button");
    button.textContent = range.label;
    button.className = range.ms === rangeMs ? "active" : "";
    button.onclick = () => { rangeMs = range.ms; route(); };
    seg.appendChild(button);
  }
  controls.appendChild(seg);
}

/** Charts for GROUPS plus a single-metric chart for anything not covered. */
function buildGroups(metrics) {
  const available = new Set(metrics.map((m) => m.name));
  const known = new Set(GROUPS.flatMap((g) => g.metrics.map((m) => m.name)));
  const groups = GROUPS.map((g) => ({
    ...g,
    metrics: g.metrics.filter((m) => available.has(m.name)),
  })).filter((g) => g.metrics.length > 0);
  for (const m of metrics) {
    if (!known.has(m.name) && !HIDDEN_METRICS.has(m.name)) {
      groups.push({ title: m.name, unit: m.unit, metrics: [{ name: m.name }] });
    }
  }
  return groups;
}

async function renderHost(name) {
  const myEpoch = epoch;
  renderRangePicker();
  crumb.textContent = "/ " + name;

  const [metrics, config] = await Promise.all([
    fetchJson("/api/metrics"),
    fetchJson("/api/config"),
  ]);
  if (myEpoch !== epoch) return;

  const win = queryWindow(config);
  const grid = document.createElement("div");
  grid.className = "grid";
  view.appendChild(grid);

  charts = buildGroups(metrics).map((group) => {
    const card = document.createElement("section");
    card.className = "chart";
    const head = document.createElement("header");
    const title = document.createElement("h3");
    title.textContent = group.title;
    const unit = document.createElement("span");
    unit.className = "unit";
    unit.textContent = group.unit;
    head.append(title, unit);
    const plot = document.createElement("div");
    plot.className = "plot loading";
    card.append(head, plot);
    grid.appendChild(card);
    return { group, plot, u: null };
  });

  await Promise.all(
    charts.map(async (chart) => {
      try {
        const payload = await loadGroupData(name, chart.group, win);
        if (myEpoch !== epoch) return;
        chart.plot.classList.remove("loading");
        if (!payload) {
          chart.plot.innerHTML = '<div class="muted empty">no data</div>';
          return;
        }
        chart.plot.replaceChildren();
        chart.u = makeChart(chart.plot, chart.group, payload);
      } catch (e) {
        if (myEpoch !== epoch) return;
        chart.plot.classList.remove("loading");
        chart.plot.innerHTML = "";
        const err = document.createElement("div");
        err.className = "error empty";
        err.textContent = e.message;
        chart.plot.appendChild(err);
      }
    })
  );
  if (myEpoch !== epoch) return;
  setUpdated();
  refreshTimer = setTimeout(() => refreshHost(name, config), REFRESH_MS);
}

/**
 * Slides the time window forward and swaps new data into the existing charts
 * in place — no rebuild, no flicker. A chart whose series set changed (e.g. a
 * new mount point appeared) or that just got its first data is recreated.
 */
async function refreshHost(name, config) {
  const myEpoch = epoch;
  const win = queryWindow(config);
  await Promise.all(
    charts.map(async (chart) => {
      try {
        const payload = await loadGroupData(name, chart.group, win);
        if (myEpoch !== epoch || !payload) return;
        if (chart.u && payload.data.length === chart.u.data.length) {
          chart.u.setData(payload.data);
        } else {
          chart.u?.destroy();
          chartByEl.delete(chart.plot);
          chart.plot.classList.remove("loading");
          chart.plot.replaceChildren();
          chart.u = makeChart(chart.plot, chart.group, payload);
        }
      } catch {
        // Keep showing the last good data; the next refresh retries.
      }
    })
  );
  if (myEpoch !== epoch) return;
  setUpdated();
  refreshTimer = setTimeout(() => refreshHost(name, config), REFRESH_MS);
}

// --- routing ---------------------------------------------------------------

function route() {
  epoch++;
  teardown();
  const match = location.hash.match(/^#\/host\/(.+)$/);
  const render = match
    ? renderHost(decodeURIComponent(match[1]))
    : renderHostList();
  render.catch((e) => {
    const err = document.createElement("div");
    err.className = "error empty";
    err.textContent = e.message;
    view.replaceChildren(err);
  });
}

window.addEventListener("hashchange", route);
route();
