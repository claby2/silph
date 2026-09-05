"use strict";

// App state: which hosts are selected, which time window is shown, and the
// stable per-host colours. All of it round-trips through the URL hash so a
// view can be bookmarked or shared; localStorage only seeds the first visit.

const STORAGE_KEY = "silph.view";

export const RANGES = [
  { label: "15m", ms: 15 * 60e3 },
  { label: "1h", ms: 3600e3 },
  { label: "6h", ms: 6 * 3600e3 },
  { label: "24h", ms: 24 * 3600e3 },
  { label: "7d", ms: 7 * 24 * 3600e3 },
  { label: "30d", ms: 30 * 24 * 3600e3 },
];

/** Zooming can't go below one minute or past a year; both ends are useless. */
export const MIN_WINDOW_MS = 60e3;
export const MAX_WINDOW_MS = 365 * 24 * 3600e3;

const DEFAULT_RANGE = { kind: "relative", ms: 3600e3 };

export const state = {
  /** Selected host names, in the order the user picked them. */
  hosts: [],
  /**
   * Either `{kind: "relative", ms}` — a window ending now, which keeps
   * refreshing — or `{kind: "absolute", from, to}`, which is pinned.
   */
  range: { ...DEFAULT_RANGE },
};

const listeners = new Set();

/**
 * The hash this module last wrote. Setting `location.hash` fires a hashchange
 * event, and re-reading state from it would render and re-query everything a
 * second time for every click; the app skips the event when it recognises its
 * own write.
 */
let ownHash = null;
export const isOwnHash = () => location.hash === ownHash;

export function onChange(fn) {
  listeners.add(fn);
}

/** Publishes the current state to the hash, storage and every listener. */
export function commit({ replace = false } = {}) {
  writeHash(replace);
  try {
    localStorage.setItem(STORAGE_KEY, location.hash.slice(1));
  } catch {
    // Private browsing or a blocked origin: the hash alone still works.
  }
  for (const fn of listeners) fn();
}

export function isLive() {
  return state.range.kind === "relative";
}

/** The concrete window to query, in epoch milliseconds. */
export function timeWindow() {
  if (state.range.kind === "absolute") {
    return { start: state.range.from, end: state.range.to };
  }
  const end = Date.now();
  return { start: end - state.range.ms, end };
}

export function windowMs() {
  const win = timeWindow();
  return win.end - win.start;
}

/** Pins the view to an explicit window, clamped to sane bounds. */
export function setAbsolute(from, to) {
  let span = Math.min(MAX_WINDOW_MS, Math.max(MIN_WINDOW_MS, to - from));
  const centre = (from + to) / 2;
  state.range = {
    kind: "absolute",
    from: Math.round(centre - span / 2),
    to: Math.round(centre + span / 2),
  };
}

export function setRelative(ms) {
  state.range = { kind: "relative", ms };
}

/** Zooms about the window's centre; `factor > 1` zooms out. */
export function zoomBy(factor) {
  const win = timeWindow();
  const centre = (win.start + win.end) / 2;
  const span = (win.end - win.start) * factor;
  setAbsolute(centre - span / 2, centre + span / 2);
}

/** Shifts the window by a fraction of its own width; `+1` is one page later. */
export function panBy(fraction) {
  const win = timeWindow();
  const shift = (win.end - win.start) * fraction;
  setAbsolute(win.start + shift, win.end + shift);
}

export function toggleHost(name) {
  const index = state.hosts.indexOf(name);
  if (index === -1) state.hosts.push(name);
  else state.hosts.splice(index, 1);
}

export function setHosts(names) {
  state.hosts = [...names];
}

// --- URL hash --------------------------------------------------------------

function writeHash(replace) {
  const params = new URLSearchParams();
  if (state.hosts.length) params.set("hosts", state.hosts.join(","));
  if (state.range.kind === "relative") {
    params.set("range", String(state.range.ms));
  } else {
    params.set("from", String(state.range.from));
    params.set("to", String(state.range.to));
  }
  const hash = "#" + params.toString();
  ownHash = hash;
  if (hash === location.hash) return;
  // A replace keeps drags and refreshes out of the back stack; only explicit
  // picks (a host, a range button) are worth a history entry.
  if (replace) history.replaceState(null, "", hash);
  else location.hash = hash;
}

/**
 * Reads state back out of the hash, falling back to the last visit's view.
 * Returns true if anything was restored, so the caller knows whether to seed
 * a default selection.
 */
export function readHash() {
  let raw = location.hash.slice(1);
  if (!raw) {
    try {
      raw = localStorage.getItem(STORAGE_KEY) ?? "";
    } catch {
      raw = "";
    }
  }
  const params = new URLSearchParams(raw);
  const hosts = params.get("hosts");
  state.hosts = hosts ? hosts.split(",").filter(Boolean) : [];
  const from = Number(params.get("from"));
  const to = Number(params.get("to"));
  if (Number.isFinite(from) && Number.isFinite(to) && to > from) {
    setAbsolute(from, to);
  } else {
    const ms = Number(params.get("range"));
    state.range = ms > 0 ? { kind: "relative", ms } : { ...DEFAULT_RANGE };
  }
  return params.has("hosts");
}


// --- colours ---------------------------------------------------------------

/**
 * The categorical palette, stepped for a dark surface and kept in its
 * published order (blue, orange, aqua, yellow, magenta, green, violet, red).
 *
 * How far this can be pushed was measured, not guessed: on this surface, with
 * every series drawn over every other, at most four colours clear both the
 * colour-vision and normal-vision separation gates, five to eight sit in the
 * "legal only with secondary encoding" band, and nothing clears past eleven —
 * no palette fixes that. So colour identifies a host at a glance for a handful
 * of hosts and is a hint beyond that; the legend, the crosshair readout and
 * hovering a host in the sidebar (which dims every other host's lines) are what
 * actually name a series in a crowded chart.
 */
const PALETTE = [
  "#3987e5",
  "#d95926",
  "#199e70",
  "#c98500",
  "#d55181",
  "#008300",
  "#9085e9",
  "#e66767",
];

/**
 * FNV-1a over the name. A host's colour must be a pure function of its name so
 * it stays the same whatever else is selected, on every chart, across reloads.
 */
function hash(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

// --- OKLCH, so a colour can be nudged in lightness or hue without drifting in
// apparent weight the way the same move in HSL would.

const toLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const toSrgb = (c) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

function oklch(hex) {
  const [r, g, b] = [1, 3, 5].map((i) => toLinear(parseInt(hex.slice(i, i + 2), 16) / 255));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const lightness = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return { lightness, chroma: Math.hypot(a, bb), hue: Math.atan2(bb, a) };
}

function toHex({ lightness, chroma, hue }) {
  const a = Math.cos(hue) * chroma;
  const b = Math.sin(hue) * chroma;
  const l = (lightness + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (lightness - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (lightness - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

/**
 * Moves a palette colour by `dLightness` and `dHue` (degrees). A lifted or
 * rotated colour can leave the sRGB gamut; chroma is pulled in until it fits,
 * which desaturates rather than clipping a channel and skewing the hue.
 */
function shift(hex, dLightness, dHue) {
  const base = oklch(hex);
  const target = {
    lightness: Math.max(0.4, Math.min(0.84, base.lightness + dLightness)),
    hue: base.hue + (dHue * Math.PI) / 180,
    chroma: base.chroma,
  };
  for (let scale = 1; scale > 0.28; scale -= 0.02) {
    const rgb = toHex({ ...target, chroma: base.chroma * scale });
    if (rgb.every((c) => c >= -0.0005 && c <= 1.0005)) {
      return (
        "#" +
        rgb
          .map((c) =>
            Math.round(255 * toSrgb(Math.max(0, Math.min(1, c))))
              .toString(16)
              .padStart(2, "0")
          )
          .join("")
      );
    }
  }
  return hex;
}

/** host name -> {slot, cycle}, rebuilt whenever the host roster changes. */
let placements = new Map();

function place(host) {
  return placements.get(host) ?? { slot: hash(host) % PALETTE.length, cycle: 0 };
}

/**
 * Picks each host's palette slot from its name hash. Two hosts can hash to the
 * same slot, which would make them one colour in an aggregate chart, so
 * collisions probe forward; past the eighth host the palette is exhausted and
 * further hosts take a lighter step of a slot they share. Sorting the names
 * first keeps the result independent of the order the server lists hosts in,
 * so it only changes when the roster does.
 */
export function assignHostColors(names) {
  placements = new Map();
  const used = new Array(PALETTE.length).fill(0);
  for (const name of [...names].sort()) {
    let slot = hash(name) % PALETTE.length;
    for (let i = 0; i < PALETTE.length && used[slot] > 0; i++) {
      slot = (slot + 1) % PALETTE.length;
    }
    placements.set(name, { slot, cycle: used[slot] });
    used[slot]++;
  }
}

/**
 * The colour for one series. The host picks the palette slot; a host's
 * instances (mount points, sensors) fan out around it in lightness and hue so
 * they read as that host's family rather than as unrelated series, and a group
 * with several "in use" metrics nudges further again. `rank`/`count` place an
 * instance within its own host's instances.
 */
export function seriesColor(host, rank = 0, count = 1, metricOffset = 0) {
  const { slot, cycle } = place(host);
  const offset = count > 1 ? rank - (count - 1) / 2 : 0;
  return shift(
    PALETTE[slot],
    cycle * 0.13 + offset * Math.min(0.06, 0.36 / count),
    offset * Math.min(16, 44 / count) + metricOffset * 12
  );
}

/** The sidebar swatch: the host's own colour, without any instance spread. */
export const hostColor = (host) => seriesColor(host);
