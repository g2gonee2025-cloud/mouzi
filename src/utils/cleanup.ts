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

export interface CleanupOutcome {
  path: string;
  status: "ok" | "failed" | "skipped";
  message?: string;
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