import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
  status: "unscanned" | "scanning" | "complete" | "failed";
  stale: boolean;
  lastScanAt: number | null;
  error: string | null;
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
  revision: number;
  totalFiles: number;
  totalBytes: number;
  categoryBreakdown: CategoryStat[];
  largestFiles: InventoryFile[];
  recentFiles: InventoryFile[];
  watchedRoots: RootStat[];
  scanEligibleRoots: number;
  lastScanAt: number | null;
  insights: DashboardInsights;
  ageBuckets: AgeBucket[];
}

export interface WeeklyStat {
  file_type: string;
  count: number;
}

export type ActivityReadStatus = "loading" | "ready" | "error";

export interface ScanProgress {
  runId: string;
  rootCount: number;
  root: string;
  processed: number;
  total?: number | null;
}

export interface ScanComplete {
  runId: string;
  root: string;
  files: number;
  bytes: number;
  status: "complete" | "failed";
  error: string | null;
}

export interface ScanFinished {
  runId: string;
  status: "finished" | "failed";
  errors: string[];
}

export interface ScanStarted {
  started: boolean;
  reason?: string | null;
  runId?: string | null;
  rootCount: number;
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
  statsReadRevision: number;
  statsSnapshotRevision: number;
  weeklyStats: WeeklyStat[];
  activityReadStatus: ActivityReadStatus;
  isScanning: boolean;
  scanProgress: ScanProgress | null;
  completedRoots: ScanComplete[];
  rootCount: number;
  isLoading: boolean;
  error: string | null;
  scanReason: string | null;
  scanRunId: string | null;
  lastFinishedRunId: string | null;
  scanStatus: string | null;
  scanErrors: string[];

  loadStats: () => Promise<void>;
  loadWeeklyStats: () => Promise<void>;
  startScan: () => Promise<void>;
  refresh: () => Promise<void>;
  syncScanState: () => Promise<void>;
}

// Run IDs are decimal nanosecond strings: comparing them as Number loses
// precision and can make two adjacent runs indistinguishable.
function compareRuns(a: string, b: string): number {
  return a.length === b.length ? (a === b ? 0 : a < b ? -1 : 1) : a.length - b.length;
}

function obsoleteRun(runId: string, state: DashboardState): boolean {
  return Boolean(
    (state.lastFinishedRunId && compareRuns(runId, state.lastFinishedRunId) <= 0) ||
    (state.scanRunId && compareRuns(runId, state.scanRunId) < 0),
  );
}

let statsRequest = 0;
let weeklyRequest = 0;
let scanStateRevision = 0;

export const useDashboardStore = create<DashboardState>((set, get) => ({
  stats: null,
  statsReadRevision: 0,
  statsSnapshotRevision: 0,
  weeklyStats: [],
  activityReadStatus: "loading",
  isScanning: false,
  scanProgress: null,
  completedRoots: [],
  rootCount: 0,
  isLoading: false,
  error: null,
  scanReason: null,
  scanRunId: null,
  lastFinishedRunId: null,
  scanStatus: null,
  scanErrors: [],

  loadStats: async () => {
    const request = ++statsRequest;
    set({ isLoading: true, error: null });
    try {
      const stats = await invoke<DashboardStats>("get_dashboard_stats_cmd");
      if (request === statsRequest) {
        set((state) => ({
          stats,
          statsReadRevision: request,
          statsSnapshotRevision: request,
          isLoading: false,
          scanReason: state.scanReason === "no_roots" ? null : state.scanReason,
        }));
      }
    } catch (e) {
      if (request === statsRequest) set({ statsReadRevision: request, error: String(e), isLoading: false });
    }
  },

  loadWeeklyStats: async () => {
    const request = ++weeklyRequest;
    set({ weeklyStats: [], activityReadStatus: "loading" });
    try {
      const raw = await invoke<[string, number][]>("get_stats_cmd");
      if (request !== weeklyRequest) return;
      set({
        weeklyStats: raw.map(([file_type, count]) => ({ file_type, count })),
        activityReadStatus: "ready",
      });
    } catch {
      if (request === weeklyRequest) set({ activityReadStatus: "error" });
    }
  },

  startScan: async () => {
    if (get().isScanning) return;
    const startRevision = ++scanStateRevision;
    set({
      isScanning: true,
      scanProgress: null,
      completedRoots: [],
      rootCount: 0,
      error: null,
      scanReason: null,
      scanRunId: null,
      scanStatus: null,
      scanErrors: [],
    });
    try {
      const result = await invoke<ScanStarted>("start_scan_cmd");
      if (result.runId) {
        if (obsoleteRun(result.runId, get())) return;
        ++scanStateRevision;
        set({ scanRunId: result.runId, isScanning: true, rootCount: result.rootCount });
        return;
      }
      if (!result.started) {
        const revision = scanStateRevision;
        const scanning = await invoke<boolean>("is_scanning_cmd");
        if (revision !== scanStateRevision) return;
        ++scanStateRevision;
        set({
          isScanning: scanning,
          scanReason: result.reason === "no_roots" ? "no_roots" : null,
        });
        if (result.reason === "no_roots") await get().loadStats();
      }
    } catch (e) {
      if (startRevision === scanStateRevision) set({ error: String(e), isScanning: false });
    }
  },

  refresh: async () => {
    await Promise.all([get().loadStats(), get().loadWeeklyStats()]);
  },

  syncScanState: async () => {
    const revision = scanStateRevision;
    try {
      const scanning = await invoke<boolean>("is_scanning_cmd");
      if (revision === scanStateRevision) {
        set({ isScanning: scanning, ...(!scanning ? { scanProgress: null } : {}) });
      }
    } catch {
      // ignore
    }
  },
}));

/* Each mounted dashboard owns its registrations, including partial setup. */
export async function subscribeDashboardEvents(signal?: AbortSignal): Promise<() => void> {
  const unlisteners: UnlistenFn[] = [];
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    signal?.removeEventListener("abort", dispose);
    if (refreshTimer) {
      clearTimeout(refreshTimer);
      refreshTimer = null;
    }
    for (const unlisten of unlisteners.splice(0)) unlisten();
  };
  signal?.addEventListener("abort", dispose, { once: true });
  if (signal?.aborted) dispose();

  const refreshSoon = () => {
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      void useDashboardStore.getState().refresh();
    }, 200);
  };

  const register = async <T,>(name: string, handler: (payload: T) => void) => {
    if (disposed) return;
    const unlisten = await listen<T>(name, (event) => {
      if (!disposed) handler(event.payload);
    });
    if (disposed) unlisten();
    else unlisteners.push(unlisten);
  };

  try {
    await register<ScanProgress>("scan-progress", (progress) => {
      const state = useDashboardStore.getState();
      if (obsoleteRun(progress.runId, state)) return;
      ++scanStateRevision;
      useDashboardStore.setState({
        isScanning: true,
        scanRunId: progress.runId,
        scanProgress: progress,
        rootCount: progress.rootCount,
        ...(state.scanRunId !== progress.runId ? { completedRoots: [], scanErrors: [], scanStatus: null } : {}),
      });
    });
    await register<ScanComplete>("scan-complete", (completed) => {
      const state = useDashboardStore.getState();
      if (obsoleteRun(completed.runId, state)) return;
      ++scanStateRevision;
      useDashboardStore.setState({
        scanRunId: completed.runId,
        ...(state.scanRunId !== completed.runId ? { scanStatus: null, scanErrors: [] } : {}),
        completedRoots: [...(state.scanRunId === completed.runId ? state.completedRoots : [])
          .filter((root) => root.root !== completed.root), completed],
      });
      if (completed.status === "failed" ||
          state.stats?.watchedRoots.some((root) => root.path === completed.root && root.status === "failed")) {
        refreshSoon();
      }
    });
    await register<ScanFinished>("scan-finished", (finished) => {
      const state = useDashboardStore.getState();
      if ((state.scanRunId && compareRuns(finished.runId, state.scanRunId) < 0) ||
          (state.lastFinishedRunId && compareRuns(finished.runId, state.lastFinishedRunId) < 0)) return;
      ++scanStateRevision;
      useDashboardStore.setState({
        isScanning: false,
        scanProgress: null,
        scanRunId: finished.runId,
        ...(state.scanRunId !== finished.runId ? { completedRoots: [] } : {}),
        lastFinishedRunId: finished.runId,
        scanStatus: finished.status,
        scanErrors: finished.errors,
      });
      refreshSoon();
    });
    return dispose;
  } catch (error) {
    dispose();
    throw error;
  }
}

export { EMPTY_INSIGHTS };
