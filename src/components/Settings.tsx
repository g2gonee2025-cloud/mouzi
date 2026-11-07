import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAppStore, Rule, ScheduleSettings, WatchedFolder, type PreferencesPatch } from "../store/useAppStore";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatBytes } from "../utils/format";
import { open } from "@tauri-apps/plugin-dialog";
import About from "./About";
import ActionRefreshWarnings from "./ActionRefreshWarnings";
import HistoryView from "./HistoryView";
import ReviewQueue from "./ReviewQueue";
import { RuleConditions, errorText } from "./BetaControls";
import { nextDestination } from "../utils/rulePath";
import ConfirmDialog, { type ConfirmRequest } from "./cleanup/ConfirmDialog";
import {
  UNDO_TONE_CLASS,
  undoAllNotices,
  type UndoNotice,
} from "../utils/undo";
import {
  Folder,
  FolderOpen,
  List,
  History,
  Inbox,
  Globe,
  Plus,
  Trash2,
  Save,
  X,
  ChevronLeft,
  RotateCcw,
  Download,
  Upload,
  Info,
  ExternalLink,
} from "lucide-react";


type Tab = "folders" | "rules" | "review" | "history" | "ignore" | "general" | "about";
type GraceUnit = "seconds" | "minutes" | "hours";
type ArchiveImportResult = {
  extractedCount: number;
  sortedCount: number;
  stagingPath: string;
};

const GRACE_STEPS = [
  0, 30, 60, 300, 900, 1800, 3600, 7200, 21600, 43200, 86400, 172800, 604800,
];
const MAX_GRACE_SECONDS = 604800; // 7 days

function formatDuration(seconds: number): string {
  if (seconds <= 0) return "0s";
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = seconds % 60;
  const parts: string[] = [];
  if (hours > 0) parts.push(`${hours}h`);
  if (minutes > 0) parts.push(`${minutes}m`);
  if (secs > 0) parts.push(`${secs}s`);
  return parts.join(" ") || "0s";
}

function secondsToUnit(seconds: number): { value: number; unit: GraceUnit } {
  if (seconds % 3600 === 0 && seconds >= 3600) {
    return { value: seconds / 3600, unit: "hours" };
  }
  if (seconds % 60 === 0 && seconds >= 60) {
    return { value: seconds / 60, unit: "minutes" };
  }
  return { value: seconds, unit: "seconds" };
}

function unitToSeconds(value: number, unit: GraceUnit): number {
  switch (unit) {
    case "hours":
      return value * 3600;
    case "minutes":
      return value * 60;
    default:
      return value;
  }
}

function nearestGraceStep(seconds: number): number {
  return GRACE_STEPS.reduce((prev, curr) =>
    Math.abs(curr - seconds) < Math.abs(prev - seconds) ? curr : prev
  );
}

function defaultSchedule(): ScheduleSettings {
  return {
    schedule_enabled: false,
    schedule_times_per_day: 1,
    schedule_time_1: "08:00",
    schedule_time_2: null,
    schedule_time_3: null,
    schedule_time_4: null,
  };
}

type SchedulerRun = {
  id: number;
  status: "claimed" | "running" | "completed" | "failed" | "interrupted";
  claimed_at: string;
  organized: number;
  errors: number;
  deferred: number;
  uncertain: number;
  message: string | null;
};

function SchedulerRunStatus() {
  const { t } = useTranslation();
  const [runs, setRuns] = useState<SchedulerRun[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let request = 0;
    let unlisten: (() => void) | undefined;
    const refresh = async (resultSaved = true) => {
      const current = ++request;
      try {
        const runs = await invoke<SchedulerRun[]>("get_recent_scheduler_runs_cmd");
        if (!disposed && current === request) {
          setRuns(runs);
          setError(resultSaved ? null : "The latest result could not be saved.");
        }
      } catch (cause) {
        if (!disposed && current === request) setError(String(cause));
      }
    };
    listen<{ result_saved: boolean }>("scheduled-clean-done", ({ payload }) => {
      if (!disposed) void refresh(payload.result_saved);
    }).then((stop) => {
      if (disposed) stop();
      else {
        unlisten = stop;
        if (!request) void refresh();
      }
    }).catch(() => { if (!disposed) void refresh(); });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  if (!runs.length && !error) return null;
  const title = t("settings.scheduler.recentRuns", { defaultValue: "Recent scheduled runs" });
  return (
    <div className="text-xs text-text-muted space-y-1">
      <p className="font-medium">{title}</p>
      {runs.length > 0 && (
        <ul aria-label={title} tabIndex={0} className="max-h-64 overflow-y-auto space-y-2">
          {runs.map((run) => (
            <li key={run.id}>
              <p>{new Date(run.claimed_at).toLocaleString()} · {t(`settings.scheduler.status.${run.status}`, { defaultValue: run.status })}</p>
              <p>{t("settings.scheduler.runCounts", {
                defaultValue: "Organized {{organized}} · Errors {{errors}} · Deferred {{deferred}}",
                organized: run.organized, errors: run.errors, deferred: run.deferred,
              })}</p>
              {run.uncertain > 0 && <p>{t("settings.scheduler.pendingRecovery", {
                defaultValue: "Move records needing review: {{count}}", count: run.uncertain,
              })}</p>}
              {run.message && <p className="break-words">{run.message}</p>}
            </li>
          ))}
        </ul>
      )}
      {error && <p role="status" className="break-words">{error}</p>}
    </div>
  );
}

export default function Settings() {
  const { t } = useTranslation();
  const {
    rules,
    folders,
    historyCounts,
    loadHistoryCounts,
    loadRules,
    loadFolders,
    loadLogs,
    addFolder,
    removeFolder,
    updateFolderMode,
    addRule,
    updateRule,
    deleteRule,
    clearLogs,
    undoAll,
    settings,
    saveSettings,
    setAutostart,
    schedule,
    getSchedule,
    updateSchedule,
    exportRules,
    importRules,
  } = useAppStore();

  const [tab, setTab] = useState<Tab>("folders");
  const [reviewPaths, setReviewPaths] = useState<string[] | null>(null);
  const [editingRule, setEditingRule] = useState<Rule | null>(null);
  const [newFolderPath, setNewFolderPath] = useState("");
  const [newOnlyNew, setNewOnlyNew] = useState(false);
  const [explorer, setExplorer] = useState<[boolean, boolean]>([false, false]);
  const [writeError, setWriteError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const savingRef = useRef(false);
  const [scheduleDirty, setScheduleDirty] = useState(false);
  const [aiModel, setAiModel] = useState("");

  const [graceValue, setGraceValue] = useState(300);
  const [graceUnit, setGraceUnit] = useState<GraceUnit>("seconds");
  const [graceError, setGraceError] = useState<string | null>(null);

  const [localSchedule, setLocalSchedule] = useState<ScheduleSettings>(defaultSchedule());
  const [ruleToast, setRuleToast] = useState<{ message: string; type: "success" | "error" } | null>(null);
  const [replaceOnImport, setReplaceOnImport] = useState(false);
  const [archiveToast, setArchiveToast] = useState<{
    message: string;
    type: "success" | "error" | "info";
    stagingPath?: string;
  } | null>(null);
  const [isImportingArchive, setIsImportingArchive] = useState(false);
  const [archiveProgress, setArchiveProgress] = useState<{ bytes: number; files: number } | null>(null);
  useEffect(() => {
    if (!isImportingArchive) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    listen<{ bytes: number; files: number }>("archive-import-progress", (event) => {
      if (!disposed) setArchiveProgress(event.payload);
    }).then((unlisten) => { if (disposed) unlisten(); else stop = unlisten; })
      .catch((error) => setArchiveToast({ type: "error", message: String(error) }));
    return () => { disposed = true; stop?.(); };
  }, [isImportingArchive]);
  const [pending, setPending] = useState<ConfirmRequest | null>(null);
  const sectionHeading = useRef<HTMLHeadingElement>(null);
  const [undoToast, setUndoToast] = useState<UndoNotice[] | null>(null);

  // 8s, not the 3s the export/import toasts use: a `collision` line ends in a
  // filesystem path, and 3s cuts it off before it can be read or copied.
  useEffect(() => {
    if (!undoToast) return;
    const timer = setTimeout(() => setUndoToast(null), 8000);
    return () => clearTimeout(timer);
  }, [undoToast]);

  useEffect(() => {
    Promise.all([loadRules(), loadFolders(), loadLogs(), getSchedule()])
      .catch((e) => setWriteError(String(e)));
  }, [loadRules, loadFolders, loadLogs, getSchedule]);

  useEffect(() => {
    let disposed = false;
    const stops: Array<() => void> = [];
    const openReview = async () => {
      const paths = await invoke<string[] | null>("take_review_request_cmd");
      if (!disposed && paths !== null) {
        setReviewPaths(paths);
        setTab("review");
      }
    };
    const openFolderRequest = async () => {
      const path = await invoke<string | null>("take_add_folder_request_cmd");
      if (!disposed && path) { setNewFolderPath(path); setTab("folders"); }
    };
    const onError = (error: unknown) => { if (!disposed) setWriteError(errorText(error)); };
    for (const [event, consume] of [["open-review", openReview], ["add-folder-request", openFolderRequest]] as const) {
      void listen(event, () => { void consume().catch(onError); }).then((stop) => {
        if (disposed) stop();
        else { stops.push(stop); return consume(); }
      }).catch(onError);
    }
    void invoke<[boolean, boolean]>("explorer_integration_status_cmd")
      .then((status) => { if (!disposed) setExplorer(status); }).catch(onError);
    return () => { disposed = true; stops.forEach((stop) => stop()); };
  }, []);

  useEffect(() => {
    setAiModel(settings?.local_ai_model ?? "");
  }, [settings?.local_ai_model]);

  const runWrite = async (action: () => Promise<unknown>): Promise<boolean> => {
    if (savingRef.current) return false;
    savingRef.current = true;
    setIsSaving(true);
    setWriteError(null);
    try {
      await action();
      return true;
    } catch (e) {
      setWriteError(errorText(e));
      return false;
    } finally {
      savingRef.current = false;
      setIsSaving(false);
    }
  };

  const savePreferences = (patch: PreferencesPatch) => runWrite(() => saveSettings(patch));

  // Sync local grace editor with loaded settings
  useEffect(() => {
    if (settings) {
      const clamped = Math.min(settings.grace_period_seconds, MAX_GRACE_SECONDS);
      const converted = secondsToUnit(clamped);
      setGraceValue(converted.value);
      setGraceUnit(converted.unit);
      setGraceError(null);
    }
  }, [settings?.grace_period_seconds]);

  // Sync local schedule editor with loaded schedule
  useEffect(() => {
    if (schedule && !scheduleDirty) {
      setLocalSchedule(schedule);
    }
  }, [schedule, scheduleDirty]);

  const handleAddFolder = async () => {
    if (!newFolderPath.trim()) return;
    if (await runWrite(async () => {
      const id = await addFolder(newFolderPath.trim(), "manual");
      if (newOnlyNew) {
        await invoke("set_only_new_cmd", { id, enabled: true });
        await loadFolders();
      }
    })) {
      setNewFolderPath("");
      setNewOnlyNew(false);
    }
  };

  const handleSaveRule = async () => {
    if (!editingRule) return;
    const rule = editingRule;
    if (await runWrite(() => rule.id ? updateRule(rule) : addRule(rule))) setEditingRule(null);
  };

  const askRemoveFolder = (folder: WatchedFolder, opener: HTMLButtonElement) => {
    if (folder.id === undefined) return;
    setPending({
      title: t("confirm.op.removeFolder"),
      count: 1,
      notice: "permanent",
      note: t("confirm.note.removeFolder"),
      opener,
      focusFallback: () => sectionHeading.current,
      onConfirm: () => removeFolder(folder.id!),
    });
  };

  const askDeleteRule = (rule: Rule, opener: HTMLButtonElement) => {
    if (rule.id === undefined) return;
    setPending({
      title: t("confirm.op.deleteRule"),
      count: 1,
      notice: "permanent",
      note: t("confirm.note.deleteRule"),
      opener,
      focusFallback: () => sectionHeading.current,
      onConfirm: () => deleteRule(rule.id!),
    });
  };

  const askUndoAll = async (opener: HTMLButtonElement) => {
    let restorable: number;
    try {
      restorable = (await loadHistoryCounts()).undoable;
    } catch (e) {
      setWriteError(String(e));
      return;
    }
    if (restorable === 0) return;
    setPending({
      title: t("confirm.op.undoAll"),
      count: restorable,
      notice: "move",
      note: t("confirm.note.undoAll"),
      opener,
      focusFallback: () => sectionHeading.current,
      // `undoAllNotices` rebuilds the restored count from `results` rather than
      // reading `UndoAllResult.count`, which the backend fills with every
      // non-`failed` row and therefore counts `missing` ones too.
      onConfirm: async () => {
        setUndoToast(undoAllNotices(await undoAll()));
      },
    });
  };

  // `clear_logs_cmd` is an unconditional DELETE FROM action_logs, so this takes
  // the undo trail with it - every Undo button in the app reads from the same
  // table. Undoing history and deleting it are not the same verb.
  const askClearLogs = async (opener: HTMLButtonElement) => {
    let total: number;
    try {
      total = (await loadHistoryCounts()).total;
    } catch (e) {
      setWriteError(String(e));
      return;
    }
    if (total === 0) return;
    setPending({
      title: t("confirm.op.clearHistory"),
      count: total,
      notice: "permanent",
      note: t("confirm.note.clearHistory", { count: total }),
      opener,
      focusFallback: () => sectionHeading.current,
      onConfirm: () => clearLogs(),
    });
  };

  const handleChangeLanguage = async (lang: string) => {
    if (!settings) return;
    await savePreferences({ language: lang });
  };

  const saveGracePeriod = async (seconds: number) => {
    if (await savePreferences({ grace_period_seconds: seconds })) return;
    const committed = useAppStore.getState().settings;
    if (!committed) return;
    const converted = secondsToUnit(Math.min(committed.grace_period_seconds, MAX_GRACE_SECONDS));
    setGraceValue(converted.value);
    setGraceUnit(converted.unit);
  };

  const handleGraceSliderChange = (stepIndex: number) => {
    if (!settings) return;
    const seconds = GRACE_STEPS[stepIndex];
    const converted = secondsToUnit(seconds);
    setGraceValue(converted.value);
    setGraceUnit(converted.unit);
    setGraceError(null);
    void saveGracePeriod(seconds);
  };

  const handleGraceNumberChange = (value: number, unit: GraceUnit) => {
    if (!settings) return;
    const nonnegative = Math.max(0, value);
    const seconds = unitToSeconds(nonnegative, unit);
    if (seconds > MAX_GRACE_SECONDS) {
      setGraceError(t("settings.general.gracePeriodMaxError"));
      setGraceValue(value);
      setGraceUnit(unit);
      return;
    }
    setGraceError(null);
    setGraceValue(nonnegative);
    setGraceUnit(unit);
    void saveGracePeriod(seconds);
  };

  const handleScheduleChange = (patch: Partial<ScheduleSettings>) => {
    setScheduleDirty(true);
    setLocalSchedule((prev) => {
      const next = { ...prev, ...patch };
      const times = Math.max(1, Math.min(4, next.schedule_times_per_day || 1));
      // Ensure required time slots have defaults when increasing count
      if (times >= 1 && !next.schedule_time_1) next.schedule_time_1 = "08:00";
      if (times >= 2 && !next.schedule_time_2) next.schedule_time_2 = "14:00";
      if (times >= 3 && !next.schedule_time_3) next.schedule_time_3 = "20:00";
      if (times >= 4 && !next.schedule_time_4) next.schedule_time_4 = "23:00";
      return { ...next, schedule_times_per_day: times };
    });
  };

  const handleSaveSchedule = async () => {
    if (await runWrite(() => updateSchedule(localSchedule))) setScheduleDirty(false);
  };

  const handleExportRules = async () => {
    try {
      if (!(await exportRules())) return;
      setRuleToast({ message: t("settings.rules.exportSuccess"), type: "success" });
    } catch (e) {
      console.error("Export rules failed:", e);
      setRuleToast({ message: `${t("settings.rules.exportError")}. ${String(e)}`, type: "error" });
    }
    setTimeout(() => setRuleToast(null), 3000);
  };

  const handleImportRules = async () => {
    try {
      const count = await importRules(replaceOnImport);
      if (count === null) return;
      setRuleToast({
        message: t("settings.rules.importSuccess", { count }),
        type: "success",
      });
    } catch (e) {
      console.error("Import rules failed:", e);
      setRuleToast({ message: `${t("settings.rules.importError")}. ${String(e)}`, type: "error" });
    }
    setTimeout(() => setRuleToast(null), 3000);
  };

  const handleImportArchive = async () => {
    try {
      const selected = await open({
        filters: [{ name: "Archives", extensions: ["zip", "tgz", "gz"] }],
        multiple: false,
      });
      const path = Array.isArray(selected) ? selected[0] : selected;
      if (!path) return;

      setIsImportingArchive(true);
      setArchiveProgress(null);
      setArchiveToast({ message: t("settings.archive.importing"), type: "info" });

      const summary = await invoke<ArchiveImportResult>("import_archive_cmd", { path });
      await loadLogs();
      setArchiveToast({
        message: t("settings.archive.importSuccess", {
          count: summary.sortedCount,
          extracted: summary.extractedCount,
        }),
        type: "success",
        stagingPath: summary.stagingPath,
      });
    } catch (e) {
      console.error("Import archive failed:", e);
      setArchiveToast({
        message: t("settings.archive.importError", { error: String(e) }),
        type: "error",
      });
    } finally {
      setIsImportingArchive(false);
    }
  };

  const currentGraceSeconds = useMemo(
    () => unitToSeconds(graceValue, graceUnit),
    [graceValue, graceUnit]
  );

  const sliderIndex = useMemo(() => {
    const clamped = Math.min(currentGraceSeconds, MAX_GRACE_SECONDS);
    const nearest = nearestGraceStep(clamped);
    return GRACE_STEPS.indexOf(nearest);
  }, [currentGraceSeconds]);

  return (
    <div className="flex h-full bg-surface text-text">
      {/* Sidebar */}
      <div className="w-56 border-r border-border bg-surface-dark flex flex-col">
        <div className="px-4 py-4 flex items-center gap-2">
          <button
            aria-label={t("common.close")}
            onClick={() => {
              invoke("close_settings").catch((e) => setWriteError(String(e)));
            }}
            className="p-1.5 rounded-md hover:bg-border transition-colors"
          >
            <ChevronLeft size={16} />
          </button>
          <span className="font-semibold text-sm">{t("settings.title")}</span>
        </div>
        <nav className="flex-1 px-2 space-y-0.5">
          <SidebarButton
            active={tab === "folders"}
            onClick={() => setTab("folders")}
            icon={<Folder size={16} />}
            label={t("settings.folders.title")}
          />
          <SidebarButton
            active={tab === "rules"}
            onClick={() => setTab("rules")}
            icon={<List size={16} />}
            label={t("settings.rules.title")}
          />
          <SidebarButton
            active={tab === "review"}
            onClick={() => { setReviewPaths(null); setTab("review"); }}
            icon={<Inbox size={16} />}
            label={t("beta.review")}
          />
          <SidebarButton
            active={tab === "history"}
            onClick={() => setTab("history")}
            icon={<History size={16} />}
            label={t("settings.history.title")}
          />
          <SidebarButton
            active={tab === "ignore"}
            onClick={() => setTab("ignore")}
            icon={<X size={16} />}
            label={t("settings.ignore.title")}
          />
          <SidebarButton
            active={tab === "general"}
            onClick={() => setTab("general")}
            icon={<Globe size={16} />}
            label={t("settings.general.title")}
          />
          <SidebarButton
            active={tab === "about"}
            onClick={() => setTab("about")}
            icon={<Info size={16} />}
            label={t("settings.about.title")}
          />
        </nav>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-6">
        {writeError && <p role="alert" className="mb-4 rounded-md border border-red-200 bg-red-50 p-3 text-sm text-red-700">{writeError}</p>}
        {isSaving && <p role="status" className="mb-3 text-xs text-text-muted">{t("common.saving")}</p>}
        <fieldset disabled={isSaving} className="min-w-0">
        {tab === "folders" && (
          <div className="space-y-4">
            <h2 ref={sectionHeading} tabIndex={-1} className="text-lg font-semibold">{t("settings.folders.title")}</h2>
            <div className="flex gap-2">
              <input
                type="text"
                value={newFolderPath}
                onChange={(e) => setNewFolderPath(e.target.value)}
                placeholder={t("settings.folders.placeholder")}
                aria-label={t("settings.folders.placeholder")}
                className="flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
              />
              <button type="button" aria-label={t("beta.pickFolder")} className="rounded-md border border-border px-3"
                onClick={() => void open({ directory: true }).then((path) => { if (typeof path === "string") setNewFolderPath(path); }).catch((error) => setWriteError(errorText(error)))}>
                <FolderOpen size={16} />
              </button>
              <button
                onClick={handleAddFolder}
                className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-2 text-sm font-medium text-on-primary hover:bg-primary-hover"
              >
                <Plus size={14} />
                {t("settings.folders.add")}
              </button>
            </div>
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={newOnlyNew} onChange={(event) => setNewOnlyNew(event.target.checked)} />
              {t("beta.onlyNew")}
            </label>
            <div className="rounded-lg border border-border bg-surface-dark p-4 space-y-3">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <h3 className="text-sm font-semibold">{t("settings.archive.title")}</h3>
                  <p className="mt-1 text-xs text-text-muted">
                    {t("settings.archive.description")}
                  </p>
                </div>
                <button
                  onClick={handleImportArchive}
                  disabled={isImportingArchive}
                  className="flex shrink-0 items-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm hover:bg-surface transition-colors disabled:cursor-not-allowed disabled:opacity-60"
                >
                  <Upload size={14} />
                  {isImportingArchive
                    ? t("settings.archive.importingShort")
                    : t("settings.archive.import")}
                </button>
              </div>
              {isImportingArchive && (
                <div className="flex items-center gap-3">
                <button className="rounded-md border border-border px-3 py-2 text-sm hover:bg-surface" onClick={() => {
                  void invoke("cancel_archive_import_cmd").catch((error) => setArchiveToast({ type: "error", message: String(error) }));
                }}>{t("common.cancel")}</button>
                {archiveProgress && <p role="status" className="text-xs text-text-muted">{t("settings.archive.progress", { files: archiveProgress.files, bytes: formatBytes(archiveProgress.bytes) })}</p>}
                </div>
              )}
              {archiveToast && (
                <div
                  className={`text-xs px-3 py-2 rounded-md ${
                    archiveToast.type === "success"
                      ? "bg-green-50 text-green-700 border border-green-200"
                      : archiveToast.type === "error"
                      ? "bg-red-50 text-red-700 border border-red-200"
                      : "bg-blue-50 text-blue-700 border border-blue-200"
                  }`}
                >
                  <div>{archiveToast.message}</div>
                  {archiveToast.stagingPath && (
                    <button
                      onClick={() =>
                        invoke("open_folder_cmd", { path: archiveToast.stagingPath }).catch((error) => setArchiveToast({ ...archiveToast, type: "error", message: String(error) }))
                      }
                      className="mt-2 inline-flex items-center gap-1 text-xs font-medium underline"
                    >
                      <ExternalLink size={12} />
                      {t("settings.archive.openImportFolder")}
                    </button>
                  )}
                </div>
              )}
            </div>
            <div className="space-y-2">
              {folders.map((f) => (
                <div
                  key={f.id}
                  className="flex items-center justify-between rounded-lg border border-border px-4 py-3 gap-3"
                >
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium truncate">{f.path}</div>
                    <div className="mt-1.5">
                      <label className="text-[10px] uppercase tracking-wide text-text-muted block mb-1">
                        {t("settings.folders.mode")}
                      </label>
                      <select
                        aria-label={`${f.path}: ${t("settings.folders.mode")}`}
                        value={f.mode || "silent"}
                        onChange={(e) => { if (f.id !== undefined) void runWrite(() => updateFolderMode(f.id!, e.target.value)); }}
                        className="appearance-none w-full max-w-xs rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                      >
                        <option value="silent">{t("settings.folders.modeSilent")}</option>
                        <option value="manual">{t("settings.folders.modeManual")}</option>
                        <option value="suggest">{t("beta.suggest")}</option>
                        <option value="paused">{t("settings.folders.modePaused")}</option>
                      </select>
                      <p className="text-[10px] text-text-muted mt-1">
                        {t("settings.folders.modeDesc")}
                      </p>
                      <label className="mt-2 flex items-center gap-2 text-sm">
                        <input type="checkbox" checked={f.only_new ?? false} onChange={(event) => {
                          const enabled = event.target.checked;
                          if (f.id !== undefined) void runWrite(async () => {
                            await invoke("set_only_new_cmd", { id: f.id, enabled });
                            await loadFolders();
                          });
                        }} />
                        {t("beta.onlyNew")}
                      </label>
                    </div>
                  </div>
                  <button
                    onClick={(event) => askRemoveFolder(f, event.currentTarget)}
                    className="p-1.5 rounded-md text-red-500 hover:bg-red-50 shrink-0"
                    title={t("confirm.op.removeFolder")}
                    aria-label={`${t("confirm.op.removeFolder")}: ${f.path}`}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              ))}
            </div>
          </div>
        )}

        {tab === "rules" && (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h2 ref={sectionHeading} tabIndex={-1} className="text-lg font-semibold">{t("settings.rules.title")}</h2>
              <button
                onClick={() =>
                  setEditingRule({
                    name: "",
                    priority: 0,
                    enabled: true,
                    extensions: [],
                    pattern: null,
                    destination: "",
                    action: "move",
                    folder_id: 0,
                  })
                }
                className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-2 text-sm font-medium text-on-primary hover:bg-primary-hover"
              >
                <Plus size={14} />
                {t("settings.rules.add")}
              </button>
            </div>

            {/* Export / Import */}
            <div className="rounded-lg border border-border bg-surface-dark p-4 space-y-3">
              <div className="flex flex-wrap items-center gap-2">
                <button
                  onClick={handleExportRules}
                  className="flex items-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm hover:bg-surface transition-colors"
                >
                  <Download size={14} />
                  {t("settings.rules.export")}
                </button>
                <button
                  onClick={handleImportRules}
                  className="flex items-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm hover:bg-surface transition-colors"
                >
                  <Upload size={14} />
                  {t("settings.rules.import")}
                </button>
              </div>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={replaceOnImport}
                  onChange={(e) => setReplaceOnImport(e.target.checked)}
                />
                {t("settings.rules.replaceOnImport")}
              </label>
              {ruleToast && (
                <div
                  className={`text-xs px-3 py-2 rounded-md ${ruleToast.type === "success"
                    ? "bg-green-50 text-green-700 border border-green-200"
                    : "bg-red-50 text-red-700 border border-red-200"
                    }`}
                >
                  {ruleToast.message}
                </div>
              )}
            </div>

            {editingRule && (
              <div className="rounded-lg border border-border bg-surface-dark p-4 space-y-3">
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <label className="text-xs font-medium text-text-muted">{t("settings.rules.name")}</label>
                    <input
                      aria-label={t("settings.rules.name")}
                      value={editingRule.name}
                      onChange={(e) => setEditingRule({ ...editingRule, name: e.target.value })}
                      className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                    />
                  </div>
                  <div>
                    <label className="text-xs font-medium text-text-muted">{t("settings.rules.extensions")}</label>
                    <input
                      aria-label={t("settings.rules.extensions")}
                      value={editingRule.extensions.join(", ")}
                      onChange={(e) =>
                        setEditingRule({
                          ...editingRule,
                          extensions: e.target.value.split(",").map((s) => s.trim()),
                        })
                      }
                      placeholder={t("settings.rules.extensions")}
                      className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                    />
                  </div>
                  <div>
                    <label className="text-xs font-medium text-text-muted">{t("settings.rules.pattern")}</label>
                    <input
                      aria-label={t("settings.rules.pattern")}
                      value={editingRule.pattern || ""}
                      onChange={(e) =>
                        setEditingRule({
                          ...editingRule,
                          pattern: e.target.value.trim() === "" ? null : e.target.value,
                        })
                      }
                      placeholder="(?i)report.*\.pdf"
                      className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                    />
                  </div>
                  {editingRule.action === "move" && <div>
                    <label className="text-xs font-medium text-text-muted">{t("settings.rules.destination")}</label>
                    <input
                      aria-label={t("settings.rules.destination")}
                      value={editingRule.destination}
                      onChange={(e) => setEditingRule({ ...editingRule, destination: e.target.value })}
                      className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                    />
                    <button type="button" className="mt-2 rounded-md border border-border px-2 py-1 text-xs"
                      onClick={() => void open({ directory: true }).then((path) => {
                        const roots = folders.filter((folder) => !editingRule.folder_id || folder.id === editingRule.folder_id).map((folder) => folder.path);
                        const destination = nextDestination(path, roots);
                        if (destination !== null) setEditingRule((rule) => rule && { ...rule, destination });
                      }).catch((error) => setWriteError(errorText(error)))}>
                      {t("beta.pickFolder")}
                    </button>
                  </div>}
                  <div>
                    <label className="text-xs font-medium text-text-muted">{t("settings.rules.priority")}</label>
                    <input
                      aria-label={t("settings.rules.priority")}
                      type="number"
                      value={editingRule.priority}
                      onChange={(e) =>
                        setEditingRule({ ...editingRule, priority: parseInt(e.target.value) || 0 })
                      }
                      className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                    />
                  </div>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <label className="text-sm">{t("settings.rules.action")}
                    <select className="mt-1 block w-full rounded-md border border-border bg-surface p-2" value={editingRule.action}
                      onChange={(event) => setEditingRule({ ...editingRule, action: event.target.value })}>
                      {["move", "rename", "delete", "ignore"].map((action) => (
                        <option key={action} value={action} disabled={action === "delete"}>{t(`beta.${action}`)}</option>
                      ))}
                    </select>
                  </label>
                  <label className="text-sm">{t("settings.folders.title")}
                    <select className="mt-1 block w-full rounded-md border border-border bg-surface p-2" value={editingRule.folder_id}
                      onChange={(event) => setEditingRule({ ...editingRule, folder_id: Number(event.target.value) })}>
                      <option value={0}>{t("beta.allWatched")}</option>
                      {folders.filter((folder) => folder.id !== undefined).map((folder) => <option key={folder.id} value={folder.id}>{folder.path}</option>)}
                    </select>
                  </label>
                </div>
                <RuleConditions rule={editingRule} onChange={setEditingRule} />
                <label className="flex items-center gap-2 text-sm">
                  <input type="checkbox" checked={editingRule.normalize_extensions ?? false}
                    onChange={(event) => setEditingRule({ ...editingRule, normalize_extensions: event.target.checked })} />
                  {t("settings.rules.normalizeExtensions")}
                </label>
                {editingRule.normalize_extensions && <input aria-label={t("settings.rules.normalizeExtensions")}
                  className="w-full rounded-md border border-border bg-surface p-2 text-sm" value={editingRule.extension_mappings ?? "jpeg:jpg"}
                  onChange={(event) => setEditingRule({ ...editingRule, extension_mappings: event.target.value })} />}
                <label className="block text-sm">{t("settings.rules.notificationMessage")}
                  <input className="mt-1 block w-full rounded-md border border-border bg-surface p-2"
                    value={editingRule.notification_message ?? ""} placeholder={t("settings.rules.notificationPlaceholder")}
                    onChange={(event) => setEditingRule({ ...editingRule, notification_message: event.target.value || null })} />
                </label>
                <div className="flex items-center gap-3">
                  <label className="flex items-center gap-1.5 text-sm">
                    <input
                      type="checkbox"
                      checked={editingRule.enabled}
                      onChange={(e) => setEditingRule({ ...editingRule, enabled: e.target.checked })}
                    />
                    {t("settings.rules.enabled")}
                  </label>
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={handleSaveRule}
                    className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-on-primary hover:bg-primary-hover"
                  >
                    <Save size={14} />
                    {t("settings.rules.edit")}
                  </button>
                  <button
                    onClick={() => setEditingRule(null)}
                    className="flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-surface"
                  >
                    <X size={14} />
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            )}

            <div className="space-y-2">
              {rules.map((r) => (
                <div
                  key={r.id}
                  className="flex items-center justify-between rounded-lg border border-border px-4 py-3"
                >
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">{r.name}</span>
                      {!r.enabled && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-border text-text-muted">
                          {t("common.off")}
                        </span>
                      )}
                    </div>
                    <div className="text-xs text-text-muted mt-0.5">
                      {r.extensions.join(", ")} → {r.destination}
                    </div>
                  </div>
                  <div className="flex items-center gap-1">
                    <button
                      type="button"
                      role="switch"
                      aria-checked={r.enabled}
                      aria-label={`${r.name}: ${r.enabled ? t("settings.rules.enabled") : t("common.off")}`}
                      onClick={() => { void runWrite(() => updateRule({ ...r, enabled: !r.enabled })); }}
                      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 ${r.enabled ? "bg-primary" : "bg-border"
                        }`}
                    >
                      <span
                        className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${r.enabled ? "translate-x-5" : "translate-x-1"
                          }`}
                      />
                    </button>
                    <button
                      aria-label={`${t("settings.rules.edit")}: ${r.name}`}
                      onClick={() => setEditingRule({ ...r })}
                      className="p-1.5 rounded-md hover:bg-border text-text-muted"
                    >
                      <Save size={14} />
                    </button>
                    <button
                      onClick={(event) => askDeleteRule(r, event.currentTarget)}
                      className="p-1.5 rounded-md text-red-500 hover:bg-red-50"
                      title={t("confirm.op.deleteRule")}
                      aria-label={`${t("confirm.op.deleteRule")}: ${r.name}`}
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {tab === "review" && <ReviewQueue paths={reviewPaths} />}
        {tab === "history" && (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h2 ref={sectionHeading} tabIndex={-1} className="text-lg font-semibold">{t("settings.history.title")}</h2>
              <div className="flex items-center gap-2">
                <button
                  onClick={(event) => void askUndoAll(event.currentTarget)}
                  disabled={!historyCounts || historyCounts.undoable === 0}
                  title={!historyCounts || historyCounts.undoable === 0 ? t("settings.history.revertAllDisabled") : undefined}
                  className="flex items-center gap-1.5 rounded-md border border-amber-300 px-3 py-2 text-sm text-amber-700 hover:bg-amber-50 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <RotateCcw size={14} />
                  {t("settings.history.revertAll")}
                </button>
                <button
                  onClick={(event) => void askClearLogs(event.currentTarget)}
                  disabled={!historyCounts || historyCounts.total === 0}
                  className="flex items-center gap-1.5 rounded-md border border-red-200 px-3 py-2 text-sm text-red-600 hover:bg-red-50"
                >
                  <Trash2 size={14} />
                  {t("settings.history.clear")}
                </button>
              </div>
            </div>
            <ActionRefreshWarnings />
            {undoToast && (
              <div role="status" aria-live="polite" className="space-y-1">
                {undoToast.map((notice) => (
                  <div
                    key={notice.key}
                    className={`rounded-md px-3 py-2 text-xs ${UNDO_TONE_CLASS[notice.tone]}`}
                  >
                    {t(notice.key, notice.values)}
                  </div>
                ))}
              </div>
            )}
            <HistoryView />
          </div>
        )}

        {tab === "general" && (
          <div className="space-y-6 max-w-md">
            {/* Settings */}
            <div className="space-y-4">
              <div>
                <label className="text-sm font-medium text-text-muted block mb-2">
                  {t("settings.general.language")}
                </label>
                <select
                  aria-label={t("settings.general.language")}
                  value={settings?.language || "en"}
                  disabled={!settings}
                  onChange={(e) => handleChangeLanguage(e.target.value)}
                  className="appearance-none w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
                >
                  <option value="en">English</option>
                  <option value="pl">Polski</option>
                  <option value="it">Italiano</option>
                  <option value="de">Deutsch</option>
                  <option value="fr">Français</option>
                  <option value="ru">Русский</option>
                  <option value="ja">日本語</option>
                  <option value="vi">Tiếng Việt</option>
                  <option value="zh-CN">简体中文</option>
                  <option value="es">Español</option>
                  <option value="uk">Українська</option>
                </select>
              </div>
              <div>
                <label className="text-sm font-medium text-text-muted block mb-2">
                  {t("settings.general.theme")}
                </label>
                <select
                  aria-label={t("settings.general.theme")}
                  value={settings?.theme || "system"}
                  disabled={!settings}
                  onChange={(e) => { void savePreferences({ theme: e.target.value }); }}
                  className="appearance-none w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
                >
                  <option value="system">{t("settings.general.themeSystem")}</option>
                  <option value="light">{t("settings.general.themeLight")}</option>
                  <option value="dark">{t("settings.general.themeDark")}</option>
                </select>
              </div>
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium text-text-muted">
                  {t("settings.general.startWithSystem")}
                </label>
                <button
                  role="switch"
                  aria-label={t("settings.general.startWithSystem")}
                  aria-checked={settings?.autostart ?? false}
                  disabled={!settings}
                  onClick={() => { void runWrite(() => setAutostart(!settings?.autostart)); }}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${settings?.autostart ? "bg-primary" : "bg-surface-dark"
                    }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${settings?.autostart ? "translate-x-6" : "translate-x-1"
                      }`}
                  />
                </button>
              </div>

              {/* Grace Period */}
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <label className="text-sm font-medium text-text-muted">
                    {t("settings.general.gracePeriod")}
                  </label>
                  <span className="text-xs text-text-muted">
                    {formatDuration(currentGraceSeconds)}
                  </span>
                </div>
                <input
                  aria-label={t("settings.general.gracePeriod")}
                  type="range"
                  min={0}
                  max={GRACE_STEPS.length - 1}
                  step={1}
                  value={sliderIndex}
                  onChange={(e) => handleGraceSliderChange(parseInt(e.target.value, 10))}
                  className="w-full"
                />
                <div className="flex items-center gap-2">
                  <input
                    aria-label={t("settings.general.gracePeriod")}
                    type="number"
                    min={0}
                    value={graceValue}
                    onChange={(e) =>
                      handleGraceNumberChange(parseInt(e.target.value, 10) || 0, graceUnit)
                    }
                    className="w-24 rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                  />
                  <select
                    aria-label={t("settings.general.gracePeriodUnit")}
                    value={graceUnit}
                    onChange={(e) =>
                      handleGraceNumberChange(graceValue, e.target.value as GraceUnit)
                    }
                    className="appearance-none rounded-md border border-border bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
                  >
                    <option value="seconds">{t("settings.general.gracePeriodSeconds")}</option>
                    <option value="minutes">{t("settings.general.gracePeriodMinutes")}</option>
                    <option value="hours">{t("settings.general.gracePeriodHours")}</option>
                  </select>
                </div>
                {graceError && (
                  <p className="text-xs text-red-500">{graceError}</p>
                )}
                <p className="text-xs text-text-muted">
                  {t("settings.general.gracePeriodDesc")}
                </p>
              </div>

              {/* Lock Check */}
              <div className="flex items-center justify-between">
                <div>
                  <label className="text-sm font-medium text-text-muted">
                    {t("settings.general.checkFileLock")}
                  </label>
                  <p className="text-xs text-text-muted">
                    {t("settings.general.checkFileLockDesc")}
                  </p>
                </div>
                <button
                  role="switch"
                  aria-label={t("settings.general.checkFileLock")}
                  aria-checked={settings?.lock_check_enabled ?? false}
                  disabled={!settings}
                  onClick={() => {
                    if (!settings) return;
                    void savePreferences({ lock_check_enabled: !settings.lock_check_enabled });
                  }}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${settings?.lock_check_enabled ? "bg-primary" : "bg-surface-dark"
                    }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${settings?.lock_check_enabled ? "translate-x-6" : "translate-x-1"
                      }`}
                  />
                </button>
              </div>

            </div>

            <div className="border-t border-border pt-6 space-y-3">
              <label className="flex items-center gap-2 text-sm font-medium">
                <input type="checkbox" checked={settings?.local_ai_enabled ?? false} disabled={!settings}
                  onChange={(e) => { void savePreferences({ local_ai_enabled: e.target.checked }); }} />
                {t("settings.general.localAi")}
              </label>
              <p className="text-xs text-text-muted">{t("settings.general.localAiPrivacy")}</p>
              <label className="block text-sm text-text-muted" htmlFor="local-ai-model">{t("settings.general.localAiModel")}</label>
              <div className="flex gap-2">
                <input id="local-ai-model" value={aiModel} onChange={(e) => setAiModel(e.target.value)}
                  className="min-w-0 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm" />
                <button type="button" disabled={!settings || !aiModel.trim()}
                  onClick={() => { void savePreferences({ local_ai_model: aiModel.trim() }); }}
                  className="rounded-md bg-primary px-3 py-2 text-sm font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50">
                  {t("common.save")}
                </button>
              </div>
            </div>

            {explorer[0] && <div className="border-t border-border pt-6">
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={explorer[1]} onChange={(event) => {
                  const enabled = event.target.checked;
                  void runWrite(async () => {
                    await invoke("explorer_integration_cmd", { enabled });
                    setExplorer([true, enabled]);
                  });
                }} />
                {t("beta.explorer")}
              </label>
              <p className="mt-1 text-xs text-text-muted">{t("beta.explorerHelp")}</p>
            </div>}
            {/* Scheduler */}
            <div className="border-t border-border pt-6 space-y-4">
              <div>
                <h3 className="text-base font-semibold">{t("settings.scheduler.title")}</h3>
                <p className="text-xs text-text-muted">{t("settings.scheduler.desc")}</p>
              </div>
              <SchedulerRunStatus />

              <div className="flex items-center justify-between">
                <label className="text-sm font-medium text-text-muted">
                  {t("settings.scheduler.enable")}
                </label>
                <button
                  role="switch"
                  aria-label={t("settings.scheduler.enable")}
                  aria-checked={localSchedule.schedule_enabled}
                  onClick={() => handleScheduleChange({ schedule_enabled: !localSchedule.schedule_enabled })}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${localSchedule.schedule_enabled ? "bg-primary" : "bg-surface-dark"
                    }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${localSchedule.schedule_enabled ? "translate-x-6" : "translate-x-1"
                      }`}
                  />
                </button>
              </div>

              <div>
                <label className="text-sm font-medium text-text-muted block mb-2">
                  {t("settings.scheduler.timesPerDay")}
                </label>
                <select
                  aria-label={t("settings.scheduler.timesPerDay")}
                  value={localSchedule.schedule_times_per_day}
                  onChange={(e) =>
                    handleScheduleChange({ schedule_times_per_day: parseInt(e.target.value, 10) })
                  }
                  className="appearance-none w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
                >
                  <option value={1}>{t("settings.scheduler.once")}</option>
                  <option value={2}>{t("settings.scheduler.twice")}</option>
                  <option value={3}>{t("settings.scheduler.thrice")}</option>
                  <option value={4}>{t("settings.scheduler.fourTimes")}</option>
                </select>
              </div>

              <div className="space-y-2">
                {Array.from({ length: localSchedule.schedule_times_per_day }).map((_, idx) => {
                  const key = `schedule_time_${idx + 1}` as keyof ScheduleSettings;
                  return (
                    <div key={idx}>
                      <label className="text-xs font-medium text-text-muted block mb-1">
                        {t("settings.scheduler.time", { number: idx + 1 })}
                      </label>
                      <input
                        aria-label={t("settings.scheduler.time", { number: idx + 1 })}
                        type="time"
                        value={(localSchedule[key] as string | null) || ""}
                        onChange={(e) =>
                          handleScheduleChange({ [key]: e.target.value || null } as Partial<ScheduleSettings>)
                        }
                        className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
                      />
                    </div>
                  );
                })}
              </div>

              <button
                onClick={handleSaveSchedule}
                className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-2 text-sm font-medium text-on-primary hover:bg-primary-hover"
              >
                <Save size={14} />
                {t("settings.rules.edit")}
              </button>
            </div>

          </div>
        )}

        {tab === "ignore" && (
          <IgnoreTab />
        )}

        {tab === "about" && (
          <About />
        )}
        </fieldset>
      </div>

      {pending && (
        <ConfirmDialog {...pending} onClose={() => setPending(null)} />
      )}
    </div>
  );
}

function IgnoreTab() {
  const { t } = useTranslation();
  const { folders } = useAppStore();
  const [selectedFolder, setSelectedFolder] = useState("");
  const [draft, setDraft] = useState<{ folder: string; patterns: string[] } | null>(null);
  const [newPattern, setNewPattern] = useState("");
  const [saved, setSaved] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const requestRef = useRef(0);
  const patterns = draft?.folder === selectedFolder ? draft.patterns : [];
  const canEdit = Boolean(draft && draft.folder === selectedFolder && !loading && !saving && folders.some((f) => f.path === selectedFolder));

  useEffect(() => {
    if (selectedFolder && !folders.some((f) => f.path === selectedFolder)) setSelectedFolder("");
  }, [folders, selectedFolder]);

  useEffect(() => {
    const request = ++requestRef.current;
    setDraft(null);
    setSaved(false);
    setError(null);
    setNewPattern("");
    setLoading(Boolean(selectedFolder));
    if (selectedFolder) {
      invoke<string[]>("load_mouziignore_cmd", { folderPath: selectedFolder }).then((patterns) => {
        if (request === requestRef.current) setDraft({ folder: selectedFolder, patterns });
      }).catch((e) => {
        if (request === requestRef.current) setError(String(e));
      }).finally(() => {
        if (request === requestRef.current) setLoading(false);
      });
    }
    return () => { ++requestRef.current; };
  }, [selectedFolder, retry]);

  useEffect(() => {
    if (!saved) return;
    const timer = setTimeout(() => setSaved(false), 2000);
    return () => clearTimeout(timer);
  }, [saved]);

  const handleAdd = () => {
    if (!canEdit || !draft) return;
    const trimmed = newPattern.trim();
    if (!trimmed || patterns.includes(trimmed)) return;
    setDraft({ ...draft, patterns: [...patterns, trimmed] });
    setNewPattern("");
    setSaved(false);
  };

  const handleRemove = (idx: number) => {
    if (!canEdit || !draft) return;
    setDraft({ ...draft, patterns: patterns.filter((_, i) => i !== idx) });
    setSaved(false);
  };

  const handleSave = async () => {
    if (!canEdit || !draft) return;
    const request = requestRef.current;
    setSaving(true);
    setError(null);
    try {
      await invoke("save_mouziignore_cmd", {
        folderPath: draft.folder,
        patterns: [...draft.patterns],
      });
      if (request === requestRef.current) setSaved(true);
    } catch (e) {
      if (request === requestRef.current) setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-6 max-w-md">
      <h2 className="text-lg font-semibold">{t("settings.ignore.rulesTitle")}</h2>
      <p className="text-sm text-text-muted">
        {t("settings.ignore.description")}
      </p>

      <div>
        <label className="text-sm font-medium text-text-muted block mb-2">
          {t("settings.ignore.folder")}
        </label>
        <select
          aria-label={t("settings.ignore.folder")}
          disabled={saving}
          value={selectedFolder}
          onChange={(e) => setSelectedFolder(e.target.value)}
          className="appearance-none w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
        >
          <option value="">{t("settings.ignore.selectFolder")}</option>
          {folders.map((f) => (
            <option key={f.id} value={f.path}>
              {f.path}
            </option>
          ))}
        </select>
      </div>

      {loading && <p role="status" className="text-sm text-text-muted">{t("common.loading")}</p>}
      {error && <div role="alert" className="text-sm text-red-600">{error}
        {!draft && <button type="button" className="block underline mt-2" onClick={() => setRetry((value) => value + 1)}>{t("common.retry")}</button>}
      </div>}

      {selectedFolder && (
        <>
          <div className="space-y-2">
            <label className="text-sm font-medium text-text-muted block">
              {t("settings.ignore.patterns")}
            </label>
            {canEdit && patterns.length === 0 && (
              <p className="text-sm text-text-muted italic">
                {t("settings.ignore.noRules")}
              </p>
            )}
            {patterns.map((p, i) => (
              <div
                key={i}
                className="flex items-center justify-between rounded-md border border-border bg-surface px-3 py-2"
              >
                <code className="text-sm text-primary">{p}</code>
                <button
                  onClick={() => handleRemove(i)}
                  disabled={!canEdit}
                  className="text-text-muted hover:text-red-400 transition-colors"
                  title={t("settings.ignore.remove")}
                  aria-label={`${t("settings.ignore.remove")}: ${p}`}
                >
                  <X size={14} />
                </button>
              </div>
            ))}
          </div>

          <div className="flex gap-2">
            <input
              aria-label={t("settings.ignore.patterns")}
              disabled={!canEdit}
              type="text"
              value={newPattern}
              onChange={(e) => setNewPattern(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAdd()}
              placeholder={t("settings.ignore.placeholder")}
              className="flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-primary"
            />
            <button
              onClick={handleAdd}
              disabled={!canEdit || !newPattern.trim()}
              className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-on-primary hover:bg-primary-hover disabled:opacity-40 transition-colors"
            >
              {t("settings.ignore.add")}
            </button>
          </div>

          <div className="rounded-md border border-border bg-surface p-3">
            <p className="text-xs text-text-muted mb-1">
              <strong className="text-text">{t("settings.ignore.tips")}</strong>
            </p>
            <ul className="text-xs text-text-muted space-y-1 list-disc pl-4">
              <li><code>*.tmp</code> — {t("settings.ignore.extensionTip")}</li>
              <li><code>node_modules/</code> — {t("settings.ignore.folderTip")}</li>
              <li><code>~$*</code> — {t("settings.ignore.officeTip")}</li>
              <li><code>.DS_Store</code> — {t("settings.ignore.exactTip")}</li>
            </ul>
          </div>

          <button
            onClick={handleSave}
            disabled={!canEdit}
            className="flex items-center gap-2 rounded-md bg-primary px-4 py-2.5 text-sm font-medium text-on-primary hover:bg-primary-hover transition-colors"
          >
            <Save size={16} />
            {saving ? t("common.saving") : saved ? t("settings.ignore.saved") : t("settings.ignore.save")}
          </button>
        </>
      )}
    </div>
  );
}

function SidebarButton({
  active,
  onClick,
  icon,
  label,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium transition-colors ${active
        ? "bg-primary/10 text-primary"
        : "text-text-muted hover:bg-border hover:text-text"
        }`}
    >
      {icon}
      {label}
    </button>
  );
}
