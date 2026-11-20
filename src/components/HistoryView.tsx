import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { type OrganizationLog, type UndoResult, useAppStore } from "../store/useAppStore";
import { parentDir } from "../utils/paths";
import { canRestoreLog as canRestore, UNDO_TONE_CLASS, undoNotice } from "../utils/undo";
import { buttonClass, fieldClass, primaryClass, errorText } from "./BetaControls";
import ConfirmDialog, { type ConfirmRequest } from "./cleanup/ConfirmDialog";

const emptyFilters = { query: "", rule: "", extension: "", from: "", to: "", run_id: "" };

function endOfLocalMinute(value: string): string {
  const lastSecond = new Date(value);
  lastSecond.setUTCSeconds(59, 0);
  // History's inclusive bound can retain nanoseconds beyond Date's precision.
  return lastSecond.toISOString().replace(".000Z", ".999999999Z");
}

type HistoryRead =
  | { status: "loading" }
  | { status: "failed"; error: string }
  | { status: "ready"; request: number; rows: OrganizationLog[] };

interface SelectedUndoResult extends UndoResult {
  id: number;
}

export default function HistoryView() {
  const { t } = useTranslation();
  const [filters, setFilters] = useState(emptyFilters);
  const [read, setRead] = useState<HistoryRead>({ status: "loading" });
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [error, setError] = useState("");
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [limit, setLimit] = useState(100);
  const [results, setResults] = useState<Array<SelectedUndoResult & { source: string }>>([]);
  const appliedFilters = useRef(emptyFilters);
  const generation = useRef(0);
  const restoring = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const busy = read.status === "loading";
  const ready = read.status === "ready";
  const logs = ready ? read.rows : [];
  const ownsRows = () => ready && read.request === generation.current;

  const load = useCallback(async (resetSelection = false) => {
    const request = ++generation.current;
    const value = appliedFilters.current;
    setRead({ status: "loading" });
    // A new read invalidates consent from the old rows unless its restore was already sent.
    if (!restoring.current) setConfirm(null);
    if (resetSelection) {
      setSelected(new Set());
      setLimit(100);
    }
    try {
      const rows = await invoke<OrganizationLog[]>("history_cmd", { filter: {
        ...value,
        from: value.from ? new Date(value.from).toISOString() : "",
        to: value.to ? endOfLocalMinute(value.to) : "",
      } });
      if (request === generation.current) {
        setRead({ status: "ready", request, rows });
        setSelected((previous) => new Set(rows.filter((row) => canRestore(row) && previous.has(row.id!)).map((row) => row.id!)));
      }
    } catch (cause) {
      if (request === generation.current) setRead({ status: "failed", error: errorText(cause) });
    }
  }, []);
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    const refresh = () => { if (!disposed) void load(); };
    void listen("mutation-completed", refresh).then((unlisten) => {
      if (disposed) unlisten();
      else { stop = unlisten; refresh(); }
    }).catch((cause) => {
      console.error("History listener failed", cause);
      refresh();
    });
    return () => { disposed = true; ++generation.current; stop?.(); };
  }, [load]);

  function applyFilters(value: typeof emptyFilters) {
    if (value.from && value.to && value.from > value.to) {
      setError(t("beta.validation.date"));
      return;
    }
    appliedFilters.current = value;
    setError("");
    void load(true);
  }

  function toggle(id: number) {
    if (!ownsRows()) return;
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function askRestore(opener: HTMLButtonElement) {
    if (!ownsRows() || !selectedLogs.length) return;
    const rows = selectedLogs;
    const reviewedRequest = generation.current;
    setConfirm({
      title: t("beta.confirmUndo", { count: rows.length }),
      count: rows.length,
      notice: "move",
      note: t("beta.undoHelp"),
      preview: rows.map((log) => log.source_path),
      opener,
      focusFallback: () => heading.current,
      onConfirm: async () => {
        if (restoring.current || reviewedRequest !== generation.current) return;
        restoring.current = true;
        try {
          const sources = new Map(rows.map((log) => [log.id!, log.source_path]));
          const outcomes = await invoke<SelectedUndoResult[]>("undo_selected_cmd", { ids: [...sources.keys()] });
          setResults(outcomes.map((outcome) => ({ ...outcome, source: sources.get(outcome.id) ?? "" })));
          await Promise.all([load(), useAppStore.getState().refreshActionViews()]);
        } finally {
          restoring.current = false;
        }
      },
    });
  }

  const eligibleLogs = logs.filter(canRestore);
  const selectedLogs = eligibleLogs.filter((log) => selected.has(log.id!));
  const groups = new Map<string, OrganizationLog[]>();
  for (const log of logs.slice(0, limit)) {
    const key = log.run_id || "legacy";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(log);
  }

  return <section className="space-y-4" aria-busy={busy}>
    <h3 ref={heading} tabIndex={-1} className="text-sm font-medium">{t("beta.filter")}</h3>
    <form className="grid grid-cols-1 gap-3 sm:grid-cols-2" onSubmit={(event) => { event.preventDefault(); applyFilters(filters); }}>
      {(["query", "rule", "extension", "from", "to"] as const).map((key) =>
        <label key={key} className="text-sm">{t("beta." + (key === "query" ? "search" : key))}
          <input className={fieldClass} type={key === "from" || key === "to" ? "datetime-local" : "text"}
            value={filters[key]} onChange={(event) => setFilters({ ...filters, [key]: event.target.value })} />
        </label>)}
      <div className="flex items-end gap-2">
        <button className={buttonClass} disabled={busy}>{t("beta.filter")}</button>
        <button type="button" className={buttonClass} disabled={busy} onClick={() => { setFilters(emptyFilters); applyFilters(emptyFilters); }}>
          {t("beta.reset")}
        </button>
      </div>
    </form>
    {error && <p role="alert" className="text-red-700 dark:text-red-300">{error}</p>}
    {busy && <p role="status" className="text-text-muted">{t("common.loading")}</p>}
    {read.status === "failed" && <div className="space-y-2">
      <p role="alert" className="text-red-700 dark:text-red-300">{read.error}</p>
      <button type="button" className={buttonClass} onClick={() => void load()}>{t("common.retry")}</button>
    </div>}
    {results.length > 0 && <div role="status" className="space-y-2 rounded-lg border border-border p-3">
      <h3 className="font-medium">{t("beta.results")}</h3>
      {results.map((result) => {
        const notices = undoNotice({ ...result, message: result.message && errorText(result.message) });
        return <div key={result.id} className="space-y-1 break-all text-sm">
          <p className="font-medium">{result.source}</p>
          {notices.map((notice) => <p key={notice.key} className={`rounded-md px-2 py-1 ${UNDO_TONE_CLASS[notice.tone]}`}>
            {t(notice.key, notice.values)}
          </p>)}
        </div>;
      })}
    </div>}
    <div className="flex flex-wrap items-center gap-2">
      <button className={buttonClass} disabled={!eligibleLogs.length}
        onClick={() => { if (ownsRows()) setSelected(new Set(eligibleLogs.map((log) => log.id!))); }}>{t("beta.selectAll")}</button>
      <button className={buttonClass} disabled={!ready} onClick={() => setSelected(new Set())}>{t("beta.clearSelection")}</button>
      <span>{t("beta.selected", { count: selectedLogs.length })}</span>
      <button className={primaryClass} disabled={!selectedLogs.length}
        onClick={(event) => askRestore(event.currentTarget)}>{t("beta.undoSelection")}</button>
    </div>
    {ready && !logs.length && <p>{t("beta.noHistory")}</p>}
    {[...groups].map(([key, entries]) => <section key={key} className="space-y-3 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 className="text-sm font-medium">
          {key === "legacy" ? t("beta.legacy") : t("beta.run") + " · " + t("beta." + entries[0].trigger, { defaultValue: entries[0].trigger })}
          <span className="block text-xs text-text-muted">{new Date(entries[0].timestamp).toLocaleString()}</span>
        </h4>
        <button className={buttonClass} onClick={() => {
          if (ownsRows()) setSelected((previous) => new Set([
            ...previous,
            ...eligibleLogs.filter((log) => (log.run_id || "legacy") === key).map((log) => log.id!),
          ]));
        }}>{t("beta.selectRun")}</button>
      </div>
      {entries.map((log) => <div key={log.id} className="flex flex-wrap justify-between gap-2 border-t border-border pt-2 text-sm">
        <label className="flex min-w-0 gap-3">
          <input type="checkbox" className="mt-1 shrink-0" checked={selected.has(log.id!)}
            disabled={!canRestore(log)} onChange={() => toggle(log.id!)} />
          <span className="min-w-0 break-all">
            <strong>{log.file_name}</strong> · {log.file_type} · {t("beta." + log.action, { defaultValue: log.action })}
            <span className="block">{log.source_path}</span>
            {log.destination_path && <span className="block">→ {log.destination_path}</span>}
            <span className="block text-xs text-text-muted">
              {new Date(log.timestamp).toLocaleString()} · {log.file_size} B
              {log.undone ? " · " + t("beta.undone")
                : log.action === "delete" ? " · " + t("beta.trashHelp")
                : !log.undo_verified ? " · " + t("beta.errors.history.unverified") : ""}
            </span>
          </span>
        </label>
        {log.destination_path && <button type="button" className="shrink-0 self-start text-xs text-primary"
          onClick={() => void invoke("open_folder_cmd", { path: parentDir(log.destination_path!) })
            .catch((cause) => setError(errorText(cause)))}>{t("cleanup.openFolder")}</button>}
      </div>)}
    </section>)}
    {logs.length > limit && <button className={buttonClass} onClick={() => setLimit(limit + 100)}>{t("beta.more")}</button>}
    {confirm && <ConfirmDialog {...confirm} onClose={() => setConfirm((current) => current === confirm ? null : current)} />}
  </section>;
}
