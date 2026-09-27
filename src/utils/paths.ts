/** Last path segment, preserving the original separators. */
export function fileName(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i >= 0 ? path.slice(i + 1) : path;
}

/** Parent directory of a file path, or the path itself if there is none. */
export function parentDir(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
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
