import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { normalizeExtensions } from '../utils/ruleInput';

export interface Rule {
  id?: number;
  name: string;
  priority: number;
  enabled: boolean;
  extensions: string[];
  pattern: string | null;
  destination: string;
  action: string;
  folder_id: number;
  notification_message?: string | null;
  normalize_extensions?: boolean;
  extension_mappings?: string;
  options?: RuleOptions;
}

export interface RuleOptions {
  min_size: number | null; max_size: number | null;
  modified_after: string | null; modified_before: string | null;
  rename_template: string;
}
export const defaultRuleOptions: RuleOptions = { min_size: null, max_size: null, modified_after: null, modified_before: null, rename_template: '{stem}.{extension}' };

export interface WatchedFolder {
  id?: number;
  path: string;
  enabled: boolean;
  mode: string;
  only_new?: boolean;
}

export interface ActionLog {
  id?: number;
  timestamp: string;
  source_path: string;
  destination_path: string | null;
  action: string;
  file_name: string;
  file_type: string;
  undone: boolean;
}

export interface OrganizationLog extends ActionLog {
  run_id: string | null;
  trigger: string;
  file_extension: string;
  file_size: number;
  fingerprint: string | null;
  undo_verified: boolean;
}

export interface AppSettings extends ScheduleSettings {
  id: number | null;
  language: string;
  theme: string;
  telemetry_enabled: boolean;
  first_run: boolean;
  autostart: boolean;
  grace_period_seconds: number;
  lock_check_enabled: boolean;
  recursive_watch: boolean;
  revision: number;
  local_ai_enabled: boolean;
  local_ai_model: string;
  auto_update_enabled?: boolean;
}

export type PreferencesPatch = Partial<Pick<AppSettings,
  'language' | 'theme' | 'autostart' | 'grace_period_seconds' |
  'lock_check_enabled' | 'local_ai_enabled' | 'local_ai_model' | 'auto_update_enabled'
>>;

export interface HistoryCounts {
  total: number;
  undoable: number;
}

export interface ScheduleSettings {
  schedule_enabled: boolean;
  schedule_times_per_day: number;
  schedule_time_1: string | null;
  schedule_time_2: string | null;
  schedule_time_3: string | null;
  schedule_time_4: string | null;
}

/**
 * The outcomes understood by this frontend. IPC values still require runtime
 * validation; the TypeScript union cannot enforce Rust wire values. `missing`
 * is the one that
 * matters: the row is marked undone and the file is never restored, so a
 * caller that ignores it reports a green "Undone" badge for a file that is
 * still in its organised location.
 */
export type UndoStatus = "ok" | "collision" | "missing" | "failed";

export interface UndoResult {
  status: UndoStatus;
  message: string | null;
  restoredTo: string | null;
}

export interface UndoAllResult {
  count: number;
  results: UndoResult[];
}

interface ArchiveImportReceipt {
  extractedCount: number | null;
  sortedCount: number;
  stagingPath: string;
  issue: string | null;
}

interface ArchiveImportResult extends ArchiveImportReceipt {
  results: [file: string, rule: string, destination: string][];
}

export interface ArchiveImportState {
  revision: number;
  jobId: number | null;
  status: 'idle' | 'running' | 'completed' | 'failed';
  phase: 'extracting' | 'sorting' | null;
  bytes: number;
  files: number;
  cancelRequested: boolean;
  result: ArchiveImportReceipt | null;
  error: string | null;
}

type ActionRefreshErrors = Record<'history' | 'historyCounts' | 'stats', string | null>;
type FolderRead = { status: 'loading' | 'ready' } | { status: 'failed'; error: string };
type PendingFile = [folder: string, name: string];

export function pendingFilePath([folder, name]: PendingFile): string {
  return folder.replace(/[\\/]$/, '') + (folder.includes('\\') ? '\\' : '/') + name;
}

interface AppState {
  rules: Rule[];
  ruleRefreshError: string | null;
  folders: WatchedFolder[];
  folderRead: FolderRead;
  logs: ActionLog[];
  historyCounts: HistoryCounts | null;
  stats: { file_type: string; count: number }[];
  settings: AppSettings | null;
  schedule: ScheduleSettings | null;
  pendingFiles: PendingFile[];
  selectedPendingPaths: ReadonlySet<string>;
  isLoading: boolean;
  actionRefreshErrors: ActionRefreshErrors;
  archiveImportState: ArchiveImportState | null;
  archiveStatusError: string | null;
  archiveImportError: string | null;
  archiveCancelError: string | null;
  archivePickerOpen: boolean;
  archiveImportPending: boolean;
  archiveCancelingJob: number | null;

  loadSettings: () => Promise<void>;
  applySettings: (settings: AppSettings) => void;
  saveSettings: (settings: PreferencesPatch) => Promise<void>;
  setAutostart: (enabled: boolean) => Promise<void>;
  loadRules: (reportRefreshError?: boolean) => Promise<void>;
  loadFolders: () => Promise<void>;
  loadLogs: (reportActionWarning?: boolean) => Promise<void>;
  loadHistoryCounts: (reportActionWarning?: boolean) => Promise<HistoryCounts>;
  loadStats: (reportActionWarning?: boolean) => Promise<void>;
  refreshActionViews: () => Promise<void>;
  cleanNow: () => Promise<{ file: string; rule: string; destination: string }[]>;
  undoAction: (id: number) => Promise<UndoResult>;
  undoAll: (reviewedIds: number[]) => Promise<UndoAllResult>;
  addFolder: (path: string, mode: string, onlyNew?: boolean) => Promise<number>;
  removeFolder: (id: number) => Promise<void>;
  updateFolderMode: (id: number, mode: string) => Promise<void>;
  addRule: (rule: Rule) => Promise<void>;
  updateRule: (rule: Rule) => Promise<void>;
  deleteRule: (id: number) => Promise<void>;
  clearLogs: (throughId: number) => Promise<void>;
  getPendingFiles: () => Promise<void>;
  setPendingFileSelected: (path: string, checked: boolean) => void;
  clearPendingSelection: () => void;
  getSchedule: () => Promise<void>;
  updateSchedule: (schedule: ScheduleSettings) => Promise<void>;
  exportRules: () => Promise<boolean>;
  importRules: (replace: boolean) => Promise<number | null>;
  loadArchiveImportState: () => Promise<void>;
  applyArchiveImportState: (snapshot: ArchiveImportState) => void;
  pickArchiveImport: (pick: () => Promise<string | string[] | null>) => Promise<void>;
  importArchive: (path: string) => Promise<ArchiveImportResult>;
  cancelArchiveImport: () => Promise<void>;
}

function scheduleFrom(settings: AppSettings): ScheduleSettings {
  return {
    schedule_enabled: settings.schedule_enabled,
    schedule_times_per_day: settings.schedule_times_per_day,
    schedule_time_1: settings.schedule_time_1,
    schedule_time_2: settings.schedule_time_2,
    schedule_time_3: settings.schedule_time_3,
    schedule_time_4: settings.schedule_time_4,
  };
}

let rulesRequest = 0;
let foldersRequest = 0;
let logsRequest = 0;
let countsRequest = 0;
let statsRequest = 0;
let pendingRequest = 0;
let archiveStateRequest = 0;
const noActionRefreshErrors: ActionRefreshErrors = { history: null, historyCounts: null, stats: null };

function withActionRefreshError(state: AppState, key: keyof ActionRefreshErrors, value: string | null) {
  return state.actionRefreshErrors[key] === value
    ? state.actionRefreshErrors
    : { ...state.actionRefreshErrors, [key]: value };
}

function withSavedRule(rules: Rule[], rule: Rule): Rule[] {
  const saved = { ...rule, extensions: normalizeExtensions(rule.extensions) };
  return [...rules.filter((current) => current.id !== saved.id), saved]
    .sort((first, second) => first.priority - second.priority || (first.id ?? 0) - (second.id ?? 0));
}

export const useAppStore = create<AppState>((set, get) => ({
  rules: [],
  ruleRefreshError: null,
  folders: [],
  folderRead: { status: 'loading' },
  logs: [],
  historyCounts: null,
  stats: [],
  settings: null,
  schedule: null,
  pendingFiles: [],
  selectedPendingPaths: new Set(),
  isLoading: false,
  actionRefreshErrors: noActionRefreshErrors,
  archiveImportState: null,
  archiveStatusError: null,
  archiveImportError: null,
  archiveCancelError: null,
  archivePickerOpen: false,
  archiveImportPending: false,
  archiveCancelingJob: null,

  loadSettings: async () => {
    const settings = await invoke<AppSettings>('get_settings_cmd');
    get().applySettings(settings);
  },

  applySettings: (settings) => {
    set((state) => {
      if (state.settings && settings.revision < state.settings.revision) return {};
      const nextSchedule = scheduleFrom(settings);
      const schedule = state.schedule && Object.keys(nextSchedule).every(
        (key) => state.schedule![key as keyof ScheduleSettings] === nextSchedule[key as keyof ScheduleSettings],
      ) ? state.schedule : nextSchedule;
      return { settings, schedule };
    });
  },

  saveSettings: async (settings) => {
    // Keep the wire patch narrow even if a caller passes a settings object.
    const allowed = ['language', 'theme', 'autostart', 'grace_period_seconds',
      'lock_check_enabled', 'local_ai_enabled', 'local_ai_model'] as const;
    const current = get().settings;
    const patch = Object.fromEntries(allowed.filter((key) =>
      settings[key] !== undefined && settings[key] !== current?.[key],
    ).map((key) => [key, settings[key]])) as PreferencesPatch;
    if (Object.keys(patch).length === 0) return;
    const committed = await invoke<AppSettings>('update_settings_cmd', { settings: patch });
    get().applySettings(committed);
  },

  setAutostart: async (enabled) => {
    const { settings } = get();
    if (!settings) return;
    if (enabled) {
      await invoke('enable_autostart_cmd');
    } else {
      await invoke('disable_autostart_cmd');
    }
    await get().saveSettings({ autostart: enabled });
  },

  loadRules: async (reportRefreshError = false) => {
    const request = ++rulesRequest;
    try {
      const rules = await invoke<Rule[]>('get_rules_cmd');
      if (request === rulesRequest) set({ rules, ruleRefreshError: null });
    } catch (error) {
      if (reportRefreshError && request === rulesRequest) set({ ruleRefreshError: String(error) });
      throw error;
    }
  },

  loadFolders: async () => {
    const request = ++foldersRequest;
    set({ folderRead: { status: 'loading' } });
    try {
      const folders = await invoke<WatchedFolder[]>('get_folders_cmd');
      if (request === foldersRequest) set({ folders, folderRead: { status: 'ready' } });
    } catch (error) {
      if (request === foldersRequest) set({ folderRead: { status: 'failed', error: String(error) } });
      throw error;
    }
  },

  loadLogs: async (reportActionWarning = false) => {
    const request = ++logsRequest;
    try {
      const logs = await invoke<ActionLog[]>('get_logs_cmd', { limit: 50 });
      if (request !== logsRequest) return;
      set((state) => ({ logs, actionRefreshErrors: withActionRefreshError(state, 'history', null) }));
    } catch (error) {
      if (reportActionWarning && request === logsRequest) {
        set((state) => ({ actionRefreshErrors: withActionRefreshError(state, 'history', String(error)) }));
      }
      throw error;
    }
    await get().loadHistoryCounts(reportActionWarning);
  },

  loadHistoryCounts: async (reportActionWarning = false) => {
    const request = ++countsRequest;
    try {
      const historyCounts = await invoke<HistoryCounts>('get_history_counts_cmd');
      if (request === countsRequest) {
        set((state) => ({ historyCounts, actionRefreshErrors: withActionRefreshError(state, 'historyCounts', null) }));
      }
      return historyCounts;
    } catch (error) {
      if (reportActionWarning && request === countsRequest) {
        set((state) => ({ actionRefreshErrors: withActionRefreshError(state, 'historyCounts', String(error)) }));
      }
      throw error;
    }
  },

  loadStats: async (reportActionWarning = false) => {
    const request = ++statsRequest;
    try {
      const raw = await invoke<[string, number][]>('get_stats_cmd');
      if (request === statsRequest) {
        set((state) => ({
          stats: raw.map(([file_type, count]) => ({ file_type, count })),
          actionRefreshErrors: withActionRefreshError(state, 'stats', null),
        }));
      }
    } catch (error) {
      if (reportActionWarning && request === statsRequest) {
        set((state) => ({ actionRefreshErrors: withActionRefreshError(state, 'stats', String(error)) }));
      }
      throw error;
    }
  },

  refreshActionViews: async () => {
    await Promise.allSettled([get().loadLogs(true), get().loadStats(true)]);
  },

  cleanNow: async () => {
    set({ isLoading: true });
    try {
      const results = await invoke<[string, string, string][]>('clean_now_cmd');
      await get().refreshActionViews();
      return results.map(([file, rule, destination]) => ({ file, rule, destination }));
    } finally {
      set({ isLoading: false });
    }
  },

  undoAction: async (id) => {
    const result = await invoke<UndoResult>('undo_action_cmd', { id });
    await get().refreshActionViews();
    return result;
  },

  undoAll: async (reviewedIds) => {
    const result = await invoke<UndoAllResult>('undo_all_cmd', { reviewedIds });
    if (result.results.length > 0) {
      await get().refreshActionViews();
    }
    return result;
  },

  addFolder: async (path, mode, onlyNew = false) => {
    try {
      const id = await invoke<number>('add_folder_cmd', { path, mode, onlyNew });
      await get().loadFolders();
      return id;
    } catch (e) {
      console.error('addFolder failed:', e);
      throw e;
    }
  },

  removeFolder: async (id) => {
    try {
      await invoke('remove_folder_cmd', { id });
      await get().loadFolders();
    } catch (e) {
      console.error('removeFolder failed:', e);
      throw e;
    }
  },

  updateFolderMode: async (id, mode) => {
    try {
      await invoke('update_folder_mode_cmd', { id, mode });
      await get().loadFolders();
    } catch (e) {
      console.error('updateFolderMode failed:', e);
      throw e;
    }
  },

  addRule: async (rule) => {
    const id = await invoke<number>('add_rule_cmd', { rule });
    set((state) => ({ rules: withSavedRule(state.rules, { ...rule, id }) }));
    await get().loadRules(true).catch(() => {});
  },

  updateRule: async (rule) => {
    await invoke('update_rule_cmd', { rule });
    set((state) => ({ rules: withSavedRule(state.rules, rule) }));
    await get().loadRules(true).catch(() => {});
  },

  deleteRule: async (id) => {
    await invoke('delete_rule_cmd', { id });
    set((state) => ({ rules: state.rules.filter((rule) => rule.id !== id) }));
    await get().loadRules(true).catch(() => {});
  },

  clearLogs: async (throughId) => {
    await invoke('clear_logs_cmd', { throughId });
    await get().refreshActionViews();
  },

  getPendingFiles: async () => {
    const request = ++pendingRequest;
    try {
      const pendingFiles = await invoke<PendingFile[]>('get_pending_files_cmd');
      if (request === pendingRequest) {
        // Reconcile every published snapshot, including updates React batches together.
        set(({ selectedPendingPaths }) => {
          if (selectedPendingPaths.size === 0) return { pendingFiles };
          const listed = new Set(pendingFiles.map(pendingFilePath));
          const retained = new Set([...selectedPendingPaths].filter((path) => listed.has(path)));
          return { pendingFiles, selectedPendingPaths: retained };
        });
      }
    } catch (e) {
      console.error('getPendingFiles failed:', e);
    }
  },

  setPendingFileSelected: (path, checked) => {
    set(({ pendingFiles, selectedPendingPaths }) => {
      const selection = new Set(selectedPendingPaths);
      if (!checked) selection.delete(path);
      else if (pendingFiles.some((file) => pendingFilePath(file) === path)) selection.add(path);
      return { selectedPendingPaths: selection };
    });
  },

  clearPendingSelection: () => set({ selectedPendingPaths: new Set() }),

  getSchedule: async () => {
    await get().loadSettings();
  },

  updateSchedule: async (schedule) => {
    const committed = await invoke<AppSettings>('update_schedule_cmd', { schedule });
    get().applySettings(committed);
  },

  exportRules: () => invoke<boolean>('export_rules_cmd'),

  importRules: async (replace) => {
    const count = await invoke<number | null>('import_rules_cmd', { replace });
    if (count !== null) {
      if (replace) set({ rules: [] });
      await get().loadRules(true).catch(() => {});
    }
    return count;
  },

  loadArchiveImportState: async () => {
    const request = ++archiveStateRequest;
    const revision = get().archiveImportState?.revision;
    try {
      const snapshot = await invoke<ArchiveImportState>('get_archive_import_state_cmd');
      get().applyArchiveImportState(snapshot);
      if (request === archiveStateRequest && get().archiveStatusError !== null) set({ archiveStatusError: null });
    } catch (error) {
      if (request === archiveStateRequest && get().archiveImportState?.revision === revision) {
        set({ archiveStatusError: String(error) });
      }
    }
  },

  applyArchiveImportState: (snapshot) => set((state) => {
    if (state.archiveImportState && snapshot.revision <= state.archiveImportState.revision) return state;
    return {
      archiveImportState: snapshot,
      archiveStatusError: null,
      archiveCancelError: snapshot.jobId === state.archiveImportState?.jobId ? state.archiveCancelError : null,
    };
  }),

  pickArchiveImport: async (pick) => {
    const state = get();
    if (!state.archiveImportState || state.archiveImportState.status === 'running'
        || state.archivePickerOpen || state.archiveImportPending || state.archiveStatusError) return;
    set({ archivePickerOpen: true, archiveImportError: null });
    try {
      const selected = await pick();
      const path = Array.isArray(selected) ? selected[0] : selected;
      if (path && get().archiveImportState?.status !== 'running') await get().importArchive(path);
    } catch (error) {
      set({ archiveImportError: String(error) });
    } finally {
      set({ archivePickerOpen: false });
    }
  },

  importArchive: async (path) => {
    if (get().archiveImportPending || get().archiveImportState?.status === 'running') {
      throw new Error('An archive import is already running');
    }
    set({ archiveImportPending: true, archiveImportError: null });
    try {
      const result = await invoke<ArchiveImportResult>('import_archive_cmd', { path });
      if (result.sortedCount > 0) void get().refreshActionViews();
      return result;
    } finally {
      // The caller's Settings page may have been replaced while the command ran.
      // Pull the authoritative receipt even if this WebView missed its event.
      void get().loadArchiveImportState().finally(() => set({ archiveImportPending: false }));
    }
  },

  cancelArchiveImport: async () => {
    const { archiveImportState, archiveCancelingJob } = get();
    if (archiveImportState?.status !== 'running' || archiveImportState.jobId === null
        || archiveImportState.cancelRequested || archiveCancelingJob === archiveImportState.jobId) return;
    const { jobId } = archiveImportState;
    set({ archiveCancelingJob: jobId, archiveCancelError: null });
    try {
      await invoke('cancel_archive_import_cmd', { jobId });
    } catch (error) {
      if (get().archiveImportState?.jobId === jobId) set({ archiveCancelError: String(error) });
    } finally {
      await get().loadArchiveImportState();
      if (get().archiveCancelingJob === jobId) set({ archiveCancelingJob: null });
    }
  },
}));

export async function observeArchiveImportState(signal: AbortSignal): Promise<void> {
  const unlisten = await listen<ArchiveImportState>('archive-import-state-changed', ({ payload }) => {
    if (!signal.aborted) useAppStore.getState().applyArchiveImportState(payload);
  });
  if (signal.aborted) {
    unlisten();
    return;
  }
  signal.addEventListener('abort', unlisten, { once: true });
  // Listen first so no change between the read and registration is lost.
  await useAppStore.getState().loadArchiveImportState();
}
