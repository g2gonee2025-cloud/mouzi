import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAppStore } from "../store/useAppStore";
import ActionRefreshWarnings from "./ActionRefreshWarnings";
import {
  FolderOpen,
  Sparkles,
  RotateCcw,
  X,
  FileText,
  Image,
  Music,
  Video,
  Archive,
  Package,
  File,
  ExternalLink,
  Inbox,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { UNDO_TONE_CLASS, undoNotice, type UndoNotice } from "../utils/undo";

function getIconForType(typeName: string) {
  const lower = typeName.toLowerCase();
  if (lower.includes("image")) return <Image size={14} />;
  if (lower.includes("music")) return <Music size={14} />;
  if (lower.includes("video")) return <Video size={14} />;
  if (lower.includes("archive")) return <Archive size={14} />;
  if (lower.includes("install")) return <Package size={14} />;
  if (lower.includes("document")) return <FileText size={14} />;
  return <File size={14} />;
}

function getFolderFromPath(filePath: string | null | undefined): string | null {
  if (!filePath) return null;
  const lastSlash = Math.max(filePath.lastIndexOf("/"), filePath.lastIndexOf("\\"));
  if (lastSlash === -1) return filePath;
  return filePath.slice(0, lastSlash) || filePath;
}

export default function Popup() {
  const { t } = useTranslation();
  const {
    logs,
    stats,
    isLoading,
    loadLogs,
    loadStats,
    loadFolders,
    undoAction,
    folders,
    pendingFiles,
    getPendingFiles,
  } = useAppStore();
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [toast, setToast] = useState<{
    file: string;
    rule: string;
    destination: string;
    destination_folder: string;
  } | null>(null);
  const [undoToast, setUndoToast] = useState<UndoNotice[] | null>(null);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 30000);
    return () => clearTimeout(timer);
  }, [toast]);

  // 8s, not the 3s the other popups use: a `collision` line ends in a
  // filesystem path, and 3s cuts it off before it can be read or copied.
  useEffect(() => {
    if (!undoToast) return;
    const timer = setTimeout(() => setUndoToast(null), 8000);
    return () => clearTimeout(timer);
  }, [undoToast]);

  useEffect(() => {
    loadLogs().catch(console.error);
    loadStats().catch(console.error);
    loadFolders().catch(console.error);
    getPendingFiles();

    // Listen for file-organized events from Rust watcher — show in-app toast
    const unlisten = listen("file-organized", (event: any) => {
      const payload = event.payload;
      if (payload?.success) {
        const destFolder: string = payload.destination_folder || payload.destination;
        // Show in-app toast (popup must be open/visible for this to appear)
        setToast({
          file: payload.file,
          rule: payload.rule,
          destination: payload.destination,
          destination_folder: destFolder,
        });
        loadLogs().catch(console.error);
        loadStats().catch(console.error);
        getPendingFiles();
      }
    });

    // Poll for pending files in manual-mode folders
    const interval = setInterval(() => {
      getPendingFiles();
    }, 3000);

    return () => {
      unlisten.then((f) => f()).catch(console.error);
      clearInterval(interval);
    };
  }, [loadLogs, loadStats, loadFolders, getPendingFiles]);



  const openReview = async (paths: string[]) => {
    setReviewError(null);
    try {
      await invoke("show_review_cmd", { paths });
      setSelectedFiles(new Set());
    } catch (error) {
      setReviewError(String(error));
    }
  };

  const pendingPaths = pendingFiles.map(([folder, name]) => ({
    name,
    path: folder.replace(/[\\/]$/, "") + (folder.includes("\\") ? "\\" : "/") + name,
  }));
  const selectedPaths = pendingPaths.filter(({ path }) => selectedFiles.has(path)).map(({ path }) => path);

  const handleOpenDownloads = async () => {
    const downloads = folders[0]?.path || (await invoke<string>("get_downloads_folder"));
    await invoke("open_folder_cmd", { path: downloads });
  };

  const handleOpenActionFolder = async (filePath: string | null | undefined) => {
    const folderPath = getFolderFromPath(filePath);
    if (!folderPath) return;
    try {
      await invoke("open_folder_cmd", { path: folderPath });
    } catch {
      console.error("Failed to open folder");
    }
  };

  const handleQuit = () => {
    invoke("close_popup");
  };

  /**
   * `undo_action_cmd` is a no-op on the row when the source is gone, yet the
   * backend still flips `undone`, so the row below re-renders as a finished
   * item. Reading the status here is the only thing that stops a file which is
   * still sitting in its organised folder being reported as restored.
   */
  const handleUndo = async (id: number | undefined) => {
    if (id === undefined) {
      setUndoToast([{ tone: "error", key: "undo.noId" }]);
      return;
    }
    try {
      setUndoToast(undoNotice(await undoAction(id)));
    } catch (e) {
      console.error("Undo failed:", e);
      setUndoToast([
        { tone: "error", key: "confirm.failed", values: { error: String(e) } },
      ]);
    }
  };

  const totalStats = stats.reduce((sum, s) => sum + s.count, 0);

  return (
    <div className="flex h-full flex-col bg-surface text-text border border-border overflow-hidden">
      {/* Header */}
      <div data-tauri-drag-region className="flex items-center justify-between px-4 py-3 bg-surface-dark border-b border-border">
        <div className="flex items-center gap-2">
          <Sparkles size={16} className="text-primary" />
          <span className="font-semibold text-sm">{t("popup.title")}</span>
        </div>
        <button
          onClick={handleQuit}
          className="p-1.5 rounded-md hover:bg-border transition-colors"
          title={t("popup.quit")}
          aria-label={t("popup.quit")}
        >
          <X size={14} />
        </button>
      </div>

      {/* Actions */}
      <div className="p-3 space-y-2">
        <button
          onClick={() => void openReview([])}
          disabled={isLoading}
          className="w-full flex items-center justify-center gap-2 rounded-lg bg-primary px-3 py-2.5 text-sm font-medium text-on-primary hover:bg-primary-hover transition-colors disabled:opacity-60"
        >
          <Sparkles size={15} />
          {isLoading ? "..." : t("popup.cleanNow")}
        </button>
        {pendingFiles.length > 0 && (
          <div className="text-center text-xs text-text-muted">
            {t("popup.pendingFiles", { count: pendingFiles.length })}
          </div>
        )}
        {reviewError && <p role="alert" className="mt-2 break-words text-xs text-red-600">{reviewError}</p>}
        <ActionRefreshWarnings />
        {pendingPaths.length > 0 && <div className="mt-2 space-y-2">
          <div className="max-h-24 space-y-1 overflow-auto text-xs">
            {pendingPaths.map(({ name, path }) => <label key={path} className="flex items-center gap-2" title={path}>
              <input type="checkbox" checked={selectedFiles.has(path)} onChange={(event) => {
                const checked = event.target.checked;
                setSelectedFiles((previous) => {
                  const next = new Set(previous);
                  if (checked) next.add(path);
                  else next.delete(path);
                  return next;
                });
              }} />
              <span className="truncate">{name}</span>
            </label>)}
          </div>
          <button type="button" disabled={!selectedPaths.length} className="text-xs text-primary underline disabled:opacity-50"
            onClick={() => void openReview(selectedPaths)}>{t("beta.review")} ({selectedPaths.length})</button>
        </div>}

        <button
          onClick={handleOpenDownloads}
          className="w-full flex items-center justify-center gap-2 rounded-lg border border-border px-3 py-2 text-sm font-medium hover:bg-surface-dark transition-colors"
        >
          <FolderOpen size={15} />
          {t("popup.openDownloads")}
        </button>
      </div>

      {/* Recent */}
      <div className="flex-1 overflow-auto px-3">
        <div className="text-xs font-semibold text-text-muted uppercase tracking-wider mb-2">
          {t("popup.recentActions")}
        </div>
        {logs.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-6 text-sm text-text-muted">
            <Inbox size={28} className="mb-2 opacity-60" />
            {t("popup.noActions")}
          </div>
        ) : (
          <div className="space-y-1.5">
            {logs.slice(0, 5).map((log) => (
              <div
                key={log.id}
                className="flex items-center gap-2 rounded-md bg-surface-dark px-2.5 py-2 text-xs"
              >
                <span className="text-text-muted shrink-0">
                  {getIconForType(log.file_type)}
                </span>
                <span className="flex-1 truncate" title={log.file_name}>
                  {log.file_name}
                </span>
                <span className="text-text-muted truncate max-w-[80px]">
                  {log.file_type}
                </span>
                {!log.undone && log.id && (
                  <>
                    <button
                      onClick={() => handleOpenActionFolder(log.destination_path)}
                      className="p-1 rounded hover:bg-border text-text-muted hover:text-text"
                      title={t("cleanup.openFolder")}
                      aria-label={t("cleanup.openFolder")}
                    >
                      <FolderOpen size={12} />
                    </button>
                    <button
                      onClick={() => void handleUndo(log.id)}
                      className="p-1 rounded hover:bg-border text-text-muted hover:text-text"
                      title={t("popup.undo")}
                      aria-label={t("popup.undo")}
                    >
                      <RotateCcw size={12} />
                    </button>
                  </>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Stats */}
      {stats.length > 0 && (
        <div className="border-t border-border p-3">
          <div className="text-xs font-semibold text-text-muted uppercase tracking-wider mb-2">
            {t("popup.weeklyStats")}
          </div>
          <div className="space-y-1.5">
            {stats.map((s) => {
              const pct = totalStats > 0 ? Math.round((s.count / totalStats) * 100) : 0;
              return (
                <div key={s.file_type} className="flex items-center gap-2 text-xs">
                  <span className="w-16 truncate text-text-muted">{s.file_type}</span>
                  <div className="flex-1 h-1.5 bg-border rounded-full overflow-hidden">
                    <div
                      className="h-full bg-primary rounded-full transition-all"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="w-8 text-right text-text-muted">{pct}%</span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Clickable toast for auto-organized files */}
      {toast && (
        <div className="px-3 pb-2">
          <div className="w-full rounded-lg bg-primary/10 border border-primary/20 px-3 py-2 text-xs">
            <div className="flex items-center justify-between">
              <button type="button" className="flex items-center gap-1.5 text-primary text-left hover:underline" onClick={async () => {
                try {
                  await invoke("open_folder_cmd", { path: toast.destination_folder });
                  setToast(null);
                } catch (e) {
                  setUndoToast([{ tone: "error", key: "confirm.failed", values: { error: String(e) } }]);
                }
              }}>
                <ExternalLink size={12} />
                <span className="font-medium">{t("popup.organized", { file: toast.file })}</span>
              </button>
              <button
                type="button"
                onClick={() => setToast(null)}
                aria-label={t("common.dismiss")}
                className="p-0.5 rounded hover:bg-primary/20 text-primary"
              >
                <X size={10} />
              </button>
            </div>
            <div className="text-text-muted mt-0.5 truncate">
              {t("popup.openFolder", { folder: toast.destination_folder })}
            </div>
          </div>
        </div>
      )}

      {/* Pinned near the bottom of the tray, where the rows themselves offer no
          correction: a `missing` undo just makes the row's Undo button vanish,
          which looks exactly like a file that went home. */}
      {undoToast && (
        <div
          role="status"
          aria-live="polite"
          className="space-y-1 px-3 pb-2"
        >
          {undoToast.map((notice) => (
            <div
              key={notice.key}
              className={`rounded-lg px-3 py-2 text-xs ${UNDO_TONE_CLASS[notice.tone]}`}
            >
              {t(notice.key, notice.values)}
            </div>
          ))}
        </div>
      )}

      {/* Scan results toast */}

    </div>
  );
}
