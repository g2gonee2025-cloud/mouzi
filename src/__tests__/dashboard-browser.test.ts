import { describe, expect, it, vi } from "vitest";
import { clearMissingDashboardRoot, createInventoryBrowserLoader, INVENTORY_MATCH_LIMIT } from "../utils/dashboard";
import type { InventoryFile } from "../store/useDashboardStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const file = (path: string): InventoryFile => ({ path, size: 1, mtime: 1, category: "Other", rootPath: "C:/work" });
const request = {
  sort: "size" as const, category: null, root: null, rootScope: "C:/work",
  query: "old", initialLargest: [file("initial")], initialRecent: [],
};

describe("inventory browser response ordering", () => {
  it("ignores older filter results and their loading completion", async () => {
    const old = deferred<InventoryFile[]>();
    const latest = deferred<InventoryFile[]>();
    const update = vi.fn();
    const fetchRows = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
    const loader = createInventoryBrowserLoader(fetchRows, update);
    const first = loader.load(request);
    const second = loader.load({ ...request, query: "new" });
    old.resolve([file("old")]);
    await first;
    expect(update).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [], loading: true }));
    latest.resolve([file("new")]);
    await second;
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [file("new")], loading: false }));
  });

  it("restores initial data immediately when filters clear and ignores the old request", async () => {
    const old = deferred<InventoryFile[]>();
    const update = vi.fn();
    const loader = createInventoryBrowserLoader(() => old.promise, update);
    const first = loader.load(request);
    await loader.load({ ...request, query: "" });
    old.resolve([file("old")]);
    await first;
    expect(update).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: request.initialLargest, loading: false }));
  });

  it("ignores delayed errors after filter change or unmount", async () => {
    const old = deferred<InventoryFile[]>();
    const update = vi.fn();
    const loader = createInventoryBrowserLoader(() => old.promise, update);
    const first = loader.load(request);
    update.mockClear();
    loader.cancel();
    old.reject(new Error("old failure"));
    await first;
    expect(update).not.toHaveBeenCalled();
  });

  it("reports the current request failure rather than presenting it as an empty success", async () => {
    const update = vi.fn();
    const loader = createInventoryBrowserLoader(async () => { throw new Error("database unavailable"); }, update);
    await loader.load(request);
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({
      rows: [], error: "Error: database unavailable", loading: false,
    }));
  });

  it("keeps rows during a same-scope refresh and discards them when watched roots change", async () => {
    const refresh = deferred<InventoryFile[]>();
    const newScope = deferred<InventoryFile[]>();
    const update = vi.fn();
    const fetchRows = vi.fn()
      .mockResolvedValueOnce([file("retained")])
      .mockReturnValueOnce(refresh.promise)
      .mockReturnValueOnce(newScope.promise);
    const loader = createInventoryBrowserLoader(fetchRows, update);
    await loader.load(request);
    const refreshing = loader.load(request);
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [file("retained")], loading: true }));
    const changed = loader.load({ ...request, rootScope: "D:/work" });
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [], loading: true }));
    refresh.resolve([file("obsolete")]);
    await refreshing;
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [], loading: true }));
    newScope.resolve([file("new")]);
    await changed;
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [file("new")], loading: false }));
  });

  it("starts a fresh request when the browser is remounted with the same view options", async () => {
    const old = deferred<InventoryFile[]>();
    const oldUpdate = vi.fn();
    const firstLoader = createInventoryBrowserLoader(() => old.promise, oldUpdate);
    const activeRequest = { ...request, sort: "mtime" as const, category: "Documents", root: "C:/work" };
    const pending = firstLoader.load(activeRequest);
    firstLoader.cancel();
    oldUpdate.mockClear();

    const newUpdate = vi.fn();
    const fetchRows = vi.fn().mockResolvedValue([file("current")]);
    const newLoader = createInventoryBrowserLoader(fetchRows, newUpdate);
    await newLoader.load(activeRequest);
    old.resolve([file("obsolete")]);
    await pending;

    expect(oldUpdate).not.toHaveBeenCalled();
    expect(fetchRows).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      sort: "mtime", query: "old", category: "Documents", root: "C:/work",
    }));
    expect(newUpdate).toHaveBeenLastCalledWith(expect.objectContaining({ rows: [file("current")], loading: false }));
  });

  it("cancels a pending debounce and fetches only the final query", async () => {
    vi.useFakeTimers();
    try {
      const fetchRows = vi.fn().mockResolvedValue([]);
      const loader = createInventoryBrowserLoader(fetchRows, vi.fn());
      loader.load(request, 200);
      loader.load({ ...request, query: "new" }, 200);
      await vi.advanceTimersByTimeAsync(199);
      expect(fetchRows).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(fetchRows).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ query: "new" }));
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([INVENTORY_MATCH_LIMIT, INVENTORY_MATCH_LIMIT + 1])("shows at most 80 out of %i matches", async (count) => {
    const rows = Array.from({ length: count }, (_, i) => file(String(i)));
    const update = vi.fn();
    const fetchRows = vi.fn().mockResolvedValue(rows);
    const loader = createInventoryBrowserLoader(fetchRows, update);
    await loader.load(request);
    expect(fetchRows).toHaveBeenCalledWith(expect.objectContaining({ limit: INVENTORY_MATCH_LIMIT + 1 }));
    expect(update).toHaveBeenLastCalledWith(expect.objectContaining({
      rows: rows.slice(0, INVENTORY_MATCH_LIMIT), limited: count > INVENTORY_MATCH_LIMIT, loading: false,
    }));
  });
});

describe("saved Dashboard root selection", () => {
  it("clears the missing root while keeping the other view options", () => {
    const view = { sort: "mtime" as const, query: "notes", category: "Documents", root: "C:/old" };
    expect(clearMissingDashboardRoot(view, "C:/old")).toEqual({ ...view, root: null });
  });

  it("keeps a newer root chosen while reconciliation is queued", () => {
    const view = { sort: "size" as const, query: "notes", category: "Documents", root: "D:/new" };
    expect(clearMissingDashboardRoot(view, "C:/old")).toBe(view);
  });
});
