import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { createPreviewSlot } from "./previewRequest";
import type { DuplicateGroup, CleanupFile, CleanupRequest, CleanupOutcome, CleanupPlan } from "../utils/cleanup";

export interface CleanupLogEntry {
  id: number | null;
  timestamp: string;
  path: string;
  prevPath: string | null;
  dest: string | null;
  action: string;
  status: string;
  message?: string | null;
  undoable: boolean;
}

interface CleanupState {
  duplicates: DuplicateGroup[] | null;
  largeFiles: { files: CleanupFile[]; minBytes: number } | null;
  staleFiles: { files: CleanupFile[]; days: number } | null;
  emptyDirs: string[] | null;
  loading: boolean;
  error: string | null;
  busy: boolean;
  preparedPlan: CleanupPlan | null;
  results: CleanupOutcome[] | null;
  history: CleanupLogEntry[] | null;
  historyLoading: boolean;
  historyError: string | null;
  findDuplicates: () => Promise<void>;
  findLargeFiles: (minBytes: number) => Promise<void>;
  findStaleFiles: (days: number) => Promise<void>;
  findEmptyDirs: () => Promise<void>;
  prepareCleanup: (actions: CleanupRequest[], owner?: AbortSignal) => Promise<CleanupPlan>;
  executeCleanup: (planId: string) => Promise<void>;
  invalidateCandidates: () => void;
  loadHistory: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

let candidatesGeneration = 0;
let historyGeneration = 0;
const emptyCandidates = { duplicates: null, largeFiles: null, staleFiles: null, emptyDirs: null };

export const useCleanupStore = create<CleanupState>((set, get) => {
  const preview = createPreviewSlot();

  async function find<Key extends keyof typeof emptyCandidates>(
    key: Key,
    request: () => Promise<NonNullable<CleanupState[Key]>>,
  ) {
    if (get().busy || get().preparedPlan) return;
    const generation = ++candidatesGeneration;
    set({ loading: true, error: null, [key]: null });
    try {
      const result = await request();
      if (generation === candidatesGeneration) set({ [key]: result, loading: false });
    } catch (error) {
      if (generation === candidatesGeneration) set({ error: String(error), loading: false });
    }
  }

  return {
    ...emptyCandidates,
    loading: false, error: null, busy: false, preparedPlan: null,
    results: null, history: null, historyLoading: false, historyError: null,
    findDuplicates: () => find("duplicates", () => invoke<DuplicateGroup[]>("find_duplicates_cmd")),
    findLargeFiles: (minBytes) => find("largeFiles", async () => ({
      files: await invoke<CleanupFile[]>("find_large_files_cmd", { minBytes }),
      minBytes,
    })),
    findStaleFiles: (days) => find("staleFiles", async () => ({
      files: await invoke<CleanupFile[]>("find_stale_files_cmd", { days }),
      days,
    })),
    findEmptyDirs: () => find("emptyDirs", () => invoke<string[]>("find_empty_dirs_cmd")),
    prepareCleanup: async (actions, owner) => {
      if (!actions.length || get().busy || get().preparedPlan) throw new Error("Another cleanup is being reviewed or processed");
      const isCurrent = preview.begin(owner, () => set({ busy: false, preparedPlan: null }));
      const generation = ++candidatesGeneration;
      const snapshot = actions.map((action) => ({ ...action }));
      set({ busy: true, loading: false, error: null });
      try {
        const plan = await invoke<CleanupPlan>("prepare_cleanup_cmd", { actions: snapshot });
        owner?.throwIfAborted();
        if (generation !== candidatesGeneration) throw new Error("Cleanup candidates changed; search and review again");
        set({ preparedPlan: plan, busy: false });
        return plan;
      } catch (error) {
        if (isCurrent()) {
          preview.clear();
          set({ error: String(error), busy: false });
        }
        throw error;
      }
    },
    executeCleanup: async (planId) => {
      const plan = get().preparedPlan;
      if (!plan || plan.planId !== planId || get().busy) throw new Error("Cleanup preview changed; review again");
      preview.clear();
      ++candidatesGeneration;
      set({ busy: true, preparedPlan: null, loading: false, error: null });
      try {
        const results = await invoke<CleanupOutcome[]>("execute_cleanup_cmd", { planId });
        const removed = new Set(results.filter((r) => r.status === "ok" || r.status === "permanently_deleted").map((r) => r.path));
        set((state) => ({
          results,
          duplicates: state.duplicates?.map((g) => ({ ...g, files: g.files.filter((f) => !removed.has(f.path)) })).filter((g) => g.files.length > 1) ?? null,
          largeFiles: state.largeFiles && {
            ...state.largeFiles,
            files: state.largeFiles.files.filter((file) => !removed.has(file.path)),
          },
          staleFiles: state.staleFiles && {
            ...state.staleFiles,
            files: state.staleFiles.files.filter((file) => !removed.has(file.path)),
          },
          emptyDirs: state.emptyDirs?.filter((path) => !removed.has(path)) ?? null,
        }));
      } catch (error) {
        set({ error: String(error) });
        throw error;
      } finally {
        set({ busy: false });
        void get().loadHistory();
      }
    },
    invalidateCandidates: () => {
      ++candidatesGeneration;
      ++historyGeneration;
      // Completion can arrive before the command result. Keep these candidates
      // until per-file outcomes identify failed items that can be retried.
      if (get().busy) set({ loading: false, history: null, historyLoading: false });
      else {
        preview.clear();
        set({ ...emptyCandidates, preparedPlan: null, loading: false, history: null, historyLoading: false });
      }
    },
    loadHistory: async () => {
      const generation = ++historyGeneration;
      set({ historyLoading: true, historyError: null });
      try {
        const history = await invoke<CleanupLogEntry[]>("get_cleanup_logs_cmd", { limit: 100 });
        if (generation === historyGeneration) set({ history, historyLoading: false });
      } catch (error) {
        if (generation === historyGeneration) set({ historyError: String(error), historyLoading: false });
      }
    },
    clearResults: () => set({ results: null }),
    clearError: () => set({ error: null }),
  };
});
