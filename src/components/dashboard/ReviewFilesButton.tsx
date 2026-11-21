import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ListChecks } from "lucide-react";
import { pendingFilePath, useAppStore } from "../../store/useAppStore";

export default function ReviewFilesButton() {
  const { t, i18n } = useTranslation();
  const pending = useAppStore(state => state.pendingFiles);
  const status = useAppStore(state => state.pendingReadStatus);
  const getPendingFiles = useAppStore(state => state.getPendingFiles);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      if (!document.hidden) await getPendingFiles();
      if (active) timer = setTimeout(refresh, 3000);
    };
    void refresh();
    return () => { active = false; clearTimeout(timer); };
  }, [getPendingFiles]);

  const openReview = async () => {
    if (status !== "ready" || pending.length === 0) return;
    setError("");
    try {
      await invoke("show_review_cmd", { paths: pending.map(pendingFilePath) });
    } catch (reason) {
      setError(String(reason));
    }
  };

  const count = pending.length;
  const countText = count.toLocaleString(i18n.resolvedLanguage);
  const statusText = status === "failed" ? t("popup.pendingFilesLoadError") : t("popup.pendingFilesLoading");
  const label = t("dashboard.reviewFiles");
  return (
    <div>
      <button type="button" onClick={openReview} disabled={status !== "ready" || count === 0}
        aria-label={status === "ready" ? `${label} (${countText})` : `${label}. ${statusText}`}
        className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-border disabled:opacity-60">
        <ListChecks size={15} aria-hidden="true" />
        {label}
        <span className="rounded bg-surface-dark px-1.5 tabular-nums">
          {status === "ready" ? countText : status === "loading" ? "…" : "?"}
        </span>
      </button>
      {status === "failed" && <p role="status" className="text-xs">{statusText}</p>}
      {error && <p role="alert" className="text-xs text-red-600">{t("confirm.failed", { error })}</p>}
    </div>
  );
}
