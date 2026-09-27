import type { UndoAllResult, UndoResult, UndoStatus } from "../store/useAppStore";

/**
 * How an undo outcome reads to the user. `success` is reserved for "the file is
 * back where it was" and nothing else, so `missing` - a row the backend marked
 * undone while the file stayed in its organised folder - can never inherit it.
 */
export type UndoTone = "success" | "warning" | "error";

export interface UndoNotice {
  tone: UndoTone;
  /** i18n key. Pluralised keys are resolved by i18next off `values.count`. */
  key: string;
  values?: Record<string, string | number>;
}

export interface UndoAllBreakdown {
  total: number;
  /** `ok` + `collision`: the file is back in the source folder. */
  restored: number;
  /** `collision`: restored, but under a different name than the user expects. */
  collision: number;
  /** `missing`: the row says undone, the file is still in its organised folder. */
  missing: number;
  failed: number;
}

/**
 * Every map here is a `Record` keyed by `UndoStatus` rather than a `switch`, so
 * a fifth status added on the Rust side fails to compile in this file instead
 * of silently falling through to the "restored" wording. That fall-through is
 * the whole bug: it produced a green "Undone" badge for files that were never
 * moved back.
 */
const NOTICE_KEY: Record<UndoStatus, string> = {
  ok: "undo.ok",
  collision: "undo.collision",
  missing: "undo.missing",
  failed: "undo.failed",
};

const NOTICE_TONE: Record<UndoStatus, UndoTone> = {
  ok: "success",
  collision: "warning",
  missing: "error",
  failed: "error",
};

/**
 * Whether the file physically moved back. Only these two statuses may render a
 * `restoredTo` path - on `missing` and `failed` the file is still sitting in
 * the folder Mouzi moved it to, so showing a destination would be the exact
 * lie this fix exists to stop.
 */
const MOVED_BACK: Record<UndoStatus, boolean> = {
  ok: true,
  collision: true,
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
  const notices: UndoNotice[] = [
    { tone: NOTICE_TONE[result.status], key: NOTICE_KEY[result.status] },
  ];
  if (MOVED_BACK[result.status] && result.restoredTo) {
    notices.push({
      tone: "success",
      key: "undo.restoredTo",
      values: { path: result.restoredTo },
    });
  }
  // `failed` is the only status where `message` carries the reason rather than
  // a location, so it is the only one that should surface it.
  if (result.status === "failed" && result.message) {
    notices.push({ tone: "error", key: "undo.reason", values: { message: result.message } });
  }
  return notices;
}

export function undoAllBreakdown(results: readonly UndoResult[]): UndoAllBreakdown {
  const breakdown: UndoAllBreakdown = {
    total: results.length,
    restored: 0,
    collision: 0,
    missing: 0,
    failed: 0,
  };
  for (const result of results) {
    if (MOVED_BACK[result.status]) breakdown.restored += 1;
    if (result.status === "collision") breakdown.collision += 1;
    if (result.status === "missing") breakdown.missing += 1;
    if (result.status === "failed") breakdown.failed += 1;
  }
  return breakdown;
}

/**
 * Aggregate one `undo_all_cmd` result.
 *
 * `UndoAllResult.count` is deliberately NOT used for the restored figure: the
 * backend counts every row that is not `failed`, so `missing` rows - where the
 * file was never moved back - land in it. Reporting it as "N restored" is the
 * same lie as the green badge, in bulk. The true restored count is recomputed
 * from `results` above.
 */
export function undoAllNotices(result: UndoAllResult): UndoNotice[] {
  const breakdown = undoAllBreakdown(result.results);
  const untruthful = breakdown.missing + breakdown.failed;
  const notices: UndoNotice[] = [];

  if (breakdown.total === 0) {
    // Reuses the history button's own disabled hint rather than a new string
    // for a case the user just cannot reach.
    return [{ tone: "warning", key: "settings.history.revertAllDisabled" }];
  }

  if (breakdown.restored > 0) {
    notices.push({
      tone: untruthful > 0 ? "warning" : "success",
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
  }
  return notices;
}
