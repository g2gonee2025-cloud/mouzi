import { describe, expect, it, vi } from "vitest";
import { createInventoryBrowserLoader } from "../utils/dashboard";
import type { InventoryFile } from "../store/useDashboardStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const file = (path: string): InventoryFile => ({ path, size: 1, mtime: 1, category: "Other", rootPath: "C:/work" });
const request = { sort: "size" as const, category: null, root: null, query: "old", initialLargest: [file("initial")], initialRecent: [] };

describe("inventory browser response ordering", () => {
  it("ignores older filter results and their loading completion", async () => {
    const old = deferred<InventoryFile[]>();
    const latest = deferred<InventoryFile[]>();
    const updates = { rows: vi.fn(), loading: vi.fn(), error: vi.fn() };
    const fetchRows = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
    const loader = createInventoryBrowserLoader(fetchRows, updates);
    const first = loader.load(request);
    const second = loader.load({ ...request, query: "new" });
    old.resolve([file("old")]);
    await first;
    expect(updates.rows).not.toHaveBeenCalled();
    expect(updates.loading).not.toHaveBeenCalledWith(false);
    latest.resolve([file("new")]);
    await second;
    expect(updates.rows).toHaveBeenCalledExactlyOnceWith([file("new")]);
    expect(updates.loading).toHaveBeenLastCalledWith(false);
  });

  it("restores initial data immediately when filters clear and ignores the old request", async () => {
    const old = deferred<InventoryFile[]>();
    const updates = { rows: vi.fn(), loading: vi.fn(), error: vi.fn() };
    const loader = createInventoryBrowserLoader(() => old.promise, updates);
    const first = loader.load(request);
    await loader.load({ ...request, query: "" });
    old.resolve([file("old")]);
    await first;
    expect(updates.rows).toHaveBeenCalledExactlyOnceWith(request.initialLargest);
  });

  it("ignores delayed errors after filter change or unmount", async () => {
    const old = deferred<InventoryFile[]>();
    const updates = { rows: vi.fn(), loading: vi.fn(), error: vi.fn() };
    const loader = createInventoryBrowserLoader(() => old.promise, updates);
    const first = loader.load(request);
    updates.error.mockClear();
    loader.cancel();
    old.reject(new Error("old failure"));
    await first;
    expect(updates.error).not.toHaveBeenCalled();
    expect(updates.loading).not.toHaveBeenCalledWith(false);
  });

  it("reports the current request failure rather than presenting it as an empty success", async () => {
    const updates = { rows: vi.fn(), loading: vi.fn(), error: vi.fn() };
    const loader = createInventoryBrowserLoader(async () => { throw new Error("database unavailable"); }, updates);
    await loader.load(request);
    expect(updates.error).toHaveBeenLastCalledWith("Error: database unavailable");
    expect(updates.loading).toHaveBeenLastCalledWith(false);
  });
});
