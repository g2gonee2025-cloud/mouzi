import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
  DuplicateGroup,
  CleanupFile,
  CleanupRequest,
  CleanupOutcome,
} from "../utils/cleanup";

export interface CleanupLogEntry {
  id: number | null;
  timestamp: string;
  path: string;
  prevPath: string | null;
  dest: string | null;
  action: string;
  status: string;
  undoable: boolean;
}

interface CleanupState {
  duplicates: DuplicateGroup[] | null;
  largeFiles: CleanupFile[] | null;
  staleFiles: CleanupFile[] | null;
  emptyDirs: string[] | null;
  loading: boolean;
  error: string | null;
  busy: boolean;
  results: CleanupOutcome[] | null;
  history: CleanupLogEntry[] | null;

  findDuplicates: () => Promise<void>;
  findLargeFiles: (minBytes: number) => Promise<void>;
  findStaleFiles: (days: number) => Promise<void>;
  findEmptyDirs: () => Promise<void>;
  executeCleanup: (actions: CleanupRequest[]) => Promise<void>;
  loadHistory: () => Promise<void>;
  clearResults: () => void;
  clearError: () => void;
}

export const useCleanupStore = create<CleanupState>((set) => ({
  duplicates: null,
  largeFiles: null,
  staleFiles: null,
  emptyDirs: null,
  loading: false,
  error: null,
  busy: false,
  results: null,
  history: null,

  findDuplicates: async () => {
    set({ loading: true, error: null, duplicates: null });
    try {
      const duplicates = await invoke<DuplicateGroup[]>("find_duplicates_cmd");
      set({ duplicates, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  findLargeFiles: async (minBytes: number) => {
    set({ loading: true, error: null, largeFiles: null });
    try {
      const largeFiles = await invoke<CleanupFile[]>("find_large_files_cmd", {
        minBytes,
      });
      set({ largeFiles, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  findStaleFiles: async (days: number) => {
    set({ loading: true, error: null, staleFiles: null });
    try {
      const staleFiles = await invoke<CleanupFile[]>("find_stale_files_cmd", {
        days,
      });
      set({ staleFiles, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  findEmptyDirs: async () => {
    set({ loading: true, error: null, emptyDirs: null });
    try {
      const emptyDirs = await invoke<string[]>("find_empty_dirs_cmd");
      set({ emptyDirs, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  executeCleanup: async (actions: CleanupRequest[]) => {
    if (actions.length === 0) return;
    set({ busy: true, results: null, error: null });
    try {
      const results = await invoke<CleanupOutcome[]>(
        "execute_cleanup_cmd",
        { actions },
      );
      set({ results, busy: false });
    } catch (e) {
      set({ error: String(e), busy: false });
    }
  },

  loadHistory: async () => {
    try {
      const history = await invoke<CleanupLogEntry[]>(
        "get_cleanup_logs_cmd",
        { limit: 100 },
      );
      set({ history });
    } catch {
      // non-critical
    }
  },

  clearResults: () => set({ results: null }),
  clearError: () => set({ error: null }),
}));