"use strict";

// uPlot plumbing: chart construction, plus the pointer and touch gestures that
// drive the shared time window. Every live chart registers here so a gesture on
// one moves all of them together.

import {
  formatAxisTime,
  formatLegendTime,
  formatTick,
  formatValue,
} from "/assets/format.js";
import { MAX_WINDOW_MS, MIN_WINDOW_MS } from "/assets/state.js";

const AXIS_STROKE = "#8494a5";
const GRID_STROKE = "#232c36";

/** Live charts, in creation order; gestures fan out across all of them. */
const live = new Set();

/**
 * Set while a two-finger gesture is in flight. The x scales are then being
 * driven directly and a background refresh would yank them back, so the app
 * checks this before swapping data in.
 */
let gesturing = false;
export const isGesturing = () => gesturing;

/**
 * Callbacks the app installs once: `onWindow(from, to)` commits a new absolute
 * window (epoch ms), `getWindow()` reads the current one.
 */
let onWindow = () => {};
export function setWindowHandler(fn) {
  onWindow = fn;
}

export function chartHeight() {
  return window.innerWidth < 700 ? 176 : 224;
}

const chartByEl = new Map();
const resizeObserver = new ResizeObserver((entries) => {
  for (const entry of entries) {
    const u = chartByEl.get(entry.target);
    // A zero width means the element is hidden (an off-canvas sidebar, a
    // collapsed card); resizing to it would wreck the layout on the way back.
    if (u && entry.contentRect.width > 0) {
      u.setSize({ width: entry.contentRect.width, height: chartHeight() });
    }
  }
});

/**
 * Dims every series that is not `host`'s, across every chart at once, and
 * restores them when passed null. With more hosts on a chart than colour alone
 * can separate, pointing at one in the sidebar is what actually isolates it.
 */
export function focusHost(host) {
  for (const u of live) {
    let changed = false;
    for (let i = 1; i < u.series.length; i++) {
      const series = u.series[i];
      const alpha = host == null || series.host === host ? 1 : 0.12;
      if (series.alpha !== alpha) {
        series.alpha = alpha;
        changed = true;
      }
    }
    // Redraw without rebuilding paths or rescaling: only the alphas moved.
    if (changed) u.redraw(false, false);
  }
}

export function destroyChart(u) {
  if (!u) return;
  live.delete(u);
  for (const [el, chart] of chartByEl) {
    if (chart === u) {
      resizeObserver.unobserve(el);
      chartByEl.delete(el);
    }
  }
  u.destroy();
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

/**
 * Builds a chart for one panel. `payload` is `{data, defs}` as produced by the
 * app's loader: uPlot columns plus a display definition per series.
 */
export function createChart(el, group, payload) {
  // Fill under the line only when a single "in use" series owns the chart;
  // overlapping fills from several series just turn to mud.
  const primaries = payload.defs.filter((d) => !d.capacity).length;
  const series = [
    {
      label: "time",
      value: (_, v) => (v == null ? "--" : formatLegendTime(v * 1000)),
    },
  ].concat(
    payload.defs.map((d) => ({
      label: d.label,
      // Read back off u.series by focusHost; uPlot copies unknown keys through.
      host: d.host,
      stroke: d.color,
      width: d.capacity ? 1 : 1.5,
      dash: d.capacity ? [6, 6] : undefined,
      fill: !d.capacity && primaries === 1 ? gradientFill(d.color) : undefined,
      points: { show: false },
      value: (_, v) => formatValue(v, group.unit),
    }))
  );
  const axisStyle = {
    stroke: AXIS_STROKE,
    grid: { stroke: GRID_STROKE, width: 1 },
    ticks: { stroke: GRID_STROKE },
  };
  const u = new uPlot(
    {
      width: el.clientWidth,
      height: chartHeight(),
      // Right padding keeps the last x-axis label from being clipped at the
      // canvas edge.
      padding: [10, 14, 0, 4],
      series,
      axes: [
        { ...axisStyle, values: (_, ticks) => ticks.map(formatAxisTime) },
        {
          ...axisStyle,
          values: (_, ticks) => ticks.map((v) => formatTick(v, group.unit)),
          size: axisAutoSize,
          gap: 8,
        },
      ],
      scales: {
        // The x range is the app's window, not the data's extent, so a chart
        // whose host stopped reporting still lines up with its neighbours.
        x: { time: true },
        ...(group.unit === "percent"
          ? { y: { range: [0, 100] } }
          : { y: { range: (_, min, max) => [0, max > 0 ? max * 1.05 : 1] } }),
      },
      cursor: {
        // One hover cursor shared across every chart on the page.
        sync: { key: "silph" },
        focus: { prox: 24 },
        points: { size: 6 },
        // Drag selects a time range; the app turns that into the new window,
        // so uPlot must not also rescale locally.
        drag: { x: true, y: false, setScale: false, dist: 6 },
        // uPlot's own dblclick resets the local scale, which fights the
        // window the app owns; the handler below zooms out instead.
        bind: { dblclick: () => null },
      },
      focus: { alpha: 0.4 },
      legend: { live: true },
      hooks: {
        setSelect: [
          (self) => {
            const { left, width } = self.select;
            if (width > 4) {
              const from = self.posToVal(left, "x") * 1000;
              const to = self.posToVal(left + width, "x") * 1000;
              onWindow(from, to);
            }
            self.setSelect({ left: 0, width: 0, top: 0, height: 0 }, false);
          },
        ],
      },
    },
    payload.data,
    el
  );
  live.add(u);
  chartByEl.set(el, u);
  resizeObserver.observe(el);
  attachGestures(u);
  return u;
}

/** Applies the app's window to a chart's x scale (uPlot works in seconds). */
export function applyWindow(u, start, end) {
  u.setScale("x", { min: start / 1000, max: end / 1000 });
}

// --- gestures --------------------------------------------------------------

function clampSpan(span) {
  return Math.min(MAX_WINDOW_MS / 1000, Math.max(MIN_WINDOW_MS / 1000, span));
}

/** Widens the current window about its centre, then hands it to the app. */
function zoomOut(u) {
  const { min, max } = u.scales.x;
  if (min == null || max == null) return;
  const centre = (min + max) / 2;
  const span = clampSpan((max - min) * 2);
  onWindow((centre - span / 2) * 1000, (centre + span / 2) * 1000);
}

/** Position of a touch in CSS pixels relative to the plot overlay. */
function localX(touch, rect) {
  return touch.clientX - rect.left;
}

function attachGestures(u) {
  const over = u.over;
  over.addEventListener("dblclick", () => zoomOut(u));

  // Touch state. `mode` is null until the first move decides what the gesture
  // is: "scrub" (one finger, mostly horizontal) or "zoom" (two fingers).
  let mode = null;
  let startX = 0;
  let startY = 0;
  let lastTapMs = 0;
  /** Two-finger anchors: screen positions and the values they must keep. */
  let anchors = null;

  const scrub = (touch) => {
    // uPlot has no touch handling of its own, but its mouse path already does
    // exactly what a scrub needs — cursor, legend and cross-chart sync — so
    // replay the touch as a mouse move rather than reimplementing it.
    over.dispatchEvent(
      new MouseEvent("mousemove", {
        clientX: touch.clientX,
        clientY: touch.clientY,
        bubbles: true,
      })
    );
  };

  const beginZoom = (touches) => {
    const rect = over.getBoundingClientRect();
    if (rect.width <= 0) return;
    const a = localX(touches[0], rect);
    const b = localX(touches[1], rect);
    anchors = {
      width: rect.width,
      va: u.posToVal(a, "x"),
      vb: u.posToVal(b, "x"),
      min: u.scales.x.min,
      max: u.scales.x.max,
    };
    mode = "zoom";
    gesturing = true;
  };

  const moveZoom = (touches) => {
    if (!anchors) return;
    const rect = over.getBoundingClientRect();
    const qa = localX(touches[0], rect);
    const qb = localX(touches[1], rect);
    // Keep the value under each finger pinned: solve v = min + q * perPx for
    // the two anchors. Guard the degenerate case of both fingers at one x.
    const dq = qb - qa;
    if (Math.abs(dq) < 8) return;
    const span = clampSpan(((anchors.vb - anchors.va) / dq) * anchors.width);
    const perPx = span / anchors.width;
    const min = anchors.va - qa * perPx;
    for (const chart of live) applyWindow(chart, min * 1000, (min + span) * 1000);
  };

  const endZoom = () => {
    const start = anchors;
    anchors = null;
    gesturing = false;
    const { min, max } = u.scales.x;
    if (min == null || max == null || start == null) return;
    // Two fingers put down and lifted again moved nothing; committing anyway
    // would pin a live window for no reason.
    const moved = Math.abs(min - start.min) + Math.abs(max - start.max);
    if (moved > (max - min) / 200) onWindow(min * 1000, max * 1000);
  };

  over.addEventListener(
    "touchstart",
    (e) => {
      if (e.touches.length >= 2) {
        beginZoom(e.touches);
        return;
      }
      mode = null;
      startX = e.touches[0].clientX;
      startY = e.touches[0].clientY;
      const now = Date.now();
      if (now - lastTapMs < 320) {
        lastTapMs = 0;
        zoomOut(u);
        return;
      }
      lastTapMs = now;
      scrub(e.touches[0]);
    },
    { passive: true }
  );

  over.addEventListener(
    "touchmove",
    (e) => {
      if (e.touches.length >= 2) {
        if (mode !== "zoom") beginZoom(e.touches);
        else moveZoom(e.touches);
        if (e.cancelable) e.preventDefault();
        return;
      }
      if (mode === "zoom") return;
      const touch = e.touches[0];
      const dx = Math.abs(touch.clientX - startX);
      const dy = Math.abs(touch.clientY - startY);
      // `touch-action: pan-y` leaves vertical scrolling to the browser, so
      // only take over once the gesture is clearly a horizontal drag.
      if (mode !== "scrub" && (dx < 8 || dx < dy)) return;
      mode = "scrub";
      lastTapMs = 0;
      if (e.cancelable) e.preventDefault();
      scrub(touch);
    },
    { passive: false }
  );

  const finish = (e) => {
    if (mode === "zoom" && e.touches.length < 2) {
      endZoom();
      mode = null;
    }
  };
  over.addEventListener("touchend", finish, { passive: true });
  over.addEventListener("touchcancel", finish, { passive: true });
}
