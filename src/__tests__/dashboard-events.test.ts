import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DashboardStats } from "../store/useDashboardStore";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  handlers: new Map<string, Set<(event: { payload: unknown }) => void>>(),
  unlisteners: [] as ReturnType<typeof vi.fn>[],
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

function emit(name: string, payload: unknown) {
  for (const handler of mocks.handlers.get(name) ?? []) handler({ payload });
}

let dashboard: typeof import("../store/useDashboardStore");

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  mocks.handlers.clear();
  mocks.unlisteners.length = 0;
  mocks.invoke.mockReset().mockResolvedValue({ watchedRoots: [] });
  mocks.listen.mockReset().mockImplementation(async (name, handler) => {
    const handlers = mocks.handlers.get(name) ?? new Set();
    handlers.add(handler);
    mocks.handlers.set(name, handlers);
    const unlisten = vi.fn(() => handlers.delete(handler));
    mocks.unlisteners.push(unlisten);
    return unlisten;
  });
  dashboard = await import("../store/useDashboardStore");
});

afterEach(() => vi.useRealTimers());

describe("dashboard scan lifecycle", () => {
  it("finishes from the terminal event and retains scan errors while refreshing", async () => {
    const dispose = await dashboard.subscribeDashboardEvents();
    const runId = "19000000000000000001";
    emit("scan-progress", { runId, root: "C:/work", processed: 0 });
    emit("scan-finished", { runId, status: "failed", errors: ["Access denied"] });
    expect(dashboard.useDashboardStore.getState().isScanning).toBe(false);
    expect(dashboard.useDashboardStore.getState().scanErrors).toEqual(["Access denied"]);
    await vi.advanceTimersByTimeAsync(200);
    expect(dashboard.useDashboardStore.getState().scanErrors).toEqual(["Access denied"]);
    dispose();
  });

  it("ignores old runs and late progress after terminal completion", async () => {
    const dispose = await dashboard.subscribeDashboardEvents();
    const runId = "19000000000000000002";
    emit("scan-progress", { runId, root: "new", processed: 1 });
    emit("scan-finished", { runId: "19000000000000000001", status: "finished", errors: [] });
    expect(dashboard.useDashboardStore.getState().isScanning).toBe(true);
    emit("scan-finished", { runId, status: "finished", errors: [] });
    emit("scan-progress", { runId, root: "late", processed: 2 });
    expect(dashboard.useDashboardStore.getState().isScanning).toBe(false);
    expect(dashboard.useDashboardStore.getState().scanProgress).toBeNull();
    dispose();
  });

  it("does not restart the spinner when terminal arrives before the start response", async () => {
    const dispose = await dashboard.subscribeDashboardEvents();
    mocks.invoke.mockImplementation(async (name) => {
      if (name === "start_scan_cmd") {
        emit("scan-finished", { runId: "19000000000000000003", status: "finished", errors: [] });
        return { started: true, runId: "19000000000000000003", rootCount: 1 };
      }
      return [];
    });
    await dashboard.useDashboardStore.getState().startScan();
    expect(dashboard.useDashboardStore.getState().isScanning).toBe(false);
    dispose();
  });

  it("uses the actual run root count without pre-reading folder configuration", async () => {
    mocks.invoke.mockResolvedValue({ started: true, runId: "19000000000000000006", rootCount: 1 });
    await dashboard.useDashboardStore.getState().startScan();
    expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith("start_scan_cmd");
    expect(dashboard.useDashboardStore.getState().rootCount).toBe(1);
  });

  it("refreshes eligibility when pausing the last root races a scan start", async () => {
    const retained: DashboardStats = {
      watchedRoots: [{ path: "C:/work", files: 2, bytes: 100, status: "complete", stale: false, lastScanAt: 1, error: null }],
      revision: 1, totalFiles: 2, totalBytes: 100, scanEligibleRoots: 1, lastScanAt: 1,
      categoryBreakdown: [], largestFiles: [], recentFiles: [], insights: dashboard.EMPTY_INSIGHTS, ageBuckets: [],
    };
    dashboard.useDashboardStore.setState({ stats: retained });
    let finishRefresh!: (stats: typeof retained) => void;
    const refresh = new Promise<typeof retained>((resolve) => { finishRefresh = resolve; });
    let startRefresh!: () => void;
    const refreshStarted = new Promise<void>((resolve) => { startRefresh = resolve; });
    mocks.invoke.mockImplementation(async (command) => {
      if (command === "start_scan_cmd") return { started: false, reason: "no_roots", rootCount: 0 };
      if (command === "is_scanning_cmd") return false;
      if (command === "get_dashboard_stats_cmd") {
        startRefresh();
        return refresh;
      }
      throw new Error(`Unexpected command: ${command}`);
    });

    const start = dashboard.useDashboardStore.getState().startScan();
    await refreshStarted;
    expect(dashboard.useDashboardStore.getState().scanReason).toBe("no_roots");
    expect(dashboard.useDashboardStore.getState().stats?.totalFiles).toBe(2);
    finishRefresh({ ...retained, scanEligibleRoots: 0 });
    await start;
    expect(dashboard.useDashboardStore.getState()).toMatchObject({
      isScanning: false, scanReason: null, stats: { totalFiles: 2, scanEligibleRoots: 0 },
    });

    mocks.invoke.mockResolvedValue(retained);
    await dashboard.useDashboardStore.getState().loadStats();
    expect(dashboard.useDashboardStore.getState().stats?.scanEligibleRoots).toBe(1);
  });

  it("refreshes root health before the whole run ends when a failed root recovers", async () => {
    const dispose = await dashboard.subscribeDashboardEvents();
    const path = "C:/first";
    const failed = { path, files: 0, bytes: 0, status: "failed", stale: true, lastScanAt: 1, error: "Unavailable" };
    mocks.invoke.mockImplementation(async (name) => name === "get_stats_cmd" ? [] : {
      watchedRoots: [{ ...failed, status: "complete", stale: false, error: null }],
    });
    dashboard.useDashboardStore.setState({
      stats: { watchedRoots: [failed] } as DashboardStats,
    });
    const runId = "19000000000000000007";
    emit("scan-progress", { runId, root: path, processed: 0, rootCount: 2 });
    emit("scan-complete", { runId, root: path, status: "complete", error: null });
    emit("scan-progress", { runId, root: "C:/second", processed: 0, rootCount: 2 });
    await vi.advanceTimersByTimeAsync(200);
    expect(mocks.invoke.mock.calls.filter(([name]) => name === "get_dashboard_stats_cmd")).toHaveLength(1);
    expect(dashboard.useDashboardStore.getState()).toMatchObject({
      isScanning: true,
      stats: { watchedRoots: [{ path, status: "complete", stale: false, error: null }] },
    });
    dispose();
  });

  it("does not settle a newer dashboard read with an older failed-root snapshot", async () => {
    const store = dashboard.useDashboardStore;
    let finishOlder!: (stats: { watchedRoots: { status: string }[] }) => void;
    mocks.invoke.mockImplementationOnce(() => new Promise((resolve) => { finishOlder = resolve; }))
      .mockResolvedValueOnce({ watchedRoots: [{ status: "complete" }] });
    const older = store.getState().loadStats();
    await store.getState().loadStats();
    const revision = store.getState().statsReadRevision;
    finishOlder({ watchedRoots: [{ status: "failed" }] });
    await older;
    expect(revision).toBeGreaterThan(0);
    expect(store.getState()).toMatchObject({
      statsReadRevision: revision,
      isLoading: false,
      stats: { watchedRoots: [{ status: "complete" }] },
    });
  });
});

describe("dashboard subscription ownership", () => {
  it("does not report readiness before the terminal listener is registered", async () => {
    const normal = mocks.listen.getMockImplementation()!;
    let reachTerminal!: () => void;
    let finishRegistration!: () => void;
    const reached = new Promise<void>((resolve) => { reachTerminal = resolve; });
    const release = new Promise<void>((resolve) => { finishRegistration = resolve; });
    mocks.listen.mockImplementation(async (name, handler) => {
      if (name === "scan-finished") {
        reachTerminal();
        await release;
      }
      return normal(name, handler);
    });
    const ready = vi.fn();
    const pending = dashboard.subscribeDashboardEvents().then((dispose) => {
      ready();
      return dispose;
    });
    await reached;
    expect(ready).not.toHaveBeenCalled();
    expect(mocks.handlers.get("scan-finished")).toBeUndefined();
    finishRegistration();
    const dispose = await pending;
    expect(ready).toHaveBeenCalledOnce();
    expect(mocks.handlers.get("scan-finished")?.size).toBe(1);
    dispose();
  });

  it("cleans successful registrations when a later listener fails", async () => {
    const normal = mocks.listen.getMockImplementation()!;
    mocks.listen.mockReset()
      .mockImplementationOnce(normal)
      .mockRejectedValueOnce(new Error("registration failed"));
    await expect(dashboard.subscribeDashboardEvents()).rejects.toThrow("registration failed");
    expect(mocks.unlisteners).toHaveLength(1);
    expect(mocks.unlisteners[0]).toHaveBeenCalledOnce();
    mocks.listen.mockImplementation(normal);
    const dispose = await dashboard.subscribeDashboardEvents();
    expect(mocks.handlers.get("scan-finished")?.size).toBe(1);
    dispose();
  });

  it("disposes each mount's listeners and timer independently", async () => {
    const first = await dashboard.subscribeDashboardEvents();
    const second = await dashboard.subscribeDashboardEvents();
    first();
    expect(mocks.handlers.get("scan-progress")?.size).toBe(1);
    emit("scan-finished", { runId: "19000000000000000004", status: "finished", errors: [] });
    second();
    await vi.advanceTimersByTimeAsync(200);
    expect(mocks.handlers.get("scan-progress")?.size).toBe(0);
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.unlisteners.every((unlisten) => unlisten.mock.calls.length === 1)).toBe(true);
  });

  it("unregisters a listener that resolves after its mount was aborted", async () => {
    let resolve!: (unlisten: () => void) => void;
    const late = vi.fn();
    mocks.listen.mockReset().mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const controller = new AbortController();
    const pending = dashboard.subscribeDashboardEvents(controller.signal);
    controller.abort();
    resolve(late);
    const dispose = await pending;
    dispose();
    expect(late).toHaveBeenCalledOnce();
    expect(mocks.listen).toHaveBeenCalledOnce();
  });

  it("does not cancel another mounted dashboard's terminal refresh", async () => {
    mocks.invoke.mockImplementation(async (name) => name === "get_stats_cmd" ? [] : { watchedRoots: [] });
    const first = await dashboard.subscribeDashboardEvents();
    const second = await dashboard.subscribeDashboardEvents();
    emit("scan-finished", { runId: "19000000000000000005", status: "finished", errors: [] });
    first();
    await vi.advanceTimersByTimeAsync(200);
    expect(mocks.invoke).toHaveBeenCalledWith("get_dashboard_stats_cmd");
    second();
  });
});
