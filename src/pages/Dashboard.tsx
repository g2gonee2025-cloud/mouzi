import { useEffect, useCallback, useState, type Dispatch, type SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  useDashboardStore,
  subscribeDashboardEvents,
  EMPTY_INSIGHTS,
} from "../store/useDashboardStore";
import StatCards from "../components/dashboard/StatCards";
import StorageRibbon from "../components/dashboard/StorageRibbon";
import StorageTreemap from "../components/dashboard/StorageTreemap";
import CategoryBars from "../components/dashboard/CategoryBars";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import AgeHistogram from "../components/dashboard/AgeHistogram";
import InsightCards from "../components/dashboard/InsightCards";
import FileBrowser from "../components/dashboard/FileBrowser";
import { clearMissingDashboardRoot, type DashboardViewOptions } from "../utils/dashboard";
import { navigateHash, fileName } from "../utils/paths";
import {
  ChevronLeft,
  LayoutDashboard,
  RefreshCw,
  Sparkles,
  Lightbulb,
  Settings,
} from "lucide-react";

interface DashboardProps {
  view: DashboardViewOptions;
  setView: Dispatch<SetStateAction<DashboardViewOptions>>;
}

export default function Dashboard({ view, setView }: DashboardProps) {
  const { t } = useTranslation();
  const {
    stats: cachedStats,
    statsReadRevision,
    statsSnapshotRevision,
    weeklyStats,
    activityReadStatus,
    loadWeeklyStats,
    isLoading,
    isScanning,
    scanProgress,
    completedRoots,
    rootCount,
    error: cachedError,
    scanReason,
    scanErrors,
    scanStatus,
    lastFinishedRunId,
    startScan,
    refresh,
    syncScanState,
  } = useDashboardStore();

  const { category, root } = view;
  const [scanListenersReady, setScanListenersReady] = useState(false);
  const [scanListenerError, setScanListenerError] = useState<string | null>(null);
  const [scanListenerAttempt, setScanListenerAttempt] = useState(0);
  // On re-entry, wait for a successful read before showing a previous route's inventory.
  const [entry] = useState({ statsReadRevision, statsSnapshotRevision, lastFinishedRunId });
  const stats = statsSnapshotRevision !== entry.statsSnapshotRevision ? cachedStats : null;
  const error = statsReadRevision !== entry.statsReadRevision ? cachedError : null;

  useEffect(() => {
    const controller = new AbortController();
    void subscribeDashboardEvents(controller.signal).then(() => {
      if (!controller.signal.aborted) {
        setScanListenersReady(true);
        void refresh();
        void syncScanState();
      }
    }).catch((error) => {
      if (!controller.signal.aborted) {
        setScanListenerError(String(error));
        void refresh();
      }
    });
    return () => controller.abort();
  }, [refresh, scanListenerAttempt, syncScanState]);

  const activeRoot = stats?.watchedRoots.some((item) => item.path === root) ? root : null;
  useEffect(() => {
    if (root && stats && !activeRoot) setView((current) => clearMissingDashboardRoot(current, root));
  }, [root, stats, activeRoot, setView]);

  const handleFocus = useCallback(() => {
    void refresh();
    if (scanListenersReady) void syncScanState();
  }, [refresh, scanListenersReady, syncScanState]);

  useEffect(() => {
    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [handleFocus]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "r" && e.key !== "R") return;
      if (e.defaultPrevented || e.isComposing || e.ctrlKey || e.metaKey || e.altKey) return;
      const active = document.activeElement as HTMLElement | null;
      if (active?.matches("input, textarea, select") || active?.isContentEditable) return;
      e.preventDefault();
      refresh();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [refresh]);

  const handleClose = () => {
    invoke("close_settings").catch(console.error);
  };

  const toggleCategory = (value: string | null) => {
    setView((current) => ({ ...current, category: current.category === value ? null : value }));
  };

  const toggleRoot = (value: string | null) => {
    setView((current) => ({ ...current, root: current.root === value ? null : value }));
  };

  const hasRoots = Boolean(stats?.watchedRoots.length);
  const canScan = Boolean(stats && stats.scanEligibleRoots > 0 && scanReason !== "no_roots");
  const failedRoots = stats?.watchedRoots.filter((item) => item.status === "failed") ?? [];
  const scanFailed = scanStatus === "failed" && lastFinishedRunId !== entry.lastFinishedRunId;
  const failedPaths = new Set(failedRoots.map(({ path }) => path));
  const reportedRootErrors = new Set([
    ...failedRoots.map(({ error }) => error),
    ...completedRoots.filter((item) => item.status === "failed" && failedPaths.has(item.root)).map(({ error }) => error),
  ]);
  const remainingScanErrors = scanFailed ? scanErrors.filter((message) => !reportedRootErrors.has(message)) : [];

  return (
    <div className="h-full flex flex-col bg-surface text-text">
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2 min-w-0">
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("common.back")}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} aria-hidden="true" />
          </button>
          <LayoutDashboard size={18} className="text-primary shrink-0" />
          <h1 className="text-base font-semibold truncate">{t("dashboard.title")}</h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => refresh()}
            className="p-1.5 rounded-md hover:bg-border text-text-muted hover:text-text transition-colors"
            title={t("dashboard.refresh")}
            aria-label={t("dashboard.refresh")}
          >
            <RefreshCw size={15} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={startScan}
            disabled={isScanning || isLoading || !scanListenersReady || !canScan}
            className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50 transition-colors inline-flex items-center gap-1.5"
          >
            <RefreshCw size={13} className={isScanning ? "animate-spin" : ""} />
            {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
          </button>
        </div>
      </header>

      {isScanning && (
        <div role="status" aria-live="polite" className="px-5 py-2 border-b border-border bg-surface-dark/80 text-xs text-text-muted flex items-center gap-3">
          <div className="scan-bar flex-1" />
          <span className="tabular-nums shrink-0">
            {scanProgress
              ? `${fileName(scanProgress.root)} · ${scanProgress.processed.toLocaleString()} ${t("dashboard.filesProcessed")}`
              : t("dashboard.scanning")}
            {rootCount > 0
              ? ` · ${t("dashboard.scanRoots", { done: completedRoots.length, total: rootCount })}`
              : ""}
          </span>
        </div>
      )}

      <div className="flex-1 overflow-auto p-5 space-y-4">
        {scanListenerError && (
          <div role="alert" className="rounded-lg border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            <p>{t("dashboard.scanConnectionFailed")}</p>
            <p className="mt-1 break-words">{scanListenerError}</p>
            <button
              type="button"
              onClick={() => {
                setScanListenerError(null);
                setScanListenerAttempt((attempt) => attempt + 1);
              }}
              className="mt-2 rounded-md border border-current px-3 py-1 font-medium"
            >
              {t("common.retry")}
            </button>
          </div>
        )}
        {error && (
          <div role="alert" className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            {error}
          </div>
        )}
        {scanFailed && (remainingScanErrors.length > 0 || failedRoots.length === 0) && (
          <div role="alert" className="rounded-lg border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            <p>{t("dashboard.scanFailed")}</p>
            {remainingScanErrors.map((message, index) => <p key={index} className="mt-1 break-words">{message}</p>)}
          </div>
        )}
        {failedRoots.length > 0 && (
          <div role="alert" className="rounded-lg border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            <p className="font-medium">{t("dashboard.scanFailedRoots")}</p>
            <ul className="mt-2 space-y-2">
              {failedRoots.map((item) => (
                <li key={item.path} className="break-words">
                  <p className="font-medium [overflow-wrap:anywhere]">{item.path}</p>
                  <p>{t(item.lastScanAt === null ? "dashboard.scanNoSnapshot" : "dashboard.scanRetainedSnapshot")}</p>
                  {item.error && <p>{item.error}</p>}
                </li>
              ))}
            </ul>
          </div>
        )}
        {hasRoots && !canScan && !isScanning && (
          <div className="rounded-lg border border-border bg-surface-dark px-4 py-3 text-sm text-text flex items-center justify-between gap-3">
            <span role="status">{t("dashboard.scanInactiveRoots")}</span>
            <button
              type="button"
              onClick={() => navigateHash("settings")}
              className="text-xs font-medium text-primary hover:underline inline-flex items-center gap-1"
            >
              <Settings size={12} aria-hidden="true" />
              {t("dashboard.openSettings")}
            </button>
          </div>
        )}
        {!stats && !error ? (
          <div className="flex items-center justify-center h-64">
            <div className="animate-pulse text-sm text-text-muted">{t("app.loading")}</div>
          </div>
        ) : error && !stats ? (
          <div className="flex items-center justify-center h-64">
            <button type="button" onClick={() => refresh()} className="rounded-md border border-border px-4 py-1.5 text-sm hover:bg-surface-dark">
              {t("common.retry")}
            </button>
          </div>
        ) : !hasRoots ? (
          <div className="flex flex-col items-center justify-center h-64 text-center px-6">
            <LayoutDashboard size={32} className="text-text-muted mb-2" />
            <p className="text-sm text-text-muted mb-4">{t("dashboard.scanNoRoots")}</p>
            <button
              type="button"
              onClick={() => navigateHash("settings")}
              className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-on-primary hover:bg-primary-hover transition-colors"
            >
              {t("dashboard.openSettings")}
            </button>
          </div>
        ) : (
          <>
            <FileBrowser
              initialLargest={stats!.largestFiles ?? []}
              initialRecent={stats!.recentFiles ?? []}
              sort={view.sort}
              query={view.query}
              category={category}
              root={activeRoot}
              rootScope={JSON.stringify(stats!.watchedRoots.map((item) => item.path).sort())}
              totalFiles={stats!.totalFiles}
              revision={stats!.revision}
              onSort={(sort) => setView((current) => ({ ...current, sort }))}
              onQuery={(query) => setView((current) => ({ ...current, query }))}
              onClearCategory={() => toggleCategory(null)}
              onClearRoot={() => toggleRoot(null)}
            />

            {(canScan || stats!.totalBytes > 0) && (
              <StorageRibbon
                data={stats!.categoryBreakdown}
                totalBytes={stats!.totalBytes}
                selected={category}
                onSelect={(c) => toggleCategory(c)}
              />
            )}

            <StatCards
              totalFiles={stats!.totalFiles}
              totalBytes={stats!.totalBytes}
              watchedRoots={stats!.watchedRoots.length}
              lastScanAt={stats!.lastScanAt}
            />

            <InsightCards
              insights={stats!.insights ?? EMPTY_INSIGHTS}
              onOpen={(dest) => {
                if (dest === "suggestions") navigateHash("suggestions");
                else navigateHash("cleanup", { tab: dest });
              }}
            />

            <div className="flex flex-wrap gap-2">
              <button
                type="button"
                onClick={() => navigateHash("cleanup")}
                className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-dark px-3 py-1.5 text-xs font-medium hover:border-primary/50 transition-colors"
              >
                <Sparkles size={13} className="text-primary" />
                {t("cleanup.open")}
              </button>
              <button
                type="button"
                onClick={() => navigateHash("suggestions")}
                className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-dark px-3 py-1.5 text-xs font-medium hover:border-primary/50 transition-colors"
              >
                <Lightbulb size={13} className="text-primary" />
                {t("dashboard.openSuggestions")}
              </button>
            </div>

            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <CategoryBars
                data={stats!.categoryBreakdown}
                totalBytes={stats!.totalBytes}
                selected={category}
                onSelect={toggleCategory}
              />
              <StorageTreemap
                data={stats!.watchedRoots}
                totalBytes={stats!.totalBytes}
                selected={activeRoot}
                onSelect={toggleRoot}
              />
            </div>

            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <AgeHistogram data={stats!.ageBuckets ?? []} />
              <ActivityTimeline data={weeklyStats} status={activityReadStatus} onRetry={loadWeeklyStats} />
            </div>
          </>
        )}
      </div>
    </div>
  );
}
