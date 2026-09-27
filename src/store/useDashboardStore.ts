import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type { WatchedFolder } from "./useAppStore";

/* ─── Types ─────────────────────────────────────────────────── */

export interface CategoryStat {
  category: string;
  files: number;
  bytes: number;
}

export interface InventoryFile {
  path: string;
  size: number;
  mtime: number;
  category: string;
  rootPath: string;
}

export interface RootStat {
  path: string;
  files: number;
  bytes: number;
}

export interface DashboardInsights {
  largeFiles: number;
  largeBytes: number;
  staleFiles: number;
  staleBytes: number;
  sameSizeGroups: number;
  sameSizeExtraBytes: number;
  otherFiles: number;
}

export interface AgeBucket {
  bucket: string;
  files: number;
  bytes: number;
}

export interface DashboardStats {
  totalFiles: number;
  totalBytes: number;
  categoryBreakdown: CategoryStat[];
  largestFiles: InventoryFile[];
  recentFiles: InventoryFile[];
  watchedRoots: RootStat[];
  lastScanAt: number | null;
  insights: DashboardInsights;
  ageBuckets: AgeBucket[];
}

export interface WeeklyStat {
  file_type: string;
  count: number;
}

export interface ScanProgress {
  root: string;
  processed: number;
  total?: number | null;
}

export interface ScanComplete {
  root: string;
  files: number;
  bytes: number;
}

export interface ScanStarted {
  started: boolean;
  reason?: string | null;
}

const EMPTY_INSIGHTS: DashboardInsights = {
  largeFiles: 0,
  largeBytes: 0,
  staleFiles: 0,
  staleBytes: 0,
  sameSizeGroups: 0,
  sameSizeExtraBytes: 0,
  otherFiles: 0,
};

/* ─── Store ─────────────────────────────────────────────────── */

interface DashboardState {
  stats: DashboardStats | null;
  weeklyStats: WeeklyStat[];
  isScanning: boolean;
  scanProgress: ScanProgress | null;
  completedRoots: ScanComplete[];
  rootCount: number;
  isLoading: boolean;
  error: string | null;
  scanReason: string | null;

  loadStats: () => Promise<void>;
  loadWeeklyStats: () => Promise<void>;
  startScan: () => Promise<void>;
  refresh: () => Promise<void>;
  syncScanState: () => Promise<void>;
}

export const useDashboardStore = create<DashboardState>((set, get) => ({
  stats: null,
  weeklyStats: [],
  isScanning: false,
  scanProgress: null,
  completedRoots: [],
  rootCount: 0,
  isLoading: false,
  error: null,
  scanReason: null,

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
    set({
      isScanning: true,
      scanProgress: null,
      completedRoots: [],
      error: null,
      scanReason: null,
    });
    try {
      const folders = await invoke<WatchedFolder[]>("get_folders_cmd");
      const active = folders.filter((f) => f.mode !== "paused");
      set({ rootCount: active.length });

      const result = await invoke<ScanStarted>("start_scan_cmd");
      if (!result.started) {
        const scanning = await invoke<boolean>("is_scanning_cmd");
        if (scanning) {
          set({ isScanning: true, scanReason: result.reason ?? "already_running" });
          return;
        }
        set({
          isScanning: false,
          scanReason: result.reason ?? "no_roots",
        });
      }
    } catch (e) {
      set({ error: String(e), isScanning: false });
    }
  },

  refresh: async () => {
    await Promise.all([get().loadStats(), get().loadWeeklyStats()]);
  },

  syncScanState: async () => {
    try {
      const scanning = await invoke<boolean>("is_scanning_cmd");
      set({ isScanning: scanning });
    } catch {
      // ignore
    }
  },
}));

/* ─── Event listeners (call once on mount) ──────────────────── */

let scanProgressUnlisten: UnlistenFn | null = null;
let scanCompleteUnlisten: UnlistenFn | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;

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
    useDashboardStore.setState({
      isScanning: true,
      scanProgress: event.payload,
    });
  });

  scanCompleteUnlisten = await listen<ScanComplete>("scan-complete", async (event) => {
    const completed = event.payload;
    useDashboardStore.setState((s) => ({
      completedRoots: s.completedRoots.some((r) => r.root === completed.root)
        ? s.completedRoots
        : [...s.completedRoots, completed],
    }));
    try {
      const scanning = await invoke<boolean>("is_scanning_cmd");
      if (!scanning) {
        scheduleRefreshAfterScan();
      } else {
        useDashboardStore.getState().refresh();
      }
    } catch {
      scheduleRefreshAfterScan();
    }
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

export { EMPTY_INSIGHTS };
