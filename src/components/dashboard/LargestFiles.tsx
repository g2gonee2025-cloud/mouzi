import { useTranslation } from "react-i18next";
import { formatBytes, formatTimestamp } from "../../utils/format";
import { ExternalLink } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

interface LargestFilesProps {
  data: Array<{ path: string; size: number; mtime: number }>;
}

export default function LargestFiles({ data }: LargestFilesProps) {
  const { t } = useTranslation();

  if (data.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-surface-dark p-4">
        <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.largestFiles")}</h3>
        <div className="flex items-center justify-center h-32 text-xs text-text-muted">
          {t("dashboard.noData")}
        </div>
      </div>
    );
  }

  const handleOpen = async (path: string) => {
    try {
      await invoke("open_folder_cmd", { path });
    } catch {
      // ignore
    }
  };

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-4">
      <h3 className="text-sm font-semibold text-text mb-3">{t("dashboard.largestFiles")}</h3>
      <div className="space-y-1">
        {data.map((file) => (
          <div
            key={file.path}
            className="flex items-center justify-between rounded-md px-3 py-2 hover:bg-surface transition-colors group"
          >
            <div className="flex-1 min-w-0">
              <div className="text-sm text-text truncate">
                {file.path.split("\\").pop()?.split("/").pop() || file.path}
              </div>
              <div className="text-[10px] text-text-muted truncate">{file.path}</div>
            </div>
            <div className="flex items-center gap-3 shrink-0 ml-3">
              <span className="text-xs text-text-muted">{formatBytes(file.size)}</span>
              <span className="text-[10px] text-text-muted hidden sm:inline">
                {formatTimestamp(file.mtime)}
              </span>
              <button
                onClick={() => handleOpen(file.path)}
                className="p-1 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 transition-all"
                title={t("dashboard.openFolder")}
              >
                <ExternalLink size={12} />
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}