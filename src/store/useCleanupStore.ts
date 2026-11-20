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
  unavailableRoots: Partial<Record<FinderKey, string[]>>;
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
type FinderKey = keyof typeof emptyCandidates;
type FinderResult<Candidates> = { candidates: Candidates; unavailableRoots: string[] };

async function requestCandidates<Item>(command: string, args?: Record<string, number>): Promise<FinderResult<Item[]>> {
  const result = await invoke<Item[] | FinderResult<Item[]>>(command, args);
  return Array.isArray(result) ? { candidates: result, unavailableRoots: [] } : result;
}

export const useCleanupStore = create<CleanupState>((set, get) => {
  const preview = createPreviewSlot();

  async function find<Key extends FinderKey>(
    key: Key,
    request: () => Promise<FinderResult<NonNullable<CleanupState[Key]>>>,
  ) {
    if (get().busy || get().preparedPlan) return;
    const generation = ++candidatesGeneration;
    set((state) => ({
      loading: true,
      error: null,
      [key]: null,
      unavailableRoots: { ...state.unavailableRoots, [key]: [] },
    }));
    try {
      const { candidates, unavailableRoots } = await request();
      if (generation === candidatesGeneration) {
        set((state) => ({
          [key]: candidates,
          loading: false,
          unavailableRoots: { ...state.unavailableRoots, [key]: unavailableRoots },
        }));
      }
    } catch (error) {
      if (generation === candidatesGeneration) set({ error: String(error), loading: false });
    }
  }

  return {
    ...emptyCandidates,
    unavailableRoots: {},
    loading: false, error: null, busy: false, preparedPlan: null,
    results: null, history: null, historyLoading: false, historyError: null,
    findDuplicates: () => find("duplicates", () => requestCandidates<DuplicateGroup>("find_duplicates_cmd")),
    findLargeFiles: (minBytes) => find("largeFiles", async () => {
      const { candidates: files, unavailableRoots } = await requestCandidates<CleanupFile>("find_large_files_cmd", { minBytes });
      return { candidates: { files, minBytes }, unavailableRoots };
    }),
    findStaleFiles: (days) => find("staleFiles", async () => {
      const { candidates: files, unavailableRoots } = await requestCandidates<CleanupFile>("find_stale_files_cmd", { days });
      return { candidates: { files, days }, unavailableRoots };
    }),
    findEmptyDirs: () => find("emptyDirs", () => requestCandidates<string>("find_empty_dirs_cmd")),
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
      if (get().busy) set({ unavailableRoots: {}, loading: false, history: null, historyLoading: false });
      else {
        preview.clear();
        set({ ...emptyCandidates, unavailableRoots: {}, preparedPlan: null, loading: false, history: null, historyLoading: false });
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
