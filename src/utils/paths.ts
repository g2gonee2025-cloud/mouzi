/** Last path segment, preserving the original separators. A trailing
 *  separator is ignored, so a folder path yields its own name. */
export function fileName(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  if (trimmed === "") return path;
  const i = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}

/** Parent directory of a file path, or the path itself if there is none. */
export function parentDir(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  if (i < 0) return path;
  if (i === 0) return path.slice(0, 1);
  const parent = path.slice(0, i);
  // "C:\photo.jpg" would otherwise yield "C:", which Explorer resolves to the
  // current directory on that drive rather than the drive root.
  return /^[A-Za-z]:$/.test(parent) ? parent + path[i] : parent;
}

export function parseHash(hash = typeof window !== "undefined" ? window.location.hash : ""): {
  route: string;
  params: URLSearchParams;
} {
  const raw = hash.replace(/^#\/?/, "") || "popup";
  const qIndex = raw.indexOf("?");
  if (qIndex === -1) {
    return { route: raw || "popup", params: new URLSearchParams() };
  }
  return {
    route: raw.slice(0, qIndex) || "popup",
    params: new URLSearchParams(raw.slice(qIndex + 1)),
  };
}

export function navigateHash(route: string, params?: Record<string, string>) {
  const qs = params && Object.keys(params).length
    ? "?" + new URLSearchParams(params).toString()
    : "";
  window.location.hash = `#/${route}${qs}`;
}
