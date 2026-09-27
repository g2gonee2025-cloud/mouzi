import { useEffect, useCallback, useState } from "react";
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
import { navigateHash, fileName } from "../utils/paths";
import {
  ChevronLeft,
  LayoutDashboard,
  RefreshCw,
  Sparkles,
  Lightbulb,
  Settings,
} from "lucide-react";

export default function Dashboard() {
  const { t } = useTranslation();
  const {
    stats,
    weeklyStats,
    isScanning,
    scanProgress,
    completedRoots,
    rootCount,
    isLoading,
    error,
    scanReason,
    startScan,
    refresh,
    syncScanState,
  } = useDashboardStore();

  const [category, setCategory] = useState<string | null>(null);
  const [root, setRoot] = useState<string | null>(null);

  useEffect(() => {
    refresh();
    syncScanState();
    let unsub: (() => void) | undefined;
    let cancelled = false;
    subscribeDashboardEvents().then((u) => {
      if (cancelled) {
        u();
      } else {
        unsub = u;
      }
    });
    return () => {
      cancelled = true;
      unsub?.();
    };
  }, [refresh, syncScanState]);

  const handleFocus = useCallback(() => {
    refresh();
    syncScanState();
  }, [refresh, syncScanState]);

  useEffect(() => {
    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [handleFocus]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "r" && e.key !== "R") return;
      const tag = (document.activeElement as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
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
    setCategory((prev) => (value && prev === value ? null : value));
  };

  const toggleRoot = (value: string | null) => {
    setRoot((prev) => (value && prev === value ? null : value));
  };

  const reasonMessage =
    scanReason === "no_roots"
      ? t("dashboard.scanNoRoots")
      : scanReason === "already_running"
        ? t("dashboard.scanAlready")
        : null;

  const hasData = Boolean(stats && stats.totalFiles > 0);

  return (
    <div className="h-full flex flex-col bg-surface text-text">
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2 min-w-0">
          <button
            type="button"
            onClick={handleClose}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} />
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
          >
            <RefreshCw size={15} />
          </button>
          <button
            type="button"
            onClick={startScan}
            disabled={isScanning}
            className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 transition-colors inline-flex items-center gap-1.5"
          >
            <RefreshCw size={13} className={isScanning ? "animate-spin" : ""} />
            {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
          </button>
        </div>
      </header>

      {isScanning && (
        <div className="px-5 py-2 border-b border-border bg-surface-dark/80 text-xs text-text-muted flex items-center gap-3">
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
        {error && (
          <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            {error}
          </div>
        )}
        {reasonMessage && !isScanning && (
          <div className="rounded-lg border border-border bg-surface-dark px-4 py-3 text-sm text-text flex items-center justify-between gap-3">
            <span>{reasonMessage}</span>
            {scanReason === "no_roots" && (
              <button
                type="button"
                onClick={() => navigateHash("settings")}
                className="text-xs font-medium text-primary hover:underline inline-flex items-center gap-1"
              >
                <Settings size={12} />
                {t("dashboard.openSettings")}
              </button>
            )}
          </div>
        )}

        {isLoading && !stats ? (
          <div className="flex items-center justify-center h-64">
            <div className="animate-pulse text-sm text-text-muted">{t("app.loading")}</div>
          </div>
        ) : !hasData ? (
          <div className="flex flex-col items-center justify-center h-64 text-center px-6">
            <LayoutDashboard size={32} className="text-text-muted mb-2" />
            <p className="text-sm text-text-muted mb-4">{t("dashboard.noData")}</p>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={startScan}
                className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-white hover:bg-primary-hover transition-colors"
              >
                {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
              </button>
              <button
                type="button"
                onClick={() => navigateHash("settings")}
                className="rounded-md border border-border px-4 py-1.5 text-sm hover:bg-surface-dark transition-colors"
              >
                {t("dashboard.openSettings")}
              </button>
            </div>
          </div>
        ) : (
          <>
            <StorageRibbon
              data={stats!.categoryBreakdown}
              totalBytes={stats!.totalBytes}
              selected={category}
              onSelect={(c) => toggleCategory(c)}
            />

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
                selected={root}
                onSelect={toggleRoot}
              />
            </div>

            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <AgeHistogram data={stats!.ageBuckets ?? []} />
              <ActivityTimeline data={weeklyStats} />
            </div>

            <FileBrowser
              initialLargest={stats!.largestFiles ?? []}
              initialRecent={stats!.recentFiles ?? []}
              category={category}
              root={root}
            />
          </>
        )}
      </div>
    </div>
  );
}
