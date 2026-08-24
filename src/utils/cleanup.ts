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

/** Build cleanup actions for duplicate groups given the kept file per group. */
export function buildDuplicateActions(
  groups: DuplicateGroup[],
  keptByGroup: Record<string, string>,
): CleanupRequest[] {
  const actions: CleanupRequest[] = [];
  for (const g of groups) {
    const keepPath = keptByGroup[g.hash];
    for (const f of g.files) {
      if (f.path !== keepPath) {
        actions.push({ kind: "trash_duplicate", path: f.path, keepPath });
      }
    }
  }
  return actions;
}