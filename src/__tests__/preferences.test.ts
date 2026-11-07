import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);

import { useAppStore, type AppSettings } from "../store/useAppStore";

function settings(patch: Partial<AppSettings> = {}): AppSettings {
  return {
    id: 1, revision: 1, language: "en", theme: "system", telemetry_enabled: false,
    first_run: false, autostart: false, grace_period_seconds: 300,
    lock_check_enabled: true, recursive_watch: false, local_ai_enabled: false,
    local_ai_model: "llama3.2", schedule_enabled: false, schedule_times_per_day: 1,
    schedule_time_1: "08:00", schedule_time_2: null, schedule_time_3: null,
    schedule_time_4: null, ...patch,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  ipc.invoke.mockReset();
  useAppStore.setState({ settings: null, schedule: null, folders: [], logs: [],
    historyCounts: null, stats: [], pendingFiles: [], isLoading: false,
    actionRefreshErrors: { history: null, historyCounts: null, stats: null } });
});

describe("committed preference and schedule settings", () => {
  it("sends only the changed preference and preserves the committed schedule", async () => {
    const committedSchedule = settings({ revision: 2, schedule_enabled: true, schedule_time_1: "21:30" });
    ipc.invoke.mockResolvedValueOnce(committedSchedule);
    await useAppStore.getState().updateSchedule({
      schedule_enabled: true, schedule_times_per_day: 1, schedule_time_1: "21:30",
      schedule_time_2: null, schedule_time_3: null, schedule_time_4: null,
    });
    ipc.invoke.mockResolvedValueOnce({ ...committedSchedule, revision: 3, theme: "dark" });
    await useAppStore.getState().saveSettings({ theme: "dark" });
    expect(ipc.invoke).toHaveBeenLastCalledWith("update_settings_cmd", { settings: { theme: "dark" } });
    expect(useAppStore.getState().settings).toMatchObject({ theme: "dark", schedule_enabled: true, schedule_time_1: "21:30" });
    expect(useAppStore.getState().schedule).toMatchObject({ schedule_enabled: true, schedule_time_1: "21:30" });
  });

  it("ignores a preference response older than another committed write", async () => {
    useAppStore.getState().applySettings(settings());
    const older = deferred<AppSettings>();
    const newer = deferred<AppSettings>();
    ipc.invoke.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const first = useAppStore.getState().saveSettings({ theme: "dark" });
    const second = useAppStore.getState().saveSettings({ language: "fr" });
    newer.resolve(settings({ revision: 3, theme: "dark", language: "fr" }));
    await second;
    older.resolve(settings({ revision: 2, theme: "dark" }));
    await first;
    expect(useAppStore.getState().settings).toMatchObject({ revision: 3, theme: "dark", language: "fr" });
  });

  it("does not put unchanged fields or hidden schedule fields on the wire", async () => {
    useAppStore.getState().applySettings(settings());
    ipc.invoke.mockResolvedValueOnce(settings({ revision: 2, theme: "dark" }));
    await useAppStore.getState().saveSettings(settings({ theme: "dark", schedule_enabled: true, recursive_watch: true }));
    expect(ipc.invoke).toHaveBeenCalledWith("update_settings_cmd", { settings: { theme: "dark" } });
  });

  it("does not let an old read replace a newer settings event", async () => {
    const pendingRead = deferred<AppSettings>();
    ipc.invoke.mockReturnValueOnce(pendingRead.promise);
    const load = useAppStore.getState().loadSettings();
    useAppStore.getState().applySettings(settings({ revision: 4, local_ai_enabled: true }));
    pendingRead.resolve(settings({ revision: 3 }));
    await load;
    expect(useAppStore.getState().settings).toMatchObject({ revision: 4, local_ai_enabled: true });
  });

  it("keeps settings unchanged and rejects when a preference write fails", async () => {
    const current = settings();
    useAppStore.getState().applySettings(current);
    ipc.invoke.mockRejectedValueOnce(new Error("model not installed"));
    await expect(useAppStore.getState().saveSettings({ local_ai_model: "missing" })).rejects.toThrow("model not installed");
    expect(useAppStore.getState().settings).toEqual(current);
  });

  it("patches autostart without serializing scheduler values", async () => {
    useAppStore.getState().applySettings(settings());
    ipc.invoke.mockResolvedValueOnce(undefined).mockResolvedValueOnce(settings({ revision: 2, autostart: true }));
    await useAppStore.getState().setAutostart(true);
    expect(ipc.invoke.mock.calls).toEqual([
      ["enable_autostart_cmd"],
      ["update_settings_cmd", { settings: { autostart: true } }],
    ]);
  });
});

describe("authoritative clean scope and history", () => {
  const restored = { status: "ok", message: null, restoredTo: "original.txt" };

  it.each([
    {
      command: "clean_now_cmd",
      response: [["original.txt", "rule", "sorted.txt"]],
      outcome: [{ file: "original.txt", rule: "rule", destination: "sorted.txt" }],
      run: () => useAppStore.getState().cleanNow(),
    },
    { command: "undo_action_cmd", response: restored, outcome: restored, run: () => useAppStore.getState().undoAction(1) },
    {
      command: "undo_all_cmd",
      response: { count: 1, results: [restored] },
      outcome: { count: 1, results: [restored] },
      run: () => useAppStore.getState().undoAll(),
    },
  ])("keeps the $command outcome when the history refresh fails", async ({ command, response, outcome, run }) => {
    const history = deferred<never>();
    ipc.invoke.mockImplementation(async (name: string) => {
      if (name === command) return response;
      if (name === "get_logs_cmd") return history.promise;
      if (name === "get_stats_cmd") return [["txt", 1]];
      throw new Error(`Unexpected IPC command ${name}`);
    });

    const action = run();
    await vi.waitFor(() => expect(useAppStore.getState().stats).toEqual([{ file_type: "txt", count: 1 }]));
    history.reject(new Error("History unavailable"));
    await expect(action).resolves.toEqual(outcome);
    expect(useAppStore.getState().actionRefreshErrors).toEqual({ history: "Error: History unavailable", historyCounts: null, stats: null });
  });

  it("clears each action warning after a successful ordinary read of that view", async () => {
    const failures = new Map([
      ["get_logs_cmd", "Logs unavailable"],
      ["get_stats_cmd", "Stats unavailable"],
    ]);
    ipc.invoke.mockImplementation(async (command: string) => {
      const failure = failures.get(command);
      if (failure) throw new Error(failure);
      if (command === "undo_action_cmd") return restored;
      if (command === "get_history_counts_cmd") return { total: 1, undoable: 0 };
      return [];
    });

    const store = useAppStore.getState();
    await expect(store.undoAction(1)).resolves.toEqual(restored);
    await store.loadHistoryCounts();
    expect(useAppStore.getState().actionRefreshErrors).toEqual({
      history: "Error: Logs unavailable", historyCounts: null, stats: "Error: Stats unavailable",
    });

    failures.delete("get_logs_cmd");
    await store.loadLogs();
    expect(useAppStore.getState().actionRefreshErrors).toEqual({
      history: null, historyCounts: null, stats: "Error: Stats unavailable",
    });

    failures.set("get_history_counts_cmd", "Counts unavailable");
    await expect(store.undoAction(1)).resolves.toEqual(restored);
    failures.delete("get_stats_cmd");
    await store.loadStats();
    expect(useAppStore.getState().actionRefreshErrors).toEqual({
      history: null, historyCounts: "Error: Counts unavailable", stats: null,
    });

    failures.delete("get_history_counts_cmd");
    await store.loadHistoryCounts();
    expect(useAppStore.getState().actionRefreshErrors).toEqual({ history: null, historyCounts: null, stats: null });
  });

  it("ignores a late action refresh failure after a newer ordinary read succeeds", async () => {
    const oldHistory = deferred<never>();
    const started = deferred<void>();
    let historyCalls = 0;
    ipc.invoke.mockImplementation(async (command: string) => {
      if (command === "undo_action_cmd") return restored;
      if (command === "get_history_counts_cmd") return { total: 1, undoable: 0 };
      if (command === "get_logs_cmd" && ++historyCalls === 1) {
        started.resolve();
        return oldHistory.promise;
      }
      return [];
    });

    const action = useAppStore.getState().undoAction(1);
    await started.promise;
    await useAppStore.getState().loadLogs();
    oldHistory.reject(new Error("Stale logs failure"));
    await expect(action).resolves.toEqual(restored);
    expect(useAppStore.getState().actionRefreshErrors).toEqual({ history: null, historyCounts: null, stats: null });
  });

  it("cleans through the current backend scope with no path or Downloads fallback", async () => {
    useAppStore.setState({ folders: [{ id: 1, path: "C:/removed", enabled: true, mode: "manual" }] });
    ipc.invoke.mockImplementation(async (command: string) => {
      if (command === "get_history_counts_cmd") return { total: 0, undoable: 0 };
      return [];
    });
    expect(await useAppStore.getState().cleanNow()).toEqual([]);
    expect(ipc.invoke).toHaveBeenCalledWith("clean_now_cmd");
    expect(ipc.invoke.mock.calls.some(([command]) => command === "scan_folder_cmd" || command === "get_downloads_folder")).toBe(false);
    expect(useAppStore.getState().isLoading).toBe(false);
  });

  it("keeps full history counts independent of the newest displayed rows", async () => {
    ipc.invoke.mockImplementation(async (command: string) => command === "get_logs_cmd" ? [] : { total: 120, undoable: 70 });
    await useAppStore.getState().loadLogs();
    expect(useAppStore.getState().logs).toEqual([]);
    expect(useAppStore.getState().historyCounts).toEqual({ total: 120, undoable: 70 });
    expect(ipc.invoke).toHaveBeenCalledWith("get_logs_cmd", { limit: 50 });
  });

  it("discards an old folder read that would restore removed roots", async () => {
    const old = deferred<unknown>();
    const latest = deferred<unknown>();
    ipc.invoke.mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise);
    const first = useAppStore.getState().loadFolders();
    const second = useAppStore.getState().loadFolders();
    latest.resolve([]);
    await second;
    old.resolve([{ id: 1, path: "C:/removed", enabled: true, mode: "manual" }]);
    await first;
    expect(useAppStore.getState().folders).toEqual([]);
  });
});
