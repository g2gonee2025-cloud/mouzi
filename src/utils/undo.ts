import type { UndoAllResult, UndoResult, UndoStatus } from "../store/useAppStore";

/** `success` confirms that the file is back at its original path. */
export type UndoTone = "success" | "warning" | "error";

export interface UndoNotice {
  tone: UndoTone;
  /** i18n key. Pluralised keys are resolved by i18next off `values.count`. */
  key: string;
  values?: Record<string, string | number>;
}

export interface UndoAllBreakdown {
  total: number;
  /** The file physically moved back, even when History recording is incomplete. */
  restored: number;
  /** `collision`: restored, but under a different name than the user expects. */
  collision: number;
  /** Restored files whose History entries still need recovery. */
  restoredPending: number;
  /** `missing`: the recorded organized file could not be found for restoration. */
  missing: number;
  failed: number;
}

/** Each wire status has explicit wording, tone and physical outcome. */
const NOTICE_KEY: Record<UndoStatus, string> = {
  ok: "undo.ok",
  collision: "undo.collision",
  restored_pending: "undo.restoredPending",
  missing: "undo.missing",
  failed: "undo.failed",
};

const NOTICE_TONE: Record<UndoStatus, UndoTone> = {
  ok: "success",
  collision: "warning",
  restored_pending: "warning",
  missing: "error",
  failed: "error",
};

/** Only a confirmed physical restore can display a restored path. */
const MOVED_BACK: Record<UndoStatus, boolean> = {
  ok: true,
  collision: true,
  restored_pending: true,
  missing: false,
  failed: false,
};

/** Tone classes, matching the toast palette already used in Settings. */
export const UNDO_TONE_CLASS: Record<UndoTone, string> = {
  success: "bg-green-50 text-green-700 border border-green-200",
  warning: "bg-amber-50 text-amber-700 border border-amber-200",
  error: "bg-red-50 text-red-700 border border-red-200",
};

/**
 * Turn one `undo_action_cmd` result into the lines the user reads. Always at
 * least one line, and the first line is the outcome itself - callers render
 * these in order so the outcome is never buried under a detail.
 */
export function undoNotice(result: UndoResult): UndoNotice[] {
  return [
    { tone: NOTICE_TONE[result.status], key: NOTICE_KEY[result.status] },
    ...undoDetails(result),
  ];
}

function undoDetails(result: UndoResult): UndoNotice[] {
  const notices: UndoNotice[] = [];
  const historyPending = result.status === "restored_pending";
  if (MOVED_BACK[result.status] && result.restoredTo) {
    notices.push({
      tone: historyPending ? "warning" : "success",
      key: "undo.restoredTo",
      values: { path: result.restoredTo },
    });
  }
  if ((historyPending || result.status === "failed") && result.message) {
    notices.push({
      tone: NOTICE_TONE[result.status],
      key: "undo.reason",
      values: { message: result.message },
    });
  }
  return notices;
}

export function undoAllBreakdown(results: readonly UndoResult[]): UndoAllBreakdown {
  const breakdown: UndoAllBreakdown = {
    total: results.length,
    restored: 0,
    collision: 0,
    restoredPending: 0,
    missing: 0,
    failed: 0,
  };
  for (const result of results) {
    if (MOVED_BACK[result.status]) breakdown.restored += 1;
    if (result.status === "collision") breakdown.collision += 1;
    if (result.status === "restored_pending") breakdown.restoredPending += 1;
    if (result.status === "missing") breakdown.missing += 1;
    if (result.status === "failed") breakdown.failed += 1;
  }
  return breakdown;
}

/** Aggregate the per-file outcomes from Restore all. */
export function undoAllNotices(result: UndoAllResult): UndoNotice[] {
  const breakdown = undoAllBreakdown(result.results);
  const incomplete = breakdown.restoredPending + breakdown.missing + breakdown.failed;
  const notices: UndoNotice[] = [];

  if (breakdown.total === 0) {
    // Reuses the history button's own disabled hint rather than a new string
    // for a case the user just cannot reach.
    return [{ tone: "warning", key: "settings.history.revertAllDisabled" }];
  }

  if (breakdown.restored > 0) {
    notices.push({
      tone: incomplete > 0 ? "warning" : "success",
      key: "undo.allRestored",
      values: { count: breakdown.restored },
    });
  }

  // Ascending severity. `missing` and `failed` always get their own line even
  // when the run also restored things, so a partial success cannot be read as
  // a complete one.
  if (breakdown.collision > 0) {
    notices.push({
      tone: "warning",
      key: "undo.allCollision",
      values: { count: breakdown.collision },
    });
  }
  if (breakdown.restoredPending > 0) {
    notices.push({
      tone: "warning",
      key: "undo.allRestoredPending",
      values: { count: breakdown.restoredPending },
    });
    for (const row of result.results) {
      if (row.status === "restored_pending") notices.push(...undoDetails(row));
    }
  }
  if (breakdown.missing > 0) {
    notices.push({
      tone: "error",
      key: "undo.allMissing",
      values: { count: breakdown.missing },
    });
  }
  if (breakdown.failed > 0) {
    notices.push({
      tone: "error",
      key: "undo.allFailed",
      values: { count: breakdown.failed },
    });
    const reasons = new Set(result.results.flatMap((row) =>
      row.status === "failed" && row.message ? [row.message] : [],
    ));
    if (reasons.size > 0) {
      notices.push({ tone: "error", key: "undo.reason", values: { message: [...reasons].join("\n") } });
    }
  }
  return notices;
}
