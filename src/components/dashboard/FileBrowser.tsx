import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Copy, ExternalLink, Search } from "lucide-react";
import { formatBytes, formatTimestamp } from "../../utils/format";
import { fileName, parentDir } from "../../utils/paths";
import { categoryColor } from "../../utils/dashboard";
import type { InventoryFile } from "../../store/useDashboardStore";

interface FileBrowserProps {
  initialLargest: InventoryFile[];
  initialRecent: InventoryFile[];
  category: string | null;
  root: string | null;
}

type Mode = "size" | "mtime";

export default function FileBrowser({
  initialLargest,
  initialRecent,
  category,
  root,
}: FileBrowserProps) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<Mode>("size");
  const [query, setQuery] = useState("");
  const [files, setFiles] = useState<InventoryFile[]>(initialLargest);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const load = async (sort: Mode, q: string, cat: string | null, r: string | null) => {
      const hasFilter = Boolean(q || cat || r);
      if (!hasFilter) {
        setFiles(sort === "size" ? initialLargest : initialRecent);
        return;
      }
      setLoading(true);
      try {
        const rows = await invoke<InventoryFile[]>("get_inventory_files_cmd", {
          sort,
          category: cat ?? "",
          root: r ?? "",
          query: q.trim(),
          limit: 80,
        });
        setFiles(rows);
      } catch {
        // keep last list
      } finally {
        setLoading(false);
      }
    };
    if (debounce.current) clearTimeout(debounce.current);
    debounce.current = setTimeout(() => {
      void load(mode, query, category, root);
    }, query ? 200 : 0);
    return () => {
      if (debounce.current) clearTimeout(debounce.current);
    };
  }, [mode, query, category, root, initialLargest, initialRecent]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "/" && document.activeElement !== searchRef.current) {
        const tag = (document.activeElement as HTMLElement | null)?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
        e.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const handleOpen = async (path: string) => {
    try {
      await invoke("open_folder_cmd", { path: parentDir(path) });
    } catch {
      // ignore
    }
  };

  const handleCopy = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      setCopied(path);
      setTimeout(() => setCopied(null), 1400);
    } catch {
      // ignore
    }
  };

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h3 className="text-sm font-semibold text-text mr-auto">
          {mode === "size" ? t("dashboard.largestFiles") : t("dashboard.recentFiles")}
        </h3>
        <div className="flex rounded-md border border-border overflow-hidden text-xs">
          <button
            type="button"
            onClick={() => setMode("size")}
            className={`px-2.5 py-1 ${mode === "size" ? "bg-primary text-white" : "text-text-muted hover:text-text"}`}
          >
            {t("dashboard.largestFiles")}
          </button>
          <button
            type="button"
            onClick={() => setMode("mtime")}
            className={`px-2.5 py-1 ${mode === "mtime" ? "bg-primary text-white" : "text-text-muted hover:text-text"}`}
          >
            {t("dashboard.recentFiles")}
          </button>
        </div>
      </div>

      <div className="relative mb-3">
        <Search size={13} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
        <input
          ref={searchRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("dashboard.searchFiles")}
          className="w-full rounded-md border border-border bg-surface pl-8 pr-3 py-1.5 text-sm text-text placeholder:text-text-muted focus:outline-none focus:border-primary"
        />
      </div>

      {(category || root) && (
        <div className="flex flex-wrap gap-2 mb-3 text-[11px]">
          {category && (
            <span className="rounded-full border border-border px-2 py-0.5 text-text">
              {t("dashboard.filterBy", { name: category })}
            </span>
          )}
          {root && (
            <span className="rounded-full border border-border px-2 py-0.5 text-text truncate max-w-[240px]">
              {t("dashboard.filterBy", { name: fileName(root) || root })}
            </span>
          )}
        </div>
      )}

      {loading && (
        <div className="text-xs text-text-muted mb-2">{t("app.loading")}</div>
      )}

      {files.length === 0 ? (
        <div className="flex items-center justify-center h-28 text-xs text-text-muted">
          {t("dashboard.noFiles")}
        </div>
      ) : (
        <div className="space-y-0.5 max-h-[28rem] overflow-auto">
          {files.map((file) => (
            <div
              key={file.path}
              className="flex items-center justify-between rounded-md px-2.5 py-1.5 hover:bg-surface transition-colors group"
            >
              <div className="flex items-center gap-2 min-w-0 flex-1">
                <span
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ backgroundColor: categoryColor(file.category) }}
                  title={file.category}
                />
                <div className="min-w-0">
                  <div className="text-sm text-text truncate">{fileName(file.path)}</div>
                  <div className="text-[10px] text-text-muted truncate">{file.path}</div>
                </div>
              </div>
              <div className="flex items-center gap-2 shrink-0 ml-3">
                <span className="text-xs text-text-muted tabular-nums">{formatBytes(file.size)}</span>
                <span className="text-[10px] text-text-muted hidden sm:inline w-24 text-right">
                  {formatTimestamp(file.mtime)}
                </span>
                <button
                  type="button"
                  onClick={() => handleCopy(file.path)}
                  className="p-1 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 focus:opacity-100 transition-all"
                  title={copied === file.path ? t("dashboard.copied") : t("dashboard.copyPath")}
                >
                  <Copy size={12} />
                </button>
                <button
                  type="button"
                  onClick={() => handleOpen(file.path)}
                  className="p-1 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 focus:opacity-100 transition-all"
                  title={t("dashboard.openFolder")}
                >
                  <ExternalLink size={12} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
