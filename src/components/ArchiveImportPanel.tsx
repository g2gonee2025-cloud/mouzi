import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { ExternalLink, Upload } from "lucide-react";
import { observeArchiveImportState, useAppStore } from "../store/useAppStore";
import { formatBytes } from "../utils/format";

export default function ArchiveImportPanel() {
  const { t } = useTranslation();
  const snapshot = useAppStore((state) => state.archiveImportState);
  const statusError = useAppStore((state) => state.archiveStatusError);
  const importError = useAppStore((state) => state.archiveImportError);
  const cancelError = useAppStore((state) => state.archiveCancelError);
  const pickerOpen = useAppStore((state) => state.archivePickerOpen);
  const pending = useAppStore((state) => state.archiveImportPending);
  const cancelingJob = useAppStore((state) => state.archiveCancelingJob);
  const pickArchiveImport = useAppStore((state) => state.pickArchiveImport);
  const cancelArchiveImport = useAppStore((state) => state.cancelArchiveImport);
  const [connection, setConnection] = useState<{ ready: boolean; error: string | null }>({ ready: false, error: null });
  const [attempt, setAttempt] = useState(0);
  const [folderError, setFolderError] = useState<{ path: string; message: string } | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    void observeArchiveImportState(controller.signal)
      .then(() => {
        if (!controller.signal.aborted) setConnection({ ready: true, error: null });
      })
      .catch((error) => {
        if (!controller.signal.aborted) setConnection({ ready: false, error: String(error) });
      });
    return () => controller.abort();
  }, [attempt]);

  const running = snapshot?.status === "running";
  const canCancel = running && snapshot.jobId !== null;
  const cancelPending = canCancel && (snapshot.cancelRequested || cancelingJob === snapshot.jobId);
  const unavailable = connection.error ?? statusError;
  const previous = snapshot?.status === "failed" ? snapshot.previousReceipt : null;
  const receipt = snapshot?.result ?? previous?.receipt;
  const failure = snapshot?.status === "failed" ? snapshot.error : null;

  const chooseArchive = () => pickArchiveImport(() => open({
    filters: [{ name: "Archives", extensions: ["zip", "tgz", "gz"] }],
    multiple: false,
  }));

  const openImportFolder = async (path: string) => {
    setFolderError(null);
    try {
      await invoke("open_folder_cmd", { path, directoryOnly: true });
    } catch (error) {
      setFolderError({ path, message: String(error) });
    }
  };

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-4 space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold">{t("settings.archive.title")}</h3>
          <p className="mt-1 text-xs text-text-muted">{t("settings.archive.description")}</p>
        </div>
        <button
          type="button"
          onClick={() => void chooseArchive()}
          disabled={!connection.ready || !!unavailable || !snapshot || running || pending || pickerOpen}
          className="flex shrink-0 items-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm hover:bg-surface transition-colors disabled:cursor-not-allowed disabled:opacity-60"
        >
          <Upload size={14} aria-hidden="true" />
          {t(running || pending ? "settings.archive.importingShort" : "settings.archive.import")}
        </button>
      </div>
      {canCancel && (
        <div className="flex flex-wrap items-center gap-3">
          <button
            type="button"
            disabled={cancelPending}
            className="rounded-md border border-border px-3 py-2 text-sm hover:bg-surface disabled:opacity-60"
            onClick={() => void cancelArchiveImport()}
          >
            {t(cancelPending ? "settings.archive.cancelling" : "common.cancel")}
          </button>
          <p role="status" className="text-xs text-text-muted">
            {snapshot.phase === "sorting" && <span className="block">{t("settings.archive.sorting")}</span>}
            {t("settings.archive.progress", { files: snapshot.files, bytes: formatBytes(snapshot.bytes) })}
          </p>
        </div>
      )}
      {unavailable && (
        <div className="space-y-1 text-xs text-red-700 dark:text-red-400">
          <p role="alert">{t("settings.archive.statusUnavailable", { error: unavailable })}</p>
          <button type="button" className="underline" onClick={() => {
            setConnection({ ready: false, error: null });
            setAttempt((current) => current + 1);
          }}>{t("common.retry")}</button>
        </div>
      )}
      {cancelError && running && <p role="alert" className="text-xs text-red-700 dark:text-red-400">{t("confirm.failed", { error: cancelError })}</p>}
      {importError && importError !== failure && <p role="alert" className="text-xs text-red-700 dark:text-red-400">{t("settings.archive.importError", { error: importError })}</p>}
      {failure && (
        <p role="alert" className="text-xs text-red-700 dark:text-red-400">
          <strong className="block break-all">{snapshot?.sourceName}</strong>
          {t("settings.archive.importError", { error: failure })}
        </p>
      )}
      {receipt && (
        <div className={`text-xs px-3 py-2 rounded-md ${receipt.issue === null
          ? "bg-green-50 text-green-700 border border-green-200"
          : "bg-amber-50 text-amber-800 border border-amber-200"}`}>
          <div role="status">
            <p className="font-medium break-all">{previous
              ? t("settings.archive.previousImport", { name: previous.sourceName })
              : snapshot?.sourceName}</p>
            <p>{receipt.extractedCount === null
              ? t("settings.archive.organizedCount", { count: receipt.sortedCount })
              : t("settings.archive.importSuccess", { extracted: receipt.extractedCount, count: receipt.sortedCount })}</p>
            {receipt.issue && <p className="break-words">{t("settings.archive.importIncomplete", { error: receipt.issue })}</p>}
          </div>
          <button
            type="button"
            onClick={() => void openImportFolder(receipt.stagingPath)}
            className="mt-2 inline-flex items-center gap-1 text-xs font-medium underline"
          >
            <ExternalLink size={12} aria-hidden="true" />
            {t("settings.archive.openImportFolder")}
          </button>
          {folderError?.path === receipt.stagingPath && (
            <p role="alert" className="mt-1">{t("confirm.failed", { error: folderError.message })}</p>
          )}
        </div>
      )}
    </div>
  );
}
