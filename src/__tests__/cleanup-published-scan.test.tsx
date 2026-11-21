import type { EffectCallback } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import App from "../App";
import FileListTab from "../components/cleanup/FileListTab";
import { useCleanupStore } from "../store/useCleanupStore";
import type { CleanupPlan } from "../utils/cleanup";
import type { ScanComplete } from "../store/useDashboardStore";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  effects: [] as EffectCallback[],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: ipc.listen }));
vi.mock("@tauri-apps/plugin-notification", () => ({ onAction: async () => ({ unregister: async () => {} }) }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: (effect: EffectCallback) => { ipc.effects.push(effect); },
}));
vi.mock("../store/useAppStore", () => ({ useAppStore: () => ({ settings: null, loadSettings: async () => {} }) }));
vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  const store = actual.useCleanupStore;
  return { ...actual, useCleanupStore: Object.assign(store.getState, store) };
});
vi.mock("../components/Popup", () => ({ default: () => null }));
vi.mock("../components/Settings", () => ({ default: () => null }));
vi.mock("../pages/Dashboard", () => ({ default: () => null }));
vi.mock("../pages/Cleanup", () => ({ default: () => null }));
vi.mock("../pages/Suggestions", () => ({ default: () => null }));

const listeners = new Map<string, (event: { payload: ScanComplete }) => void>();
const unmounts: Array<() => void> = [];
const minBytes = 10 * 1_048_576;
const file = (path: string) => ({ path, size: 20 * 1_048_576, mtime: 1 });
const oldFile = file("C:/Fixture/old-report.bin");
const newFile = file("C:/Fixture/renamed-report.bin");

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((finish) => { resolve = finish; });
  return { promise, resolve };
}

function publish(status: ScanComplete["status"]) {
  listeners.get("scan-complete")?.({ payload: {
    runId: "100", root: "C:/Fixture", files: 1, bytes: oldFile.size, status,
    error: status === "failed" ? "Fixture scan failed" : null,
  } });
}

function renderLarge() {
  window.location.hash = "#/cleanup?tab=large";
  return renderToStaticMarkup(<FileListTab
    kind="large" defaultThreshold={100} findLabel="Find Large Files" hint="Review large files"
    emptyKey="cleanup.noLargeFiles" operationKey="confirm.op.trashLarge"
  />);
}

beforeEach(() => {
  ipc.invoke.mockReset();
  ipc.listen.mockImplementation(async (name: string, handle: (event: { payload: ScanComplete }) => void) => {
    listeners.set(name, handle);
    return () => { listeners.delete(name); };
  });
  useCleanupStore.setState(useCleanupStore.getInitialState(), true);
  vi.stubGlobal("window", Object.assign(new EventTarget(), { location: { hash: "#/dashboard" } }));
  renderToStaticMarkup(<App />);
  for (const effect of ipc.effects.splice(0)) {
    const cleanup = effect();
    if (cleanup) unmounts.push(cleanup);
  }
});

afterEach(() => {
  unmounts.splice(0).forEach((cleanup) => cleanup());
  ipc.effects.length = 0;
  listeners.clear();
  vi.unstubAllGlobals();
});

it("drops a cached file list on a Dashboard publication and preserves its threshold until Find", async () => {
  ipc.invoke.mockResolvedValueOnce([oldFile]);
  await useCleanupStore.getState().findLargeFiles(minBytes);
  expect(renderLarge()).toContain("old-report.bin");
  expect(renderLarge()).toMatch(/<input[^>]*type="number"[^>]*value="10"/);

  window.location.hash = "#/dashboard";
  publish("complete");

  const stale = renderLarge();
  expect(stale).not.toContain("old-report.bin");
  expect(stale).toMatch(/<input[^>]*type="number"[^>]*value="10"/);
  expect(stale).toContain("cleanup.notSearched");
  expect(stale).not.toContain("cleanup.noLargeFiles");
  expect(ipc.invoke).toHaveBeenCalledExactlyOnceWith("find_large_files_cmd", { minBytes });

  ipc.invoke.mockResolvedValueOnce([newFile]);
  await useCleanupStore.getState().findLargeFiles(minBytes);
  expect(renderLarge()).toContain("renamed-report.bin");
  expect(renderLarge()).not.toContain("old-report.bin");
  expect(ipc.invoke.mock.calls).toEqual(Array.from({ length: 2 }, () => ["find_large_files_cmd", { minBytes }]));
});

it("retains the last published candidates during a failed scan", async () => {
  ipc.invoke.mockResolvedValueOnce([oldFile]);
  await useCleanupStore.getState().findLargeFiles(minBytes);

  publish("failed");

  const html = renderLarge();
  expect(html).toContain("old-report.bin");
  expect(html).toMatch(/<input[^>]*type="number"[^>]*value="10"/);
  expect(ipc.invoke).toHaveBeenCalledExactlyOnceWith("find_large_files_cmd", { minBytes });
});

it("releases a visible old-snapshot finder and rejects its late rows and omissions", async () => {
  const pending = deferred<{ candidates: ReturnType<typeof file>[]; unavailableRoots: string[] }>();
  ipc.invoke.mockReturnValueOnce(pending.promise);
  const search = useCleanupStore.getState().findLargeFiles(minBytes);
  expect(useCleanupStore.getState().loading).toBe(true);
  renderLarge();

  publish("complete");
  expect(useCleanupStore.getState().loading).toBe(false);
  pending.resolve({ candidates: [oldFile], unavailableRoots: ["D:/Old scope"] });
  await search;

  expect(useCleanupStore.getState()).toMatchObject({ loading: false, largeFiles: { minBytes, files: null }, unavailableRoots: {} });
  expect(renderLarge()).not.toContain("old-report.bin");
  expect(renderLarge()).toMatch(/<input[^>]*type="number"[^>]*value="10"/);
});

it("clears only inventory file rows and lets the Empty folders finder and History continue", async () => {
  const directory = deferred<{ candidates: string[]; unavailableRoots: string[] }>();
  const history = deferred<{ entries: []; nextCursor: null }>();
  const receipt = [{ path: "C:/Fixture/finished.bin", status: "ok" }] as const;
  const receipts = { trash_large: { planId: "cleanup:finished", outcomes: [...receipt], error: null } };
  useCleanupStore.setState({
    duplicates: [{ hash: "hash", files: [oldFile, newFile] }],
    staleFiles: { days: 28, files: [oldFile] },
    receipts,
    unavailableRoots: { duplicates: ["D:/Old duplicate scope"], staleFiles: ["D:/Old stale scope"] },
  });
  ipc.invoke.mockReturnValueOnce(directory.promise).mockReturnValueOnce(history.promise);
  const emptySearch = useCleanupStore.getState().findEmptyDirs();
  const historyLoad = useCleanupStore.getState().loadHistory();

  publish("complete");

  expect(useCleanupStore.getState()).toMatchObject({
    duplicates: null, staleFiles: { days: 28, files: null }, loading: true, historyLoading: true, receipts,
  });
  directory.resolve({ candidates: ["C:/Fixture/empty"], unavailableRoots: ["Z:/Empty scope"] });
  history.resolve({ entries: [], nextCursor: null });
  await Promise.all([emptySearch, historyLoad]);

  expect(useCleanupStore.getState()).toMatchObject({
    loading: false, historyLoading: false, history: [], emptyDirs: ["C:/Fixture/empty"], receipts,
  });
  expect(useCleanupStore.getState().unavailableRoots).toEqual({ emptyDirs: ["Z:/Empty scope"] });
});

it("preserves the exact requested plan and partial receipt across successful publications", async () => {
  const plan: CleanupPlan = {
    planId: "cleanup:reviewed", expiresAt: "2026-11-22T12:00:00Z",
    actions: [
      { kind: "trash_large", path: oldFile.path, minBytes },
      { kind: "trash_large", path: newFile.path, minBytes },
    ],
    totalBytes: oldFile.size + newFile.size,
  };
  const preparation = deferred<CleanupPlan>();
  const execution = deferred<Array<{ path: string; status: "ok" | "failed" }>>();
  const outcomes = [
    { path: oldFile.path, status: "ok" as const },
    { path: newFile.path, status: "failed" as const },
  ];
  useCleanupStore.setState({ largeFiles: { minBytes, files: [oldFile, newFile] } });
  ipc.invoke.mockReturnValueOnce(preparation.promise).mockReturnValueOnce(execution.promise).mockResolvedValueOnce([]);
  const preparing = useCleanupStore.getState().prepareCleanup(plan.actions);
  publish("complete");
  preparation.resolve(plan);
  await expect(preparing).resolves.toEqual(plan);

  publish("complete");
  expect(useCleanupStore.getState().preparedPlan).toEqual(plan);
  const executing = useCleanupStore.getState().executeCleanup(plan.planId);
  publish("complete");
  execution.resolve(outcomes);
  await executing;

  expect(ipc.invoke).toHaveBeenCalledWith("execute_cleanup_cmd", { planId: plan.planId });
  expect(useCleanupStore.getState()).toMatchObject({
    busy: false,
    receipts: { trash_large: { planId: plan.planId, outcomes, error: null } },
    largeFiles: { minBytes, files: null },
  });
});
