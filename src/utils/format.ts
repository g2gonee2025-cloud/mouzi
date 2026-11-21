import i18n from "../i18n";

/**
 * Human-readable file size string from bytes.
 * E.g. 1536 → "1.50 KiB", 1048576 → "1.00 MiB"
 */
export function formatBytes(bytes: number): string {
  if (bytes < 0) return "0 B";
  if (bytes === 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const base = 1024;
  let unitIndex = 0;
  let value = bytes;
  while (value >= base && unitIndex < units.length - 1) {
    value /= base;
    unitIndex++;
  }
  return `${value.toFixed(unitIndex === 0 ? 0 : 2)} ${units[unitIndex]}`;
}

// Apply each date's own UTC offset so a daylight-saving change cannot shift the day count.
function localDayNumber(date: Date): number {
  return Math.floor((date.getTime() - date.getTimezoneOffset() * 60_000) / 86_400_000);
}

/** Format Unix seconds relative to the current local calendar day. */
export function formatTimestamp(ts: number | null | undefined): string {
  if (ts == null) return "—";
  const date = new Date(ts * 1000);
  const days = localDayNumber(date) - localDayNumber(new Date());
  const lang = i18n.language;
  if (Math.abs(days) < 7) {
    const label = new Intl.RelativeTimeFormat(lang, { numeric: "auto" }).format(days, "day");
    if (Math.abs(days) > 1) return label;
    const time = date.toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" });
    return `${label} ${time}`;
  }
  return date.toLocaleDateString(lang, { month: "short", day: "numeric", year: "numeric" });
}
