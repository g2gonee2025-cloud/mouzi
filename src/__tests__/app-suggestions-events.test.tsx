import { afterEach, expect, it, vi } from "vitest";
import type { EffectCallback } from "react";
import App from "../App";

const adapters = vi.hoisted(() => ({
  app: vi.fn(), dashboard: vi.fn(), cleanup: vi.fn(), suggestions: vi.fn(),
  currentWindow: vi.fn(), listen: vi.fn(), useEffect: vi.fn(),
}));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: adapters.useEffect,
  useState: (initial: unknown) => [typeof initial === "function" ? initial() : initial, vi.fn()],
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: adapters.listen }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: adapters.currentWindow }));
vi.mock("@tauri-apps/plugin-notification", () => ({ onAction: async () => ({ unregister: async () => {} }) }));
vi.mock("../store/useAppStore", () => ({ useAppStore: Object.assign(adapters.app, { getState: adapters.app }) }));
vi.mock("../store/useDashboardStore", () => ({ useDashboardStore: { getState: adapters.dashboard } }));
vi.mock("../store/useCleanupStore", () => ({ useCleanupStore: { getState: adapters.cleanup } }));
vi.mock("../store/useSuggestionsStore", () => ({ useSuggestionsStore: { getState: adapters.suggestions } }));
vi.mock("../components/Popup", () => ({ default: () => null }));
vi.mock("../components/Settings", () => ({ default: () => null }));
vi.mock("../pages/Dashboard", () => ({ default: () => null }));
vi.mock("../pages/Cleanup", () => ({ default: () => null }));
vi.mock("../pages/Suggestions", () => ({ default: () => null }));

const unmounts: (() => void)[] = [];
afterEach(() => {
  unmounts.splice(0).forEach((unmount) => unmount());
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function createBackend() {
  const pending = deferred<void>();
  let current = 0;
  return {
    finish: () => pending.resolve(),
    load: vi.fn(async (_renderer: string) => {
      const generation = ++current;
      await pending.promise;
      if (generation !== current) throw new Error("Suggestion request superseded");
      return ["fixture.pdf"];
    }),
  };
}

type InventoryEvent = "mutation-completed" | "folders-changed";

function mountRenderer(
  backend: ReturnType<typeof createBackend>,
  { label, hash, visible = true, busy = false }: { label: string; hash: string; visible?: boolean; busy?: boolean },
) {
  const window = { location: { hash }, addEventListener: vi.fn(), removeEventListener: vi.fn() };
  const nativeWindow = { label, isVisible: vi.fn(async () => visible) };
  const callbacks = new Map<string, () => void>();
  const app = {
    settings: null,
    loadSettings: vi.fn(async () => {}),
    loadLogs: vi.fn(async () => {}),
    loadStats: vi.fn(async () => {}),
    loadFolders: vi.fn(async () => {}),
    getPendingFiles: vi.fn(async () => {}),
  };
  const dashboard = { refresh: vi.fn(async () => {}), syncScanState: vi.fn(async () => {}) };
  const cleanup = { invalidateCandidates: vi.fn() };
  const result = { files: [] as string[], error: null as string | null };
  const suggestions = {
    busy,
    loadSuggestions: vi.fn(async () => {
      try {
        result.files = await backend.load(label);
      } catch (error) {
        result.error = String(error);
      }
    }),
  };

  const activate = () => {
    vi.stubGlobal("window", window);
    adapters.currentWindow.mockReturnValue(nativeWindow);
    adapters.app.mockReturnValue(app);
    adapters.dashboard.mockReturnValue(dashboard);
    adapters.cleanup.mockReturnValue(cleanup);
    adapters.suggestions.mockReturnValue(suggestions);
  };
  activate();
  adapters.listen.mockImplementation(async (event: string, listener: () => void) => {
    callbacks.set(event, listener);
    return () => { callbacks.delete(event); };
  });
  adapters.useEffect.mockImplementation((run: EffectCallback) => {
    const cleanupEffect = run();
    if (cleanupEffect) unmounts.push(cleanupEffect);
  });
  App();

  const deliver = async (event: InventoryEvent) => {
    activate();
    callbacks.get(event)!();
    // Resolve the renderer's visibility check before switching contexts, while
    // suggestion collection is still held by the shared backend fixture.
    await Promise.resolve();
  };
  return { deliver, result, suggestions, dashboard, cleanup, app, window, nativeWindow };
}

it.each(["mutation-completed", "folders-changed"] as const)(
  "a single %s event cannot let the popup supersede the workspace Suggestions request",
  async (event) => {
    const backend = createBackend();
    const workspace = mountRenderer(backend, { label: "app", hash: "#/suggestions" });
    const popup = mountRenderer(backend, { label: "popup", hash: "#/popup" });

    await workspace.deliver(event);
    await popup.deliver(event);
    backend.finish();
    await Promise.all([workspace, popup].flatMap(({ suggestions }) => suggestions.loadSuggestions.mock.results.map(({ value }) => value)));

    expect(workspace.result).toEqual({ files: ["fixture.pdf"], error: null });
    expect(backend.load.mock.calls).toEqual([["app"]]);
    expect(workspace.nativeWindow.isVisible).toHaveBeenCalledOnce();
    expect(popup.nativeWindow.isVisible).not.toHaveBeenCalled();
    for (const renderer of [workspace, popup]) {
      expect(renderer.dashboard.refresh).toHaveBeenCalledOnce();
      expect(renderer.dashboard.syncScanState).toHaveBeenCalledOnce();
      expect(renderer.cleanup.invalidateCandidates).toHaveBeenCalledOnce();
      expect(renderer.app.getPendingFiles).toHaveBeenCalledOnce();
    }
  },
);

it("does not start suggestions in a hidden workspace that was left on Suggestions", async () => {
  const backend = createBackend();
  const workspace = mountRenderer(backend, { label: "app", hash: "#/suggestions", visible: false });
  await workspace.deliver("mutation-completed");
  backend.finish();
  expect(backend.load).not.toHaveBeenCalled();
  expect(workspace.cleanup.invalidateCandidates).toHaveBeenCalledOnce();
});

it("drops the refresh if its native visibility check returns after navigation away", async () => {
  const backend = createBackend();
  const workspace = mountRenderer(backend, { label: "app", hash: "#/suggestions" });
  const visibility = deferred<boolean>();
  workspace.nativeWindow.isVisible.mockReturnValueOnce(visibility.promise);
  await workspace.deliver("mutation-completed");
  workspace.window.location.hash = "#/dashboard";
  visibility.resolve(true);
  await Promise.resolve();
  backend.finish();
  expect(backend.load).not.toHaveBeenCalled();
});

it("preserves the busy guard while refreshing the rest of the workspace", async () => {
  const backend = createBackend();
  const workspace = mountRenderer(backend, { label: "app", hash: "#/suggestions", busy: true });
  await workspace.deliver("folders-changed");
  backend.finish();
  expect(backend.load).not.toHaveBeenCalled();
  expect(workspace.app.loadFolders).toHaveBeenCalledOnce();
  expect(workspace.cleanup.invalidateCandidates).toHaveBeenCalledOnce();
});
