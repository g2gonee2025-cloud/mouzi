/** Tailwind utilities backed by the `--color-cat-*` tokens in src/index.css,
 *  so the palette follows the light/dark scheme instead of being baked in
 *  here as hex literals. The seven keys must stay aligned with the
 *  `categorize()` arms in src-tauri/src/scan.rs. */
import type { InventoryFile } from "../store/useDashboardStore";

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

export function categoryLabel(name: string, translate: (key: string) => string): string {
  return Object.prototype.hasOwnProperty.call(CATEGORY_CLASSES, name) ? translate(`categories.${name}`) : name;
}

interface InventoryQuery {
  sort: "size" | "mtime";
  category: string;
  root: string;
  query: string;
  limit: number;
}

interface BrowserRequest extends Omit<InventoryQuery, "category" | "root" | "limit"> {
  category: string | null;
  root: string | null;
  initialLargest: InventoryFile[];
  initialRecent: InventoryFile[];
}

/** Owns response eligibility, including requests invalidated before debounce fires. */
export function createInventoryBrowserLoader(
  fetchRows: (query: InventoryQuery) => Promise<InventoryFile[]>,
  updates: {
    rows: (rows: InventoryFile[]) => void;
    loading: (loading: boolean) => void;
    error: (error: string | null) => void;
  },
) {
  let generation = 0;
  return {
    cancel: () => { ++generation; },
    load: async (request: BrowserRequest) => {
      const current = ++generation;
      const query = request.query.trim();
      updates.error(null);
      if (!query && !request.category && !request.root) {
        updates.rows(request.sort === "size" ? request.initialLargest : request.initialRecent);
        updates.loading(false);
        return;
      }
      updates.loading(true);
      try {
        const rows = await fetchRows({
          sort: request.sort,
          query,
          category: request.category ?? "",
          root: request.root ?? "",
          limit: 80,
        });
        if (current === generation) updates.rows(rows);
      } catch (error) {
        if (current === generation) updates.error(String(error));
      } finally {
        if (current === generation) updates.loading(false);
      }
    },
  };
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
