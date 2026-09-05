"use strict";

// Value, tick and timestamp formatting. Shared by the charts and the sidebar.

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
const stampShort = new Intl.DateTimeFormat([], {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

export function fmtBytes(value, decimals) {
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let x = Math.abs(value);
  let i = 0;
  while (x >= 1024 && i < units.length - 1) { x /= 1024; i++; }
  const s = x.toFixed(decimals).replace(/\.0+$/, "");
  return (value < 0 ? "-" : "") + s + " " + units[i];
}

/** Hover-legend values: full precision. */
export function formatValue(value, unit) {
  if (value == null) return "--";
  if (unit === "percent") return value.toFixed(1) + "%";
  if (unit === "bytes") return fmtBytes(value, 2);
  if (unit === "celsius") return value.toFixed(1) + " °C";
  return value.toFixed(1);
}

/** Axis ticks: compact. */
export function formatTick(value, unit) {
  if (value == null) return "";
  if (unit === "percent") return Math.round(value * 10) / 10 + "%";
  if (unit === "bytes") return fmtBytes(value, 1);
  if (unit === "celsius") return Math.round(value) + "°";
  return String(Math.round(value * 100) / 100);
}

export function formatAge(ms) {
  if (ms == null) return "never";
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 60) return seconds + "s ago";
  if (seconds < 3600) return Math.round(seconds / 60) + "m ago";
  if (seconds < 86400) return Math.round(seconds / 3600) + "h ago";
  return Math.round(seconds / 86400) + "d ago";
}

/** A duration in the compact form the range picker uses ("15s", "6h", "7d"). */
export function formatDuration(ms) {
  const trim = (n, unit) => (Number.isInteger(n) ? n : n.toFixed(1)) + unit;
  const seconds = ms / 1000;
  if (seconds < 90) return trim(Math.max(1, Math.round(seconds)), "s");
  const minutes = seconds / 60;
  if (minutes < 90) return trim(Math.round(minutes), "m");
  const hours = minutes / 60;
  if (hours < 48) return trim(hours, "h");
  return trim(hours / 24, "d");
}

export const formatClock = (date) => clockTime.format(date);
export const formatStamp = (ms) => stampShort.format(ms);
export const formatLegendTime = (ms) => legendTime.format(ms);

/** X-axis tick labels; midnight ticks carry the date so multi-day ranges read. */
export function formatAxisTime(seconds) {
  const d = new Date(seconds * 1000);
  return d.getHours() === 0 && d.getMinutes() === 0
    ? monthDay.format(d)
    : timeHM.format(d);
}

/**
 * `datetime-local` input values are local wall-clock strings with no zone, so
 * they need building and parsing by hand to round-trip epoch milliseconds.
 */
export function toLocalInput(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
    `T${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

export function fromLocalInput(text) {
  const ms = new Date(text).getTime();
  return Number.isFinite(ms) ? ms : null;
}
