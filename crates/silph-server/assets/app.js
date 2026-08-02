"use strict";

const view = document.getElementById("view");
const controls = document.getElementById("controls");

const RANGES = [
  { label: "1h", ms: 3600e3 },
  { label: "6h", ms: 6 * 3600e3 },
  { label: "24h", ms: 24 * 3600e3 },
  { label: "7d", ms: 7 * 24 * 3600e3 },
];
let rangeMs = RANGES[0].ms;
let charts = [];
let refreshTimer = null;

const PALETTE = ["#5ab0f7", "#4fc26a", "#e0b25d", "#e05d5d", "#b58af7", "#5de0d3"];

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status}`);
  return response.json();
}

function formatValue(value, unit) {
  if (value == null) return "-";
  if (unit === "percent") return value.toFixed(1) + "%";
  if (unit === "bytes") {
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let i = 0;
    while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
    return value.toFixed(1) + " " + units[i];
  }
  return value.toFixed(1);
}

function formatAge(ms) {
  if (ms == null) return "never";
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 60) return seconds + "s ago";
  if (seconds < 3600) return Math.round(seconds / 60) + "m ago";
  return Math.round(seconds / 3600) + "h ago";
}

function teardown() {
  clearTimeout(refreshTimer);
  refreshTimer = null;
  charts.forEach((c) => c.destroy());
  charts = [];
  controls.replaceChildren();
  view.replaceChildren();
}

// --- host list -------------------------------------------------------------

async function renderHostList() {
  const hosts = await fetchJson("/api/hosts");
  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>host</th><th>last scrape</th><th>status</th></tr></thead>";
  const tbody = document.createElement("tbody");
  if (hosts.length === 0) {
    tbody.innerHTML =
      '<tr><td colspan="3" class="muted">no hosts scraped yet</td></tr>';
  }
  for (const host of hosts) {
    const row = document.createElement("tr");
    row.className = "host";
    row.onclick = () => (location.hash = "#/host/" + encodeURIComponent(host.name));
    const dot = `<span class="dot ${host.up ? "up" : "down"}"></span>`;
    row.innerHTML =
      `<td>${dot}${host.name}</td>` +
      `<td class="muted">${formatAge(host.last_scrape_ms)}</td>` +
      `<td>${host.error ? `<span class="error">${host.error}</span>` : host.up ? "up" : "down"}</td>`;
    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  view.replaceChildren(table);
  refreshTimer = setTimeout(route, 30e3);
}

// --- per-host charts -------------------------------------------------------

function renderRangePicker() {
  for (const range of RANGES) {
    const button = document.createElement("button");
    button.textContent = range.label;
    button.className = range.ms === rangeMs ? "active" : "";
    button.onclick = () => { rangeMs = range.ms; route(); };
    controls.appendChild(button);
  }
}

function makeChart(container, metric, data, seriesNames) {
  const series = [{}].concat(
    seriesNames.map((name, i) => ({
      label: name,
      stroke: PALETTE[i % PALETTE.length],
      width: 1.5,
      value: (_, v) => formatValue(v, metric.unit),
    }))
  );
  const opts = {
    width: container.clientWidth - 24,
    height: 180,
    series,
    axes: [
      { stroke: "#7d8a99", grid: { stroke: "#2a323c" } },
      {
        stroke: "#7d8a99",
        grid: { stroke: "#2a323c" },
        values: (_, ticks) => ticks.map((v) => formatValue(v, metric.unit)),
      },
    ],
    scales: metric.unit === "percent" ? { y: { range: [0, 100] } } : {},
  };
  charts.push(new uPlot(opts, data, container));
}

async function renderHost(name) {
  renderRangePicker();
  const title = document.createElement("h2");
  title.textContent = name;
  view.appendChild(title);

  const [metrics, config] = await Promise.all([
    fetchJson("/api/metrics"),
    fetchJson("/api/config"),
  ]);
  const end = Date.now();
  const start = end - rangeMs;
  // Round the step up to a multiple of the scrape interval so every bucket
  // spans at least one sample slot; otherwise empty buckets alias into
  // periodic gaps in the charts. Gaps then only mean genuinely missed scrapes.
  const interval = Math.max(1000, config.scrape_interval_ms);
  const step = Math.ceil(Math.max(1, rangeMs / 300) / interval) * interval;

  for (const metric of metrics) {
    const box = document.createElement("div");
    box.className = "chart";
    box.innerHTML = `<h3>${metric.name}</h3>`;
    view.appendChild(box);
    try {
      const result = await fetchJson(
        `/api/query?host=${encodeURIComponent(name)}&metric=${metric.name}` +
          `&start=${start}&end=${end}&step=${step}`
      );
      if (result.series.length === 0) {
        box.insertAdjacentHTML("beforeend", '<div class="muted">no data</div>');
        continue;
      }
      const data = [result.t.map((t) => t / 1000)].concat(
        result.series.map((s) => s.values)
      );
      const names = result.series.map((s) => s.instance ?? metric.name);
      makeChart(box, metric, data, names);
    } catch (e) {
      box.insertAdjacentHTML("beforeend", `<div class="error">${e.message}</div>`);
    }
  }
  refreshTimer = setTimeout(route, 30e3);
}

// --- routing ---------------------------------------------------------------

function route() {
  teardown();
  const match = location.hash.match(/^#\/host\/(.+)$/);
  const render = match
    ? renderHost(decodeURIComponent(match[1]))
    : renderHostList();
  render.catch((e) => {
    view.innerHTML = `<div class="error">${e.message}</div>`;
  });
}

window.addEventListener("hashchange", route);
route();
