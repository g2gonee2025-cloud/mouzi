import { Ban, CheckCircle2, HelpCircle, MinusCircle, XCircle } from "lucide-react";
import type { LucideIcon } from "lucide-react";

export interface DuplicateFile {
  path: string;
  size: number;
  mtime: number;
}

export interface DuplicateGroup {
  hash: string;
  files: DuplicateFile[];
}

export interface CleanupFile {
  path: string;
  size: number;
  mtime: number;
}

export interface CleanupRequest {
  kind: "trash_duplicate" | "trash_large" | "trash_stale" | "remove_empty_dir";
  path: string;
  keepPath?: string;
}

/**
 * The cleanup result contract shared with `src-tauri/src/cleanup.rs`.
 *
 * `permanently_deleted` is NOT a flavour of `failed`. `failed` means the move
 * did not happen and the file is still on disk; this means Windows refused the
 * Recycle Bin (typically because it was full) and shredded the file, so no copy
 * exists anywhere. Collapsing the two hides the only outcome the user cannot
 * recover from, which is why it gets its own member rather than a `message`
 * hanging off `failed`.
 */
export type CleanupStatus = "ok" | "failed" | "skipped" | "permanently_deleted";

export interface CleanupOutcome {
  path: string;
  status: CleanupStatus;
  message?: string;
}

/** Worst last: a reader scanning the pills meets the irreversible outcome at the end. */
export const CLEANUP_STATUS_ORDER: readonly CleanupStatus[] = [
  "ok",
  "skipped",
  "failed",
  "permanently_deleted",
];

export interface CleanupStatusMark {
  icon: LucideIcon;
  className: string;
}

export interface CleanupStatusSummary {
  labelKey: string;
  pillClass: string;
}

export type CleanupCountField = "ok" | "skipped" | "failed" | "permanentlyDeleted";

export interface CleanupBreakdown {
  total: number;
  ok: number;
  skipped: number;
  failed: number;
  permanentlyDeleted: number;
  /**
   * Statuses outside `CleanupStatus`. Counted here or nowhere - never folded
   * into a real bucket, because every historical "unknown value" bug in this
   * file was an `else` branch quietly reusing a known bucket's wording.
   */
  unknown: number;
}

/**
 * Every map below is a `Record<CleanupStatus, ...>` rather than a chain of
 * `===` tests, so a fifth status added on the Rust side fails to compile here
 * instead of reaching the final `else`. The `Record` is the compile-time
 * guard; the `Map` built from it is the runtime one, and the miss it can return
 * is the only thing that makes an off-union value detectable at all.
 */
const STATUS_MARK: Record<CleanupStatus, CleanupStatusMark> = {
  ok: { icon: CheckCircle2, className: "text-green-500" },
  skipped: { icon: MinusCircle, className: "text-yellow-500" },
  failed: { icon: XCircle, className: "text-red-500" },
  // Deep red plus a `Ban` glyph no other status uses; at icon size a
  // red-to-red hue shift alone could not separate this from `failed`.
  permanently_deleted: { icon: Ban, className: "text-red-700 dark:text-red-400" },
};

const STATUS_SUMMARY: Record<CleanupStatus, CleanupStatusSummary> = {
  ok: { labelKey: "cleanup.ok", pillClass: "bg-green-100 text-green-700" },
  skipped: { labelKey: "cleanup.skipped", pillClass: "bg-yellow-100 text-yellow-700" },
  failed: { labelKey: "cleanup.failed", pillClass: "bg-red-100 text-red-700" },
  // Filled where the others are tinted, so the irreversible count is the one
  // chip in the row that cannot be skimmed past as just another bad number.
  permanently_deleted: {
    labelKey: "cleanup.permanentlyDeleted",
    pillClass: "bg-red-700 text-white",
  },
};

const COUNT_FIELD: Record<CleanupStatus, CleanupCountField> = {
  ok: "ok",
  skipped: "skipped",
  failed: "failed",
  permanently_deleted: "permanentlyDeleted",
};

const MARK_BY_WIRE = new Map<string, CleanupStatusMark>(Object.entries(STATUS_MARK));
const COUNT_FIELD_BY_WIRE = new Map<string, CleanupCountField>(Object.entries(COUNT_FIELD));

/** Deliberately not `skipped`'s yellow, which is the trap this whole map exists to close. */
const UNKNOWN_MARK: CleanupStatusMark = {
  icon: HelpCircle,
  className: "text-text-muted",
};

/**
 * `status` arrives as a bare string across IPC, so nothing forces it to be a
 * member of the union: an unrecognised value has to be *drawn* as
 * unrecognised rather than inherit a status we do understand.
 */
export function cleanupStatusMark(status: string): CleanupStatusMark {
  return MARK_BY_WIRE.get(status) ?? UNKNOWN_MARK;
}

export function cleanupBreakdown(
  results: readonly { status: string }[],
): CleanupBreakdown {
  const counts: CleanupBreakdown = {
    total: results.length,
    ok: 0,
    skipped: 0,
    failed: 0,
    permanentlyDeleted: 0,
    unknown: 0,
  };
  for (const result of results) {
    const field = COUNT_FIELD_BY_WIRE.get(result.status);
    if (field === undefined) counts.unknown += 1;
    else counts[field] += 1;
  }
  return counts;
}

export interface CleanupSummaryRow extends CleanupStatusSummary {
  status: CleanupStatus;
  count: number;
}

/**
 * The pills above the result rows, with their counts read out of the same
 * breakdown pass. Deriving both from one source is what keeps a label from
 * drifting away from the number printed next to it.
 */
export function cleanupSummaryRows(
  results: readonly { status: string }[],
): CleanupSummaryRow[] {
  const counts = cleanupBreakdown(results);
  return CLEANUP_STATUS_ORDER.map((status) => ({
    status,
    ...STATUS_SUMMARY[status],
    count: counts[COUNT_FIELD[status]],
  }));
}

/** Total bytes that can be reclaimed by keeping one copy per duplicate group. */
export function reclaimableBytes(groups: DuplicateGroup[]): number {
  return groups.reduce((sum, g) => {
    const keepOne = g.files.slice(1);
    return sum + keepOne.reduce((s, f) => s + f.size, 0);
  }, 0);
}

/** Sort duplicate groups by reclaimable bytes descending. */
export function sortGroups(groups: DuplicateGroup[]): DuplicateGroup[] {
  return [...groups].sort((a, b) => {
    const ra = a.files.slice(1).reduce((s, f) => s + f.size, 0);
    const rb = b.files.slice(1).reduce((s, f) => s + f.size, 0);
    return rb - ra;
  });
}

/** The copy a group keeps when the user has not chosen one explicitly. */
export function defaultKeeper(group: DuplicateGroup): string | undefined {
  return group.files[0]?.path;
}

/**
 * Resolve which copy of a group is kept. The UI used to default this to the
 * first file for display while the action builder read the raw selection and
 * got `undefined`, which queued every copy for the bin while the card showed
 * the first one badged "kept" - and dropped the `keepPath` field entirely, so
 * the backend's own self-reference guard could not catch it either. Both
 * callers must go through here so the badge, the count and the action list
 * cannot disagree.
 */
export function resolveKeeper(
  group: DuplicateGroup,
  keptByGroup: Readonly<Record<string, string | undefined>>,
): string | undefined {
  const chosen = keptByGroup[group.hash];
  // A selection left over from an earlier find can name a path that is no
  // longer in the group; trusting it would delete the whole group.
  if (chosen !== undefined && group.files.some((f) => f.path === chosen)) {
    return chosen;
  }
  return defaultKeeper(group);
}

/** Build cleanup actions for duplicate groups given the kept file per group. */
export function buildDuplicateActions(
  groups: DuplicateGroup[],
  keptByGroup: Readonly<Record<string, string | undefined>>,
): CleanupRequest[] {
  const actions: CleanupRequest[] = [];
  for (const g of groups) {
    const keepPath = resolveKeeper(g, keptByGroup);
    // No copy to keep means there is nothing safe to do for this group.
    if (keepPath === undefined) continue;
    for (const f of g.files) {
      if (f.path !== keepPath) {
        actions.push({ kind: "trash_duplicate", path: f.path, keepPath });
      }
    }
  }
  return actions;
}