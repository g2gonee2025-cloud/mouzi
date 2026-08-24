import { useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useDashboardStore, subscribeDashboardEvents } from "../store/useDashboardStore";
import StatCards from "../components/dashboard/StatCards";
import StorageTreemap from "../components/dashboard/StorageTreemap";
import CategoryBars from "../components/dashboard/CategoryBars";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import LargestFiles from "../components/dashboard/LargestFiles";
import { ChevronLeft, LayoutDashboard, Sparkles } from "lucide-react";

export default function Dashboard() {
  const { t } = useTranslation();
  const {
    stats,
    weeklyStats,
    isScanning,
    scanProgress,
    isLoading,
    error,
    startScan,
    refresh,
  } = useDashboardStore();

  // Initial load + event subscription
  useEffect(() => {
    refresh();
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
  }, [refresh]);

  // Refresh on window focus
  const handleFocus = useCallback(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [handleFocus]);

  const handleClose = () => {
    getCurrentWebviewWindow().close().catch(console.error);
  };

  return (
    <div className="h-full flex flex-col bg-surface text-text">
      {/* Header */}
      <header className="flex items-center justify-between px-5 py-3 border-b border-border bg-surface-dark shrink-0">
        <div className="flex items-center gap-2">
          <button
            onClick={handleClose}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={18} />
          </button>
          <LayoutDashboard size={18} className="text-primary" />
          <h1 className="text-base font-semibold">{t("dashboard.title")}</h1>
        </div>
      </header>

      {/* Content */}
      <div className="flex-1 overflow-auto p-5 space-y-4">
        {error && (
          <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-sm text-red-700 dark:text-red-400">
            {error}
          </div>
        )}

        {/* Cleanup card */}
        {!isLoading && (
          <div className="rounded-lg border border-border bg-surface-dark p-4 flex items-center justify-between">
            <div className="flex items-center gap-3">
              <Sparkles size={20} className="text-primary" />
              <div>
                <div className="text-sm font-semibold text-text">
                  {t("cleanup.title")}
                </div>
                <div className="text-xs text-text-muted">
                  {t("cleanup.dashboardHint")}
                </div>
              </div>
            </div>
            <button
              onClick={() => {
                window.location.hash = "#/cleanup";
              }}
              className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-white hover:bg-primary-hover transition-colors"
            >
              {t("cleanup.open")}
            </button>
          </div>
        )}

        {isLoading && !stats ? (
          <div className="flex items-center justify-center h-64">
            <div className="animate-pulse text-sm text-text-muted">{t("app.loading")}</div>
          </div>
        ) : !isLoading && !stats ? (
          <div className="flex flex-col items-center justify-center h-64 text-center">
            <LayoutDashboard size={32} className="text-text-muted mb-2" />
            <p className="text-sm text-text-muted">{t("dashboard.noData")}</p>
            <button
              onClick={startScan}
              className="mt-3 rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-white hover:bg-primary-hover transition-colors"
            >
              {isScanning ? t("dashboard.scanning") : t("dashboard.scanNow")}
            </button>
          </div>
        ) : (
          <>
            {/* Stat Cards + Scan Button */}
            <StatCards
              totalFiles={stats?.totalFiles ?? 0}
              totalBytes={stats?.totalBytes ?? 0}
              watchedRoots={stats?.watchedRoots.length ?? 0}
              lastScanAt={stats?.lastScanAt ?? null}
              isScanning={isScanning}
              scanProgress={scanProgress}
              onScan={startScan}
            />

            {/* Two-column layout */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <StorageTreemap
                data={stats?.watchedRoots ?? []}
                totalBytes={stats?.totalBytes ?? 0}
              />
              <CategoryBars
                data={stats?.categoryBreakdown ?? []}
                totalBytes={stats?.totalBytes ?? 0}
              />
            </div>

            {/* Activity Timeline */}
            <ActivityTimeline data={weeklyStats} />

            {/* Largest Files */}
            <LargestFiles data={stats?.largestFiles ?? []} />
          </>
        )}
      </div>
    </div>
  );
}