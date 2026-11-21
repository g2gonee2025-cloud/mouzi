/** Tailwind utilities backed by the `--color-cat-*` tokens in src/index.css,
 *  so the palette follows the light/dark scheme instead of being baked in
 *  here as hex literals. The seven keys must stay aligned with the
 *  `categorize()` arms in src-tauri/src/scan.rs. */
import type { InventoryFile } from "../store/useDashboardStore";
import { fileName } from "./paths";

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

/** Find paths whose short folder names need context to tell them apart. */
export function rootsWithSharedNames(paths: readonly string[]): Set<string> {
  const firstByName = new Map<string, string>();
  const shared = new Set<string>();
  for (const path of paths) {
    const name = (fileName(path) || path).toLowerCase();
    const first = firstByName.get(name);
    if (first === undefined) firstByName.set(name, path);
    else if (first !== path) {
      shared.add(first);
      shared.add(path);
    }
  }
  return shared;
}

interface InventoryQuery {
  sort: DashboardViewOptions["sort"];
  category: string;
  root: string;
  query: string;
  limit: number;
  revision: number;
  after?: string;
}

interface BrowserScope extends DashboardViewOptions {
  rootScope: string;
  revision: number;
}

interface BrowserRequest extends BrowserScope {
  initialLargest: InventoryFile[];
  initialRecent: InventoryFile[];
}

export interface InventoryBrowserView {
  scope: string;
  rows: InventoryFile[];
  page: number;
  limited: boolean;
  loading: boolean;
  error: string | null;
}

export const INVENTORY_MATCH_LIMIT = 80;

export function inventoryBrowserScope({ sort, query, category, root, rootScope, revision }: BrowserScope): string {
  return JSON.stringify([sort, query.trim(), category, root, rootScope, revision]);
}

export const INVENTORY_CHANGED = "inventory.changed";

export function createInventoryBrowserLoader(
  fetchRows: (query: InventoryQuery) => Promise<InventoryFile[]>,
  update: (view: InventoryBrowserView) => void,
) {
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let view: InventoryBrowserView | undefined;
  let activeRequest: BrowserRequest | undefined;
  let pageStarts: (string | undefined)[] = [undefined];

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

  const loadPage = (request: BrowserRequest, page: number, after: string | undefined, debounceMs = 0) => {
    cancel();
    const current = generation;
    const scope = inventoryBrowserScope(request);
    const previous = view?.scope === scope ? view : undefined;
    const pending = {
      scope, page: previous?.page ?? 0, rows: previous?.rows ?? [],
      limited: previous?.limited ?? false, loading: true, error: null,
    };
    publish(pending);

    const fetch = async () => {
      try {
        const rows = await fetchRows({
          sort: request.sort,
          query: request.query.trim(),
          category: request.category ?? "",
          root: request.root ?? "",
          limit: INVENTORY_MATCH_LIMIT + 1,
          revision: request.revision,
          ...(after === undefined ? {} : { after }),
        });
        if (current !== generation) return;
        pageStarts[page] = after;
        pageStarts.length = page + 1;
        publish({
          scope, page,
          rows: rows.slice(0, INVENTORY_MATCH_LIMIT),
          limited: rows.length > INVENTORY_MATCH_LIMIT,
          loading: false,
          error: null,
        });
      } catch (error) {
        if (current !== generation) return;
        if ((error instanceof Error ? error.message : error) === INVENTORY_CHANGED) {
          pageStarts = [undefined];
          publish({ scope, page: 0, rows: [], limited: false, loading: false, error: INVENTORY_CHANGED });
        } else {
          publish({ ...pending, loading: false, error: String(error) });
        }
      }
    };

    if (request.query.trim() && !previous && debounceMs > 0) {
      timer = setTimeout(() => {
        timer = undefined;
        void fetch();
      }, debounceMs);
      return;
    }
    return fetch();
  };

  return {
    cancel,
    load: (request: BrowserRequest, debounceMs = 0) => {
      activeRequest = request;
      const scope = inventoryBrowserScope(request);
      const sameScope = view?.scope === scope;
      if (!sameScope) pageStarts = [undefined];
      if (!request.query.trim() && !request.category && !request.root) {
        cancel();
        publish({
          scope, page: 0,
          rows: request.sort === "size" ? request.initialLargest : request.initialRecent,
          limited: false, loading: false, error: null,
        });
        return;
      }
      const page = sameScope ? view?.page ?? 0 : 0;
      return loadPage(request, page, pageStarts[page], debounceMs);
    },
    navigate: (direction: -1 | 1) => {
      if (!view || !activeRequest || view.loading || view.error === INVENTORY_CHANGED) return;
      const page = view.page + direction;
      if (page < 0 || (direction === 1 && !view.limited)) return;
      const after = direction === 1 ? view.rows[view.rows.length - 1]?.path : pageStarts[page];
      return loadPage(activeRequest, page, after);
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
