import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Copy, ExternalLink, Search, X } from "lucide-react";
import { formatBytes, formatTimestamp } from "../../utils/format";
import { fileName, parentDir } from "../../utils/paths";
import {
  categoryClass, categoryLabel, createInventoryBrowserLoader, inventoryBrowserScope,
  type InventoryBrowserView,
} from "../../utils/dashboard";
import type { InventoryFile } from "../../store/useDashboardStore";

interface FileBrowserProps {
  initialLargest: InventoryFile[];
  initialRecent: InventoryFile[];
  category: string | null;
  root: string | null;
  rootScope: string;
  totalFiles: number;
  revision: number;
  onClearCategory: () => void;
  onClearRoot: () => void;
}

type Mode = "size" | "mtime";

export default function FileBrowser({
  initialLargest,
  initialRecent,
  category,
  root,
  rootScope,
  totalFiles,
  revision,
  onClearCategory,
  onClearRoot,
}: FileBrowserProps) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<Mode>("size");
  const [query, setQuery] = useState("");
  const [view, setView] = useState<InventoryBrowserView>();
  const [copied, setCopied] = useState<string | null>(null);
  const searchId = useId();
  const searchRef = useRef<HTMLInputElement>(null);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mounted = useRef(true);
  const [loader] = useState(() => createInventoryBrowserLoader(
    (query) => invoke<InventoryFile[]>("get_inventory_files_cmd", { ...query }),
    setView,
  ));
  const search = query.trim();
  const filtered = Boolean(search || category || root);
  const scope = inventoryBrowserScope({ sort: mode, query: search, category, root, rootScope });
  const activeView = view?.scope === scope ? view : undefined;
  const files = filtered ? activeView?.rows ?? [] : mode === "size" ? initialLargest : initialRecent;
  const loading = filtered && (activeView?.loading ?? true);
  const error = filtered ? activeView?.error : null;

  useEffect(() => {
    void loader.load({ sort: mode, query: search, category, root, rootScope, initialLargest, initialRecent }, 200);
    return loader.cancel;
  }, [mode, search, category, root, rootScope, initialLargest, initialRecent, revision, loader]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (copyTimer.current) clearTimeout(copyTimer.current);
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.defaultPrevented || e.isComposing || e.ctrlKey || e.metaKey || e.altKey) return;
      const active = document.activeElement as HTMLElement | null;
      if (active?.matches("input, textarea, select") || active?.isContentEditable) return;
      e.preventDefault();
      searchRef.current?.focus();
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
      if (!mounted.current) return;
      setCopied(path);
      if (copyTimer.current) clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => setCopied(null), 1400);
    } catch {
      // ignore
    }
  };

  const isEmpty = !loading && !error && files.length === 0;
  const shown = files.length.toLocaleString();
  const status = loading
    ? t(files.length ? "dashboard.refreshingFiles" : "dashboard.loadingFiles")
    : isEmpty
      ? t(totalFiles === 0 ? "dashboard.noIndexedFiles" : "dashboard.noFiles")
      : filtered && !error
        ? t(activeView?.limited ? "dashboard.limitedFiles" : "dashboard.filesShown", { shown })
        : "";

  return (
    <div className="rounded-xl border border-border bg-surface-dark p-4">
      <div className="mb-3" role="search">
        <label htmlFor={searchId} className="block text-sm font-semibold mb-1.5">
          {t("dashboard.searchFiles")} <kbd aria-hidden="true" className="text-text-muted text-xs">/</kbd>
        </label>
        <div className="relative">
          <Search size={13} aria-hidden="true" className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
          <input
            ref={searchRef}
            id={searchId}
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-keyshortcuts="/"
            className="w-full rounded-md border border-border bg-surface pl-8 pr-3 py-1.5 text-sm text-text focus:outline-none focus:border-primary"
          />
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h3 className="text-sm font-semibold text-text mr-auto">
          {mode === "size" ? t("dashboard.largestFiles") : t("dashboard.recentFiles")}
        </h3>
        <div className="flex rounded-md border border-border overflow-hidden text-xs">
          <button
            type="button"
            onClick={() => setMode("size")}
            aria-pressed={mode === "size"}
            className={`px-2.5 py-1 ${mode === "size" ? "bg-primary text-on-primary" : "text-text-muted hover:text-text"}`}
          >
            {t("dashboard.largestFiles")}
          </button>
          <button
            type="button"
            onClick={() => setMode("mtime")}
            aria-pressed={mode === "mtime"}
            className={`px-2.5 py-1 ${mode === "mtime" ? "bg-primary text-on-primary" : "text-text-muted hover:text-text"}`}
          >
            {t("dashboard.recentFiles")}
          </button>
        </div>
      </div>

      {(category || root) && (
        <div className="flex flex-wrap gap-2 mb-3 text-[11px]">
          {category && (
            <button
              type="button"
              onClick={() => {
                onClearCategory();
                searchRef.current?.focus();
              }}
              aria-label={t("dashboard.clearFilter", { name: categoryLabel(category, t) })}
              className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-text hover:bg-border"
            >
              {categoryLabel(category, t)} <X size={12} aria-hidden="true" />
            </button>
          )}
          {root && (
            <button
              type="button"
              onClick={() => {
                onClearRoot();
                searchRef.current?.focus();
              }}
              aria-label={t("dashboard.clearFilter", { name: root })}
              title={root}
              className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-text max-w-[240px] hover:bg-border"
            >
              <span className="truncate">{fileName(root) || root}</span> <X size={12} aria-hidden="true" />
            </button>
          )}
        </div>
      )}

      <div
        role="status"
        className={isEmpty ? "flex items-center justify-center h-28 text-xs text-text-muted" : "text-xs text-text-muted"}
      >
        {status && <p className={isEmpty ? undefined : "mb-2"}>{status}</p>}
      </div>
      {error && <div className="text-xs text-red-700 dark:text-red-400 mb-2" role="alert">{error}</div>}
      {files.length > 0 && (
        <div className="space-y-0.5 max-h-[18rem] overflow-auto">
          {files.map((file) => (
            <div
              key={file.path}
              className="flex items-center justify-between rounded-md px-2.5 py-1.5 hover:bg-surface transition-colors group"
            >
              <div className="flex items-center gap-2 min-w-0 flex-1">
                <span
                  className={`w-1.5 h-1.5 rounded-full shrink-0 ${categoryClass(file.category)}`}
                  title={categoryLabel(file.category, t)}
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
                  aria-label={
                    copied === file.path ? t("dashboard.copied") : t("dashboard.copyPath")
                  }
                  className="inline-flex items-center justify-center w-6 h-6 shrink-0 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100 focus-visible:outline-2 focus-visible:outline-primary focus-visible:outline-offset-1 transition-opacity"
                  title={copied === file.path ? t("dashboard.copied") : t("dashboard.copyPath")}
                >
                  <Copy size={12} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  onClick={() => handleOpen(file.path)}
                  aria-label={t("dashboard.openFolder")}
                  className="inline-flex items-center justify-center w-6 h-6 shrink-0 rounded text-text-muted hover:text-text hover:bg-border opacity-0 group-hover:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100 focus-visible:outline-2 focus-visible:outline-primary focus-visible:outline-offset-1 transition-opacity"
                  title={t("dashboard.openFolder")}
                >
                  <ExternalLink size={12} aria-hidden="true" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
