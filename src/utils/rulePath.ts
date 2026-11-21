/**
 * Turns a folder chosen in the rule destination picker into the value the rule
 * should store.
 *
 * Rule destinations are resolved relative to the source watched folder (see
 * `resolve_dest_dir` in `src-tauri/src/rules.rs`).
 */

const SEPARATORS = /[\\/]+/;

function toComponents(path: string): string[] {
  const parts = path.split(SEPARATORS).filter((part) => part.length > 0);
  // A leading separator is the root of an absolute path. Keep it as its own
  // component so the filesystem root itself ("/") can be a watched folder.
  return /^[\\/]/.test(path) ? ["/", ...parts] : parts;
}

/**
 * Windows paths are case-insensitive. The renderer has no direct OS signal
 * here, so the path shape decides: a backslash or a drive letter means Windows.
 * Either side of the comparison is enough, because a watched folder may have
 * been typed with forward slashes even on Windows.
 */
function isWindowsStyle(path: string): boolean {
  return path.includes("\\") || /^[a-z]:/i.test(path);
}

function sameComponent(a: string, b: string, caseInsensitive: boolean): boolean {
  return caseInsensitive ? a.toLowerCase() === b.toLowerCase() : a === b;
}

/**
 * An exact watched folder keeps its absolute path. Descendants are relative to
 * the rule's scoped folder, or to the deepest watched folder for a global rule.
 *
 * An undefined scope means all watched folders. A null scope means the rule's
 * watched folder could not be found, so there is no relative base.
 */
export function relativizeDestination(
  selected: string,
  watchedFolders: string[],
  scopedFolder?: string | null,
): string {
  const selectedParts = toComponents(selected);
  if (selectedParts.length === 0) return selected;

  let deepest = -1;
  let relative: string | null = null;

  for (const folder of watchedFolders) {
    const folderParts = toComponents(folder);
    if (folderParts.length === 0) continue;
    if (folderParts.length > selectedParts.length) continue;

    const caseInsensitive = isWindowsStyle(folder) || isWindowsStyle(selected);
    const contained = folderParts.every((part, i) =>
      sameComponent(part, selectedParts[i], caseInsensitive),
    );
    if (!contained) continue;
    if (folderParts.length === selectedParts.length) return selected;
    if (scopedFolder !== undefined && folder !== scopedFolder) continue;
    if (folderParts.length <= deepest) continue;

    deepest = folderParts.length;
    relative = selectedParts.slice(folderParts.length).join("/");
  }

  return relative ?? selected;
}

/**
 * Maps a folder-picker result to the destination a rule should store, or `null`
 * when the picker was dismissed and the current value must be left alone.
 */
export function nextDestination(
  selected: string | string[] | null,
  watchedFolders: string[],
  scopedFolder?: string | null,
): string | null {
  const path = Array.isArray(selected) ? selected[0] : selected;
  if (!path) return null;
  return relativizeDestination(path, watchedFolders, scopedFolder);
}
