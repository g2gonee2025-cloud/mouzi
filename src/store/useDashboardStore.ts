import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

/* ─── Types ─────────────────────────────────────────────────── */

export interface DashboardStats {
  totalFiles: number;
  totalBytes: number;
  categoryBreakdown: Array<{ category: string; files: number; bytes: number }>;
  largestFiles: Array<{ path: string; size: number; mtime: number }>;
  watchedRoots: Array<{ path: string; files: number; bytes: number }>;
  lastScanAt: number | null;
}

export interface WeeklyStat {
  file_type: string;
  count: number;
}

export interface ScanProgress {
  root: string;
  processed: number;
}

export interface ScanComplete {
  root: string;
  files: number;
  bytes: number;
}

/* ─── Store ─────────────────────────────────────────────────── */

interface DashboardState {
  stats: DashboardStats | null;
  weeklyStats: WeeklyStat[];
  isScanning: boolean;
  scanProgress: ScanProgress | null;
  isLoading: boolean;
  error: string | null;

  loadStats: () => Promise<void>;
  loadWeeklyStats: () => Promise<void>;
  startScan: () => Promise<void>;
  refresh: () => Promise<void>;
}

export const useDashboardStore = create<DashboardState>((set, get) => ({
  stats: null,
  weeklyStats: [],
  isScanning: false,
  scanProgress: null,
  isLoading: false,
  error: null,

  loadStats: async () => {
    set({ isLoading: true, error: null });
    try {
      const stats = await invoke<DashboardStats>("get_dashboard_stats_cmd");
      set({ stats, isLoading: false });
    } catch (e) {
      set({ error: String(e), isLoading: false });
    }
  },

  loadWeeklyStats: async () => {
    try {
      const raw = await invoke<[string, number][]>("get_stats_cmd");
      set({
        weeklyStats: raw.map(([file_type, count]) => ({ file_type, count })),
      });
    } catch {
      // non-critical — weekly stats may be empty
    }
  },

  startScan: async () => {
    set({ isScanning: true, scanProgress: null, error: null });
    try {
      await invoke<{ started: boolean }>("start_scan_cmd");
    } catch (e) {
      set({ error: String(e), isScanning: false });
    }
  },

  refresh: async () => {
    await Promise.all([get().loadStats(), get().loadWeeklyStats()]);
  },
}));

/* ─── Event listeners (call once on mount) ──────────────────── */

let scanProgressUnlisten: UnlistenFn | null = null;
let scanCompleteUnlisten: UnlistenFn | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;

// Multiple roots emit one scan-complete each — debounce the stat reload
// so a many-folder scan results in a single refresh, not a storm.
function scheduleRefreshAfterScan() {
  useDashboardStore.setState({ isScanning: false, scanProgress: null });
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    useDashboardStore.getState().refresh();
  }, 200);
}

export async function subscribeDashboardEvents(): Promise<() => void> {
  scanProgressUnlisten = await listen<ScanProgress>("scan-progress", (event) => {
    useDashboardStore.setState({ scanProgress: event.payload });
  });

  scanCompleteUnlisten = await listen<ScanComplete>("scan-complete", () => {
    scheduleRefreshAfterScan();
  });

  return () => {
    if (refreshTimer) {
      clearTimeout(refreshTimer);
      refreshTimer = null;
    }
    scanProgressUnlisten?.();
    scanCompleteUnlisten?.();
  };
}