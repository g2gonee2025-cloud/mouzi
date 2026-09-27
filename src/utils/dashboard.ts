/** Tailwind utilities backed by the `--color-cat-*` tokens in src/index.css,
 *  so the palette follows the light/dark scheme instead of being baked in
 *  here as hex literals. The seven keys must stay aligned with the
 *  `categorize()` arms in src-tauri/src/scan.rs. */
export const CATEGORY_CLASSES: Record<string, string> = {
  Documents: "bg-cat-documents",
  Images: "bg-cat-images",
  Videos: "bg-cat-videos",
  Audio: "bg-cat-audio",
  Archives: "bg-cat-archives",
  Code: "bg-cat-code",
  Other: "bg-cat-other",
};

export const AGE_BUCKETS = ["7d", "30d", "90d", "365d", "older"] as const;

export function categoryClass(name: string): string {
  return CATEGORY_CLASSES[name] ?? CATEGORY_CLASSES.Other;
}

export interface CategoryShare {
  category: string;
  files: number;
  bytes: number;
}

export function leadCategory(
  data: CategoryShare[],
  totalBytes: number,
): { category: string; percent: number } | null {
  if (!data.length || totalBytes <= 0) return null;
  const top = [...data].sort((a, b) => b.bytes - a.bytes)[0];
  if (!top || top.bytes <= 0) return null;
  return {
    category: top.category,
    percent: Math.round((top.bytes / totalBytes) * 100),
  };
}

export function percentOf(part: number, total: number): number {
  if (total <= 0 || part <= 0) return 0;
  return Math.max(0, Math.min(100, (part / total) * 100));
}

/** Rounding a real non-zero share down to a flat "0%" is its own small lie,
 *  so sub-half shares read "<1%" instead. Numeric only, no i18n needed. */
export function formatShare(percent: number): string {
  if (percent <= 0) return "0%";
  if (percent < 0.5) return "<1%";
  return `${Math.round(percent)}%`;
}
