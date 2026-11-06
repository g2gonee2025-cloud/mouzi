import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
      if (name === "get_folders_cmd") return [
        { path: "on", enabled: true, mode: "silent" },
        { path: "off", enabled: false, mode: "silent" },
        { path: "paused", enabled: true, mode: "paused" },
      ];
      if (name === "start_scan_cmd") {
        emit("scan-finished", { runId: "19000000000000000003", status: "finished", errors: [] });
        return { started: true, runId: "19000000000000000003" };
      }
      return [];
    });
    await dashboard.useDashboardStore.getState().startScan();
    expect(dashboard.useDashboardStore.getState().isScanning).toBe(false);
    expect(dashboard.useDashboardStore.getState().rootCount).toBe(1);
    dispose();
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
