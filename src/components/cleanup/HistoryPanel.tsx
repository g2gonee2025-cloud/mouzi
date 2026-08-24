import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import i18n from "../../i18n";
import { useCleanupStore } from "../../store/useCleanupStore";
import { Trash2, FolderX, ExternalLink } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

export default function HistoryPanel() {
  const { t } = useTranslation();
  const { history, loadHistory } = useCleanupStore();

  useEffect(() => {
    loadHistory();
  }, [loadHistory]);

  const handleOpen = async (path: string) => {
    try {
      await invoke("open_folder_cmd", { path });
    } catch {
      // ignore
    }
  };

  const logs = history ?? [];

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-text">
          {t("cleanup.historyTitle")}
        </h3>
        <button
          onClick={loadHistory}
          className="text-xs text-primary hover:text-primary-hover transition-colors"
        >
          {t("cleanup.refresh")}
        </button>
      </div>

      {logs.length === 0 && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          <p>{t("cleanup.historyEmpty")}</p>
          <p className="mt-1">{t("cleanup.historyEmptyDirs")}</p>
        </div>
      )}

      {logs.length > 0 && (
        <div className="rounded-lg border border-border bg-surface-dark overflow-hidden">
          <div className="max-h-80 overflow-auto">
            {logs.map((log) => {
              const fname =
                log.path.split("\\").pop()?.split("/").pop() || log.path;
              const isDir = log.action === "remove_empty_dir";
              const Icon = isDir ? FolderX : Trash2;
              const statusColor =
                log.status === "ok"
                  ? "text-green-500"
                  : log.status === "failed"
                    ? "text-red-500"
                    : "text-yellow-500";
              return (
                <div
                  key={log.id}
                  className="flex items-center gap-2 px-3 py-2 border-b border-border last:border-0 group"
                >
                  <Icon
                    size={14}
                    className={`${statusColor} shrink-0`}
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-xs text-text truncate">{fname}</div>
                    <div className="text-[10px] text-text-muted truncate">
                      {log.path}
                    </div>
                  </div>
                  <span className="text-[10px] text-text-muted shrink-0">
                    {log.timestamp
                      ? new Date(log.timestamp).toLocaleDateString(i18n.language, {
                          month: "short",
                          day: "numeric",
                          year: "numeric",
                        })
                      : ""}
                  </span>
                  <button
                    onClick={() => handleOpen(log.path)}
                    className="p-1 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 transition-all"
                    title={t("cleanup.openFolder")}
                  >
                    <ExternalLink size={12} />
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}