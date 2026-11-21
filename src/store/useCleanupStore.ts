import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

interface CleanupHistoryCursor {
  timestamp: string;
  id: number;
  throughId: number;
}

interface CleanupHistoryPage {
  entries: CleanupLogEntry[];
  nextCursor: CleanupHistoryCursor | null;
}

interface CleanupState {
  inventoryReady: boolean | null;
  duplicates: DuplicateGroup[] | null;
  duplicateKeepers: Record<string, string>;
  largeFiles: { files: CleanupFile[] | null; minBytes: number } | null;
  staleFiles: { files: CleanupFile[] | null; days: number } | null;
  emptyDirs: string[] | null;
  unavailableRoots: Partial<Record<FinderKey, string[]>>;
  loading: boolean;
  error: string | null;
  busy: boolean;
  preparedPlan: CleanupPlan | null;
  results: CleanupOutcome[] | null;
  history: CleanupLogEntry[] | null;
  historyCursor: CleanupHistoryCursor | null;
  historyLoading: boolean;
  historyError: string | null;
  findDuplicates: () => Promise<void>;
  keepDuplicate: (group: DuplicateGroup, path: string) => void;
  findLargeFiles: (minBytes: number) => Promise<void>;
  findStaleFiles: (days: number) => Promise<void>;
  findEmptyDirs: () => Promise<void>;
  prepareCleanup: (actions: CleanupRequest[], owner?: AbortSignal) => Promise<CleanupPlan>;
  executeCleanup: (planId: string) => Promise<void>;
  invalidateCandidates: () => void;
  invalidateFileCandidates: () => void;
  observeInventoryReadiness: () => () => void;
  loadHistory: () => Promise<void>;
  loadOlderHistory: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

let candidatesGeneration = 0;
let historyGeneration = 0;
const HISTORY_PAGE_SIZE = 100;
const emptyCandidates = { duplicates: null, duplicateKeepers: {}, largeFiles: null, staleFiles: null, emptyDirs: null };
const inventoryEvents = ["scan-complete", "scan-finished", "folders-changed", "mutation-completed"];
type FinderKey = keyof typeof emptyCandidates;
type FinderResult<Candidates> = { candidates: Candidates; unavailableRoots: string[] };

function keepersIn(groups: DuplicateGroup[], choices: Record<string, string>): Record<string, string> {
  const retained: Record<string, string> = {};
  for (const group of groups) {
    const path = choices[group.hash];
    if (path && group.files.some((file) => file.path === path)) retained[group.hash] = path;
  }
  return retained;
}

async function requestCandidates<Item>(command: string, args?: Record<string, number>): Promise<FinderResult<Item[]>> {
  const result = await invoke<Item[] | FinderResult<Item[]>>(command, args);
  return Array.isArray(result) ? { candidates: result, unavailableRoots: [] } : result;
}

export const useCleanupStore = create<CleanupState>((set, get) => {
  const preview = createPreviewSlot();
  let activeFinder: { key: FinderKey; generation: number } | null = null;

  async function readHistory(older: boolean) {
    const { history, historyCursor, historyLoading } = get();
    if (older && (!history || !historyCursor || historyLoading)) return;
    const generation = ++historyGeneration;
    const cursor = older ? historyCursor : null;
    set({ historyLoading: true, historyError: null });
    try {
      const page = await invoke<CleanupHistoryPage>("get_cleanup_history_page_cmd", { limit: HISTORY_PAGE_SIZE, cursor });
      if (generation === historyGeneration) {
        set({
          history: older && history ? history.concat(page.entries) : page.entries,
          historyCursor: page.nextCursor,
          historyLoading: false,
        });
      }
    } catch (error) {
      if (generation === historyGeneration) set({ historyError: String(error), historyLoading: false });
    }
  }

  async function find<Key extends FinderKey>(
    key: Key,
    request: () => Promise<FinderResult<NonNullable<CleanupState[Key]>>>,
    waiting?: NonNullable<CleanupState[Key]>,
  ) {
    if (get().busy || get().preparedPlan) return;
    const finder = { key, generation: ++candidatesGeneration };
    const priorKeepers = get().duplicateKeepers;
    activeFinder = finder;
    const isCurrent = () => activeFinder === finder && finder.generation === candidatesGeneration;
    set((state) => ({
      loading: true,
      error: null,
      [key]: waiting ?? null,
      ...(key === "duplicates" && { duplicateKeepers: {} }),
      unavailableRoots: { ...state.unavailableRoots, [key]: [] },
    }));
    try {
      const { candidates, unavailableRoots } = await request();
      if (isCurrent()) {
        activeFinder = null;
        const found: Partial<CleanupState> = { [key]: candidates };
        if (found.duplicates) found.duplicateKeepers = keepersIn(found.duplicates, priorKeepers);
        set((state) => ({
          ...found,
          loading: false,
          unavailableRoots: { ...state.unavailableRoots, [key]: unavailableRoots },
        }));
      }
    } catch (error) {
      if (isCurrent()) {
        activeFinder = null;
        set({ error: String(error), loading: false });
      }
    }
  }

  return {
    ...emptyCandidates,
    inventoryReady: null,
    unavailableRoots: {},
    loading: false, error: null, busy: false, preparedPlan: null,
    results: null, history: null, historyCursor: null, historyLoading: false, historyError: null,
    findDuplicates: () => find("duplicates", () => requestCandidates<DuplicateGroup>("find_duplicates_cmd")),
    keepDuplicate: (group, path) => set((state) => {
      if (!state.duplicates?.includes(group) || !group.files.some((file) => file.path === path)) return state;
      return { duplicateKeepers: { ...state.duplicateKeepers, [group.hash]: path } };
    }),
    findLargeFiles: (minBytes) => find("largeFiles", async () => {
      const { candidates: files, unavailableRoots } = await requestCandidates<CleanupFile>("find_large_files_cmd", { minBytes });
      return { candidates: { files, minBytes }, unavailableRoots };
    }, { minBytes, files: null }),
    findStaleFiles: (days) => find("staleFiles", async () => {
      const { candidates: files, unavailableRoots } = await requestCandidates<CleanupFile>("find_stale_files_cmd", { days });
      return { candidates: { files, days }, unavailableRoots };
    }, { days, files: null }),
    findEmptyDirs: () => find("emptyDirs", () => requestCandidates<string>("find_empty_dirs_cmd")),
    prepareCleanup: async (actions, owner) => {
      if (!actions.length || get().busy || get().preparedPlan) throw new Error("Another cleanup is being reviewed or processed");
      const isCurrent = preview.begin(owner, () => set({ busy: false, preparedPlan: null }));
      const generation = ++candidatesGeneration;
      activeFinder = null;
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
        set((state) => {
          const duplicates = state.duplicates?.map((group) => ({
            ...group, files: group.files.filter((file) => !removed.has(file.path)),
          })).filter((group) => group.files.length > 1) ?? null;
          return {
            results,
            duplicates,
            duplicateKeepers: keepersIn(duplicates ?? [], state.duplicateKeepers),
            largeFiles: state.largeFiles && {
              ...state.largeFiles,
              files: state.largeFiles.files?.filter((file) => !removed.has(file.path)) ?? null,
            },
            staleFiles: state.staleFiles && {
              ...state.staleFiles,
              files: state.staleFiles.files?.filter((file) => !removed.has(file.path)) ?? null,
            },
            emptyDirs: state.emptyDirs?.filter((path) => !removed.has(path)) ?? null,
          };
        });
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
      activeFinder = null;
      // Completion can arrive before the command result. Keep the candidates
      // and their coverage together until per-file outcomes identify retryable items.
      if (get().busy) set({ loading: false, history: null, historyLoading: false });
      else {
        preview.clear();
        set({ ...emptyCandidates, unavailableRoots: {}, preparedPlan: null, loading: false, history: null, historyLoading: false });
      }
    },
    invalidateFileCandidates: () => {
      // Scan publication replaces inventory rows. Live directory queries and
      // exact backend review plans have their own lifetimes.
      const stoppedFileFinder = activeFinder !== null && activeFinder.key !== "emptyDirs";
      if (stoppedFileFinder) activeFinder = null;
      set((state) => {
        const { duplicates, largeFiles, staleFiles, unavailableRoots } = state;
        const { emptyDirs, ...fileCoverage } = unavailableRoots;
        if (!stoppedFileFinder && duplicates === null && !largeFiles?.files && !staleFiles?.files
            && Object.keys(fileCoverage).length === 0) return state;
        return {
          duplicates: null,
          duplicateKeepers: {},
          largeFiles: largeFiles && { minBytes: largeFiles.minBytes, files: null },
          staleFiles: staleFiles && { days: staleFiles.days, files: null },
          unavailableRoots: emptyDirs === undefined ? {} : { emptyDirs },
          ...(stoppedFileFinder ? { loading: false } : {}),
        };
      });
    },
    observeInventoryReadiness: () => {
      let stopped = false;
      let subscribed = false;
      let latestRead = 0;
      const refresh = async () => {
        if (stopped || !subscribed) return;
        const read = ++latestRead;
        set({ inventoryReady: null });
        try {
          const ready = await invoke<boolean>("has_cleanup_inventory_cmd");
          if (!stopped && read === latestRead) set({ inventoryReady: ready });
        } catch {
          // A failed lookup leaves readiness unknown.
        }
      };
      const listeners = inventoryEvents.map((event) => listen(event, () => { void refresh(); }));
      // Register first so the entry snapshot covers changes during setup.
      void Promise.all(listeners).then(() => {
        subscribed = true;
        return refresh();
      }).catch(console.error);
      return () => {
        stopped = true;
        set({ inventoryReady: null });
        for (const listener of listeners) {
          void listener.then((unlisten) => unlisten()).catch(() => {});
        }
      };
    },
    loadHistory: () => readHistory(false),
    loadOlderHistory: () => readHistory(true),
    clearResults: () => set({ results: null }),
    clearError: () => set({ error: null }),
  };
});
