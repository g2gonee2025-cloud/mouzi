import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("@tauri-apps/api/event", () => ({ listen: ipc.listen }));

import { observeArchiveImportState, useAppStore, type AppSettings, type ArchiveImportState } from "../store/useAppStore";

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
  ipc.listen.mockReset();
  useAppStore.setState(useAppStore.getInitialState(), true);
});

describe("committed preference and schedule settings", () => {
  it("sets Only new as part of the folder registration", async () => {
    const row = { id: 42, path: "C:/fixture", mode: "manual", enabled: true, only_new: true };
    ipc.invoke.mockResolvedValueOnce(row.id).mockResolvedValueOnce([row]);

    await expect(useAppStore.getState().addFolder(row.path, row.mode, true)).resolves.toBe(row.id);
    expect(ipc.invoke.mock.calls).toEqual([
      ["add_folder_cmd", { path: row.path, mode: row.mode, onlyNew: true }],
      ["get_folders_cmd"],
    ]);
    expect(useAppStore.getState().folders).toEqual([row]);
  });

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
    await useAppStore.getState().saveSettings(settings({ theme: "dark", autostart: true, schedule_enabled: true, recursive_watch: true }));
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

describe("archive import in Settings", () => {
  const running: ArchiveImportState = {
    sourceName: "previous.zip", previousReceipt: null,
    revision: 4, jobId: 1, status: "running", phase: "extracting", bytes: 10, files: 1,
    cancelRequested: false, result: null, error: null,
  };
  const partialReceipt = {
    extractedCount: 1, sortedCount: 0, stagingPath: "C:/archive-imports/cancelled", issue: "Archive import cancelled",
  };
  const completed: ArchiveImportState = {
    ...running, revision: 6, status: "completed", phase: null, cancelRequested: true,
    result: partialReceipt,
  };

  it("hydrates a new Settings WebView without a preceding progress event", async () => {
    const unlisten = vi.fn();
    ipc.listen.mockResolvedValue(unlisten);
    ipc.invoke.mockResolvedValueOnce(running);
    const controller = new AbortController();

    await observeArchiveImportState(controller.signal);

    expect(ipc.listen).toHaveBeenCalledWith("archive-import-state-changed", expect.any(Function));
    expect(ipc.invoke).toHaveBeenCalledWith("get_archive_import_state_cmd");
    expect(useAppStore.getState().archiveImportState).toEqual(running);
    controller.abort();
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("keeps the receipt when its initial read or an event arrives out of order", async () => {
    const read = deferred<ArchiveImportState>();
    ipc.listen.mockResolvedValue(vi.fn());
    ipc.invoke.mockReturnValueOnce(read.promise);
    const controller = new AbortController();
    const observing = observeArchiveImportState(controller.signal);
    await vi.waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith("get_archive_import_state_cmd"));
    const emit = ipc.listen.mock.calls[0][1];

    emit({ payload: completed });
    emit({ payload: { ...running, revision: 5 } });
    read.resolve(running);
    await observing;

    expect(useAppStore.getState().archiveImportState).toEqual(completed);
    controller.abort();
    emit({ payload: { ...running, revision: 7, jobId: 2 } });
    expect(useAppStore.getState().archiveImportState).toEqual(completed);
  });

  it("retains the previous snapshot when the file chooser fails before starting a job", async () => {
    useAppStore.getState().applyArchiveImportState(completed);
    await useAppStore.getState().pickArchiveImport(async () => { throw new Error("File chooser unavailable"); });
    expect(ipc.invoke).not.toHaveBeenCalled();
    expect(useAppStore.getState().archiveImportError).toBe("Error: File chooser unavailable");
    expect(useAppStore.getState().archiveImportState).toEqual(completed);
  });

  it("loads the failed job and its distinct previous receipt after the native import rejects the file", async () => {
    useAppStore.getState().applyArchiveImportState(completed);
    const failed: ArchiveImportState = {
      revision: 8, jobId: 7, sourceName: "notes.txt.gz", status: "failed", phase: null,
      bytes: 0, files: 0, cancelRequested: false, error: "Unsupported archive format", result: null,
      previousReceipt: { sourceName: "previous.zip", receipt: partialReceipt },
    };
    ipc.invoke.mockRejectedValueOnce(failed.error).mockResolvedValueOnce(failed);
    await useAppStore.getState().pickArchiveImport(async () => "C:/notes.txt.gz");

    expect(ipc.invoke.mock.calls).toEqual([
      ["import_archive_cmd", { path: "C:/notes.txt.gz" }],
      ["get_archive_import_state_cmd"],
    ]);
    expect(useAppStore.getState().archiveImportState).toEqual(failed);
    useAppStore.getState().applyArchiveImportState({ ...running, revision: 9, jobId: 9, sourceName: "third.zip" });
    expect(useAppStore.getState().archiveImportError).toBeNull();
  });

  it("cancels the observed job and leaves a newer job alone", async () => {
    const pendingCancel = deferred<void>();
    const nextJob = { ...running, revision: 7, jobId: 2 };
    useAppStore.getState().applyArchiveImportState(running);
    ipc.invoke.mockReturnValueOnce(pendingCancel.promise).mockResolvedValueOnce(nextJob);

    const cancelling = useAppStore.getState().cancelArchiveImport();
    await useAppStore.getState().cancelArchiveImport();
    expect(ipc.invoke.mock.calls).toEqual([["cancel_archive_import_cmd", { jobId: 1 }]]);
    useAppStore.getState().applyArchiveImportState(nextJob);
    pendingCancel.reject(new Error("Previous job ended"));
    await cancelling;

    expect(useAppStore.getState().archiveImportState).toEqual(nextJob);
    expect(useAppStore.getState().archiveCancelError).toBeNull();
    expect(useAppStore.getState().archiveCancelingJob).toBeNull();
  });

  it("retains the pending archive invocation in the workspace store", async () => {
    const result = { ...partialReceipt, results: [] };
    const flight = deferred<typeof result>();
    ipc.invoke.mockImplementation(async (command: string) => {
      if (command === "import_archive_cmd") return flight.promise;
      if (command === "get_archive_import_state_cmd") return completed;
      return [];
    });

    const importing = useAppStore.getState().importArchive("C:/fixture.zip");
    const busyDuringRequest = useAppStore.getState().archiveImportPending;
    flight.resolve(result);
    await expect(importing).resolves.toEqual(result);
    expect(busyDuringRequest).toBe(true);
    expect(ipc.invoke.mock.calls.map(([command]) => command)).not.toContain("get_logs_cmd");
    await vi.waitFor(() => expect(useAppStore.getState().archiveImportPending).toBe(false));
    expect(useAppStore.getState().archiveImportState?.result).toEqual(completed.result);
  });
});

describe("authoritative clean scope and history", () => {
  const restored = { status: "ok", message: null, restoredTo: "original.txt" };

  it("limits Restore all to the history IDs reviewed at confirmation", async () => {
    ipc.invoke.mockResolvedValue({ count: 0, results: [] });

    await useAppStore.getState().undoAll([42, 17]);

    expect(ipc.invoke).toHaveBeenCalledWith("undo_all_cmd", { reviewedIds: [42, 17] });
  });

  it("keeps later activity and its Undo after clearing the reviewed history", async () => {
    const late = { id: 43, source_path: "late.txt" };
    ipc.invoke.mockImplementation(async (command: string, args) => {
      if (command === "clear_logs_cmd") return expect(args).toEqual({ throughId: 42 });
      if (command === "get_logs_cmd") return [late];
      if (command === "get_history_counts_cmd") return { total: 1, undoable: 1 };
      if (command === "get_stats_cmd") return [["txt", 1]];
      throw new Error(`Unexpected IPC command ${command}`);
    });

    await useAppStore.getState().clearLogs(42);

    const { logs, historyCounts, stats } = useAppStore.getState();
    expect(logs).toEqual([late]);
    expect(historyCounts).toEqual({ total: 1, undoable: 1 });
    expect(stats).toEqual([{ file_type: "txt", count: 1 }]);
  });

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
      run: () => useAppStore.getState().undoAll([1]),
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

  it.each([null, "Import cancelled after organizing 1 files"])(
    "returns archive receipts immediately when history refresh fails (issue: %s)",
    async (issue) => {
      const history = deferred<never>();
      const result = {
        extractedCount: 2, sortedCount: 1, stagingPath: "C:/archive-imports/fixture",
        results: [["first.txt", "Documents", "C:/filed/first.txt"]], issue,
      };
      ipc.invoke.mockImplementation(async (command: string) => {
        if (command === "import_archive_cmd") return result;
        if (command === "get_archive_import_state_cmd") return {
          revision: 7, jobId: 1, status: "completed", phase: null, bytes: 10, files: 2, cancelRequested: issue !== null,
          sourceName: "fixture.zip", previousReceipt: null,
          result: { extractedCount: result.extractedCount, sortedCount: result.sortedCount, stagingPath: result.stagingPath, issue },
          error: null,
        };
        if (command === "get_logs_cmd") return history.promise;
        if (command === "get_stats_cmd") return [];
        throw new Error(`Unexpected IPC command ${command}`);
      });

      await expect(useAppStore.getState().importArchive("C:/fixture.zip")).resolves.toEqual(result);
      expect(ipc.invoke).toHaveBeenCalledWith("import_archive_cmd", { path: "C:/fixture.zip" });

      history.reject(new Error("History unavailable"));
      await vi.waitFor(() => expect(useAppStore.getState().actionRefreshErrors.history)
        .toBe("Error: History unavailable"));
    },
  );

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
