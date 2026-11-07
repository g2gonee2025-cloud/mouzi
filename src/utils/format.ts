import i18n from "../i18n";

/**
 * Human-readable file size string from bytes.
 * E.g. 1536 → "1.50 KB", 1048576 → "1.00 MB"
 */
export function formatBytes(bytes: number): string {
  if (bytes < 0) return "0 B";
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const base = 1024;
  let unitIndex = 0;
  let value = bytes;
  while (value >= base && unitIndex < units.length - 1) {
    value /= base;
    unitIndex++;
  }
  return `${value.toFixed(unitIndex === 0 ? 0 : 2)} ${units[unitIndex]}`;
}

/**
 * Format a Unix timestamp (seconds) to a short locale-aware date string.
 * Uses Intl.RelativeTimeFormat for today/yesterday/N-days-ago, so dates
 * are automatically translated to the current i18n language.
 * Returns "—" for null/undefined.
 */
export function formatTimestamp(ts: number | null | undefined): string {
  if (ts == null) return "—";
  const d = new Date(ts * 1000);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffDays = Math.floor(diffMs / 86_400_000);
  const lang = i18n.language;
  const time = d.toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" });

  if (diffDays === 0) {
    const rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
    return `${rtf.format(0, "day")} ${time}`;
  }
  if (diffDays === 1) {
    const rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
    return `${rtf.format(-1, "day")} ${time}`;
  }
  if (diffDays < 7) {
    const rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
    return rtf.format(-diffDays, "day");
  }
  return d.toLocaleDateString(lang, { month: "short", day: "numeric", year: "numeric" });
}