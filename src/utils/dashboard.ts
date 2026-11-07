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

export interface DashboardViewOptions {
  sort: "size" | "mtime";
  query: string;
  category: string | null;
  root: string | null;
}

export const DEFAULT_DASHBOARD_VIEW: DashboardViewOptions = {
  sort: "size", query: "", category: null, root: null,
};

export function clearMissingDashboardRoot(view: DashboardViewOptions, missingRoot: string): DashboardViewOptions {
  return view.root === missingRoot ? { ...view, root: null } : view;
}

interface InventoryQuery {
  sort: DashboardViewOptions["sort"];
  category: string;
  root: string;
  query: string;
  limit: number;
}

interface BrowserScope extends DashboardViewOptions {
  rootScope: string;
}

interface BrowserRequest extends BrowserScope {
  initialLargest: InventoryFile[];
  initialRecent: InventoryFile[];
}

export interface InventoryBrowserView {
  scope: string;
  rows: InventoryFile[];
  limited: boolean;
  loading: boolean;
  error: string | null;
}

export const INVENTORY_MATCH_LIMIT = 80;

export function inventoryBrowserScope({ sort, query, category, root, rootScope }: BrowserScope): string {
  return JSON.stringify([sort, query.trim(), category, root, rootScope]);
}

export function createInventoryBrowserLoader(
  fetchRows: (query: InventoryQuery) => Promise<InventoryFile[]>,
  update: (view: InventoryBrowserView) => void,
) {
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let view: InventoryBrowserView | undefined;

  const publish = (next: InventoryBrowserView) => {
    view = next;
    update(next);
  };
  const cancel = () => {
    ++generation;
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };

  return {
    cancel,
    load: (request: BrowserRequest, debounceMs = 0) => {
      cancel();
      const current = generation;
      const scope = inventoryBrowserScope(request);
      const query = request.query.trim();
      if (!query && !request.category && !request.root) {
        publish({
          scope,
          rows: request.sort === "size" ? request.initialLargest : request.initialRecent,
          limited: false,
          loading: false,
          error: null,
        });
        return;
      }

      const previous = view?.scope === scope ? view : undefined;
      const pending = {
        scope, rows: previous?.rows ?? [], limited: previous?.limited ?? false, loading: true, error: null,
      };
      publish(pending);

      const fetch = async () => {
        try {
          const rows = await fetchRows({
            sort: request.sort,
            query,
            category: request.category ?? "",
            root: request.root ?? "",
            limit: INVENTORY_MATCH_LIMIT + 1,
          });
          if (current === generation) {
            publish({
              scope,
              rows: rows.slice(0, INVENTORY_MATCH_LIMIT),
              limited: rows.length > INVENTORY_MATCH_LIMIT,
              loading: false,
              error: null,
            });
          }
        } catch (error) {
          if (current === generation) publish({ ...pending, loading: false, error: String(error) });
        }
      };

      if (query && !previous && debounceMs > 0) {
        timer = setTimeout(() => {
          timer = undefined;
          void fetch();
        }, debounceMs);
        return;
      }
      return fetch();
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
