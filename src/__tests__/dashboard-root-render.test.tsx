import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Children, isValidElement, type ComponentProps, type EffectCallback, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Dashboard from "../pages/Dashboard";
import FileBrowser from "../components/dashboard/FileBrowser";
import StorageTreemap from "../components/dashboard/StorageTreemap";
import StorageRibbon from "../components/dashboard/StorageRibbon";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import { EMPTY_INSIGHTS, useDashboardStore, type DashboardStats, type InventoryFile, type RootStat } from "../store/useDashboardStore";
import en from "../i18n/locales/en.json";
import { DEFAULT_DASHBOARD_VIEW } from "../utils/dashboard";
import { formatTimestamp } from "../utils/format";

const mock = vi.hoisted(() => ({
  revealItemInDir: vi.fn(),
  invoke: vi.fn(),
  listen: vi.fn(),
  renderingDashboard: false,
  state: [] as unknown[],
  cursor: 0,
  effects: [] as EffectCallback[],
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: mock.revealItemInDir }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mock.listen }));
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useEffect: (effect: EffectCallback, dependencies?: readonly unknown[]) => {
      if (mock.renderingDashboard) mock.effects.push(effect);
      else actual.useEffect(effect, dependencies);
    },
    useState: (initial: unknown) => {
      if (!mock.renderingDashboard) return actual.useState(initial);
      const index = mock.cursor++;
      if (!(index in mock.state)) mock.state[index] = initial;
      return [
        mock.state[index],
        (next: unknown) => { mock.state[index] = typeof next === "function" ? next(mock.state[index]) : next; },
      ];
    },
  };
});

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: { date?: string; name?: string; path?: string }) => {
      if (key === "dashboard.rootLastScan") return en.dashboard.rootLastScan.replace("{{date}}", values?.date ?? "{{date}}");
      if (key === "dashboard.clearFilter") return en.dashboard.clearFilter.replace("{{name}}", values?.name ?? "{{name}}");
      if (key === "dashboard.rootRevealFailed") {
        return (en.dashboard as Record<string, string>).rootRevealFailed?.replace("{{path}}", values?.path ?? "{{path}}") ?? key;
      }
      return key === "categories.Documents" ? "Documentos" : key;
    },
  }),
}));

vi.mock("../store/useDashboardStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useDashboardStore")>();
  return {
    ...actual,
    useDashboardStore: Object.assign(() => actual.useDashboardStore.getState(), actual.useDashboardStore),
  };
});

const root: RootStat = {
  path: "C:/empty-work", files: 0, bytes: 0, status: "complete", stale: false, lastScanAt: 1, error: null,
};

function dashboardStats(
  watchedRoots: RootStat[],
  scanEligibleRoots = watchedRoots.length,
  largestFiles: InventoryFile[] = [],
): DashboardStats {
  const totalFiles = watchedRoots.reduce((total, root) => total + root.files, 0);
  const totalBytes = watchedRoots.reduce((total, root) => total + root.bytes, 0);
  return {
    revision: 1, totalFiles, totalBytes,
    categoryBreakdown: totalFiles ? [{ category: "Other", files: totalFiles, bytes: totalBytes }] : [],
    largestFiles, recentFiles: [], watchedRoots, lastScanAt: null,
    insights: EMPTY_INSIGHTS, ageBuckets: [], scanEligibleRoots,
  };
}

function mountedDashboard(view = DEFAULT_DASHBOARD_VIEW) {
  let tree: ReactNode;
  function CaptureDashboard() {
    mock.cursor = 0;
    mock.renderingDashboard = true;
    try {
      tree = Dashboard({ view, setView: vi.fn() });
      return tree;
    } finally {
      mock.renderingDashboard = false;
    }
  }
  const html = renderToStaticMarkup(<CaptureDashboard />);
  return { html, tree, effects: mock.effects.splice(0) };
}

function findElement<Props>(tree: ReactNode, matches: (element: ReactElement<Props>) => boolean): ReactElement<Props> | undefined {
  for (const node of Children.toArray(tree)) {
    if (!isValidElement<Props & { children?: ReactNode }>(node)) continue;
    if (matches(node)) return node;
    const nested = findElement(node.props.children, matches);
    if (nested) return nested;
  }
}

function renderStorageRoots(data: RootStat[], onSelect = vi.fn(), selected?: string) {
  let tree: ReactNode;
  function CaptureStorage() {
    mock.cursor = 0;
    mock.renderingDashboard = true;
    try {
      tree = StorageTreemap({ data, totalBytes: 0, onSelect, selected });
      return tree;
    } finally {
      mock.renderingDashboard = false;
    }
  }
  const html = renderToStaticMarkup(<CaptureStorage />);
  for (const effect of mock.effects.splice(0)) effect();
  return { html, tree };
}

function revealRoot(tree: ReactNode, name: string) {
  const action = findElement<{
    "aria-label"?: string; "aria-describedby"?: string; onClick?: () => Promise<void>;
  }>(tree, (element) => element.type === "button" && element.props["aria-label"] === `dashboard.openFolder: ${name}`);
  if (!action?.props.onClick) throw new Error(`Show in folder was absent for ${name}`);
  return { click: action.props.onClick, description: action.props["aria-describedby"] };
}

function rootAlert(tree: ReactNode) {
  return findElement<{ id?: string; role?: string }>(tree, (element) => element.props.role === "alert");
}

function deferredReveal() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const result = new Promise<void>((success, failure) => { resolve = success; reject = failure; });
  return { result, resolve, reject };
}

function clickActivityRetry(tree: ReactNode) {
  const chart = findElement<ComponentProps<typeof ActivityTimeline>>(tree, (element) => element.type === ActivityTimeline);
  if (!chart) throw new Error("Activity card is absent");
  let click: (() => unknown) | undefined;
  function CaptureActivity() {
    const card = ActivityTimeline(chart!.props);
    const retry = findElement<{ children: ReactNode; onClick?: () => unknown }>(
      card, (element) => element.type === "button" && element.props.children === "common.retry",
    );
    click = retry?.props.onClick;
    return card;
  }
  renderToStaticMarkup(<CaptureActivity />);
  if (!click) throw new Error("Activity retry is absent");
  click();
}

function deferredActivity() {
  let resolve!: (rows: [string, number][]) => void;
  let reject!: (error: Error) => void;
  const result = new Promise<[string, number][]>((success, failure) => { resolve = success; reject = failure; });
  return { result, resolve, reject };
}

function renderDashboard(
  watchedRoots: RootStat[],
  view = DEFAULT_DASHBOARD_VIEW,
  scanEligibleRoots = watchedRoots.length,
  largestFiles: InventoryFile[] = [],
  previousRun?: { lastFinishedRunId: string; scanStatus: "failed"; scanErrors: string[] },
) {
  mock.state.length = 0;
  useDashboardStore.setState({ stats: null, ...previousRun });
  mountedDashboard(view);
  const revision = useDashboardStore.getState().statsReadRevision + 1;
  useDashboardStore.setState({
    stats: dashboardStats(watchedRoots, scanEligibleRoots, largestFiles),
    statsReadRevision: revision,
    statsSnapshotRevision: revision,
  });
  return mountedDashboard(view).html;
}

beforeEach(() => {
  useDashboardStore.setState(useDashboardStore.getInitialState(), true);
  mock.state.length = 0;
  mock.invoke.mockReset();
  mock.listen.mockReset();
});

afterEach(() => vi.unstubAllGlobals());

describe("dashboard entry points", () => {
  it("guides a user with no watched folders to Settings", () => {
    const html = renderDashboard([]);
    expect(html).toContain("dashboard.scanNoRoots");
    expect(html).toContain("dashboard.openSettings");
    expect(html).not.toContain("dashboard.noFiles");
  });

  it("offers labeled search before charts and distinguishes an empty inventory", () => {
    const html = renderDashboard([root]);
    expect(html).toMatch(/<label [^>]*>dashboard\.searchFiles /);
    expect(html.indexOf('role="search"')).toBeLessThan(html.indexOf("dashboard.watchedRoots"));
    expect(html).toContain("dashboard.noIndexedFiles");
    expect(html).not.toContain("dashboard.noFiles");
  });

  it("shows activity from the store after the entry inventory read", async () => {
    renderDashboard([{ ...root, files: 1, bytes: 256 }]);
    mock.invoke.mockResolvedValueOnce([["Friday invoices", 2]]);
    await useDashboardStore.getState().loadWeeklyStats();
    const html = mountedDashboard().html;
    expect(html).toContain("dashboard.organizedThisWeek");
    expect(html).toContain("Friday invoices");
    expect(html).toContain(">2<");
    mock.invoke.mockResolvedValueOnce([]);
    await useDashboardStore.getState().loadWeeklyStats();
    const empty = mountedDashboard().html;
    expect(empty).toContain("dashboard.noOrganized");
    expect(empty).not.toContain("Friday invoices");
  });

  it("shows activity loading independently and reserves the empty message for a successful empty result", async () => {
    renderDashboard([root]);
    const result = deferredActivity();
    mock.invoke.mockReturnValueOnce(result.result);
    const read = useDashboardStore.getState().loadWeeklyStats();
    const pending = mountedDashboard().html;
    expect(pending).toContain('role="search"');
    expect(pending).toContain("dashboard.activityLoading");
    expect(pending).not.toContain("dashboard.noOrganized");
    result.resolve([]);
    await read;
    const empty = mountedDashboard().html;
    expect(empty).toContain("dashboard.noOrganized");
    expect(empty).not.toContain("dashboard.activityLoading");
  });

  it("withholds previous activity on failure and retries only the activity read", async () => {
    renderDashboard([root]);
    mock.invoke.mockResolvedValueOnce([["Friday invoices", 2]]);
    await useDashboardStore.getState().loadWeeklyStats();
    expect(mountedDashboard().html).toContain("Friday invoices");
    mock.invoke.mockRejectedValueOnce(new Error("Fixture activity read failed"));
    await useDashboardStore.getState().loadWeeklyStats();
    const failed = mountedDashboard();
    expect(failed.html).toContain('role="search"');
    expect(failed.html).toContain("dashboard.activityReadFailed");
    expect(failed.html).not.toContain("dashboard.noOrganized");
    expect(failed.html).not.toContain("Friday invoices");
    mock.invoke.mockClear();
    const retry = deferredActivity();
    mock.invoke.mockReturnValueOnce(retry.result);
    clickActivityRetry(failed.tree);
    expect(mock.invoke).toHaveBeenCalledExactlyOnceWith("get_stats_cmd");
    const retrying = mountedDashboard().html;
    expect(retrying).toContain("dashboard.activityLoading");
    expect(retrying).not.toContain("dashboard.activityReadFailed");
    retry.resolve([["Monday receipts", 3]]);
    await retry.result;
    const recovered = mountedDashboard().html;
    expect(recovered).toContain("Monday receipts");
    expect(recovered).not.toContain("dashboard.activityReadFailed");
    expect(recovered).not.toContain("Friday invoices");
  });

  it("ignores superseded activity successes and failures", async () => {
    renderDashboard([root]);
    const first = deferredActivity();
    const second = deferredActivity();
    const latest = deferredActivity();
    mock.invoke.mockReturnValueOnce(first.result).mockReturnValueOnce(second.result).mockReturnValueOnce(latest.result);
    const reads = [first, second, latest].map(() => useDashboardStore.getState().loadWeeklyStats());
    second.reject(new Error("Superseded activity failure"));
    await reads[1];
    const pending = mountedDashboard().html;
    expect(pending).toContain("dashboard.activityLoading");
    expect(pending).not.toContain("dashboard.activityReadFailed");
    latest.resolve([["Current activity", 4]]);
    await reads[2];
    first.resolve([["Superseded activity", 9]]);
    await reads[0];
    const ready = mountedDashboard().html;
    expect(ready).toContain("Current activity");
    expect(ready).not.toContain("Superseded activity");
    expect(ready).not.toContain("dashboard.activityReadFailed");
  });

  it("warns above retained rows when reopening after a root failed outside the dashboard", () => {
    const retained = { path: `${root.path}/retained.txt`, rootPath: root.path, size: 100, mtime: 1, category: "Other" };
    const failed = { ...root, status: "failed" as const, stale: true, files: 1, bytes: 100, error: "Folder is unavailable" };
    const firstFailure = { ...failed, path: "C:/first-scan", files: 0, bytes: 0, lastScanAt: null, error: "Folder could not be read" };
    const html = renderDashboard([failed, firstFailure], DEFAULT_DASHBOARD_VIEW, 2, [retained]);
    const warning = html.match(/<div role="alert"[^>]*>[\s\S]*?<\/ul><\/div>/)?.[0];
    expect(warning).toContain("dashboard.scanFailedRoots");
    expect(warning).toContain(failed.path);
    expect(warning).toContain(firstFailure.path);
    expect(warning).toContain("dashboard.scanRetainedSnapshot");
    expect(warning).toContain("dashboard.scanNoSnapshot");
    expect(warning).toContain(failed.error);
    expect(warning).toContain(firstFailure.error);
    expect(html.indexOf(failed.error)).toBeLessThan(html.indexOf('role="search"'));
    expect(html).toContain(retained.path);
    expect(html.split('role="search"')[0].match(/Folder is unavailable/g)).toHaveLength(1);
    expect(renderDashboard([root])).not.toContain("dashboard.scanFailedRoots");
  });

  it("does not show a retained terminal event as a current error on a fresh mount", () => {
    const html = renderDashboard([root], DEFAULT_DASHBOARD_VIEW, 1, [], {
      lastFinishedRunId: "19000000000000000001", scanStatus: "failed", scanErrors: ["Expired worker error"],
    });
    expect(html).not.toContain("Expired worker error");
  });

  it("keeps cached inventory hidden on a failed entry read, then keeps verified rows through a live scan and recovery", async () => {
    vi.stubGlobal("window", new EventTarget());
    const listeners = new Map<string, (event: { payload: unknown }) => void>();
    mock.listen.mockImplementation(async (name: string, onEvent: (event: { payload: unknown }) => void) => {
      listeners.set(name, onEvent);
      return () => { listeners.delete(name); };
    });
    function publish(name: string, payload: unknown) {
      listeners.get(name)?.({ payload });
    }
    const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
    const retained = { path: `${root.path}/prior.txt`, rootPath: root.path, size: 100, mtime: 1, category: "Other" };
    const cached = dashboardStats([{ ...root, files: 1, bytes: 100 }], 1, [retained]);
    const failed = dashboardStats([{ ...cached.watchedRoots[0], status: "failed", stale: true, error: "Folder unreachable" }], 1, [retained]);
    const recovered = dashboardStats([root]);
    let failRead!: (error: Error) => void;
    let read = new Promise<DashboardStats>((_resolve, reject) => { failRead = reject; });
    mock.invoke.mockImplementation(async (name: string) => {
      if (name === "get_dashboard_stats_cmd") return read;
      if (name === "get_stats_cmd") return [];
      if (name === "is_scanning_cmd") return false;
      throw new Error(`Unexpected command: ${name}`);
    });
    useDashboardStore.setState({ stats: cached });
    const initial = mountedDashboard();
    expect(initial.html).not.toContain(retained.path);
    expect(initial.html).toContain("app.loading");
    const cleanups = initial.effects.map((effect) => effect());
    try {
      await settle();
      failRead(new Error("Could not read dashboard"));
      await settle();
      const unavailable = mountedDashboard().html;
      expect(unavailable).toContain("Could not read dashboard");
      expect(unavailable).toContain("common.retry");
      expect(unavailable).not.toContain(retained.path);
      expect(unavailable).not.toContain("dashboard.scanFailedRoots");

      read = Promise.resolve(failed);
      await useDashboardStore.getState().refresh();
      expect(mountedDashboard().html).toContain("Folder unreachable");
      expect(mountedDashboard().html).toContain(retained.path);

      read = Promise.reject(new Error("Temporary refresh error"));
      await useDashboardStore.getState().refresh();
      expect(mountedDashboard().html).toContain("Temporary refresh error");
      expect(mountedDashboard().html).toContain(retained.path);

      const runId = "19000000000000000002";
      publish("scan-complete", { runId, root: root.path, status: "failed", error: "Folder unreachable" });
      publish("scan-finished", { runId, status: "failed", errors: ["Folder unreachable", "Worker stopped unexpectedly"] });
      const live = mountedDashboard().html;
      expect(live).toContain("Worker stopped unexpectedly");
      expect(live.split('role="search"')[0].match(/Folder unreachable/g)).toHaveLength(1);

      const nextRunId = "19000000000000000003";
      publish("scan-complete", { runId: nextRunId, root: root.path, status: "complete", error: null });
      publish("scan-finished", { runId: nextRunId, status: "finished", errors: [] });
      read = Promise.resolve(recovered);
      await useDashboardStore.getState().refresh();
      publish("scan-finished", { runId, status: "failed", errors: ["Worker stopped unexpectedly"] });
      const updated = mountedDashboard().html;
      expect(updated).not.toContain("dashboard.scanFailedRoots");
      expect(updated).not.toContain("Worker stopped unexpectedly");
      expect(updated).not.toContain(retained.path);
    } finally {
      for (const cleanup of cleanups) cleanup?.();
    }
  });

  it("renders Scan Now disabled before the scan listeners connect", () => {
    const html = renderDashboard([root]);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>(?:(?!<\/button>).)*dashboard\.scanNow<\/button>/);
  });

  it("offers to resume inactive folders and keeps their inventory visible", () => {
    const retained = { path: `${root.path}/retained.txt`, rootPath: root.path, size: 100, mtime: 1, category: "Other" };
    const html = renderDashboard([{ ...root, files: 1, bytes: 100 }], DEFAULT_DASHBOARD_VIEW, 0, [retained]);
    expect(html).toContain("dashboard.scanInactiveRoots");
    expect(html).not.toContain("dashboard.scanNoRoots");
    expect(html).toContain("dashboard.watchedRoots");
    expect(html).toContain("empty-work");
    expect(html).toContain(retained.path);
    expect(renderDashboard([root], DEFAULT_DASHBOARD_VIEW, 0)).not.toContain("dashboard.leadEmpty");
    expect(renderDashboard([root])).not.toContain("dashboard.scanInactiveRoots");
  });

  it("reveals the exact file represented by the row", async () => {
    const path = "C:\\work\\quarterly report.txt";
    let reveal: (() => Promise<void>) | undefined;
    function capture(node: ReactNode) {
      Children.forEach(node, (child) => {
        if (!isValidElement<{ children?: ReactNode; "aria-label"?: string; onClick?: () => Promise<void> }>(child)) return;
        if (child.props["aria-label"] === "dashboard.openFolder") reveal = child.props.onClick;
        capture(child.props.children);
      });
    }
    function CaptureBrowser() {
      const browser = FileBrowser({
        ...DEFAULT_DASHBOARD_VIEW,
        initialLargest: [{ path, rootPath: "C:\\work", size: 2, mtime: 1, category: "Other" }],
        initialRecent: [],
        rootScope: "C:\\work",
        totalFiles: 1,
        revision: 1,
        onSort: vi.fn(),
        onQuery: vi.fn(),
        onClearCategory: vi.fn(),
        onClearRoot: vi.fn(),
      });
      capture(browser);
      return browser;
    }
    renderToStaticMarkup(<CaptureBrowser />);
    expect(reveal).toBeTypeOf("function");
    await reveal!();
    expect(mock.revealItemInDir).toHaveBeenCalledExactlyOnceWith(path);
  });

  it("renders supplied view options and excludes a root missing from the current scope", () => {
    const view = { sort: "mtime" as const, query: "receipts", category: "Documents", root: root.path };
    const html = renderDashboard([root], view);
    expect(html).toMatch(/<input[^>]*type="search"[^>]*value="receipts"/);
    expect(html).toMatch(/<button[^>]*aria-pressed="true"[^>]*>dashboard\.recentFiles<\/button>/);
    expect(html).toContain('aria-label="Clear Documentos filter"');
    expect(html).toContain('aria-label="Clear C:/empty-work filter"');

    const newScope = renderDashboard([{ ...root, path: "D:/other" }], view);
    expect(newScope).toContain('value="receipts"');
    expect(newScope).toContain('aria-label="Clear Documentos filter"');
    expect(newScope).not.toContain('aria-label="Clear C:/empty-work filter"');
  });
});

describe("dashboard search translations", () => {
  const locales = import.meta.glob<{ dashboard: Record<string, string> }>(
    "../i18n/locales/*.json", { eager: true, import: "default" },
  );

  it("includes search guidance and preserves placeholders across all shipped locales", () => {
    expect(locales).toHaveProperty(["../i18n/locales/zh-CN.json"]);
    for (const { dashboard } of Object.values(locales)) {
      for (const key of ["searchFiles", "noIndexedFiles", "refreshingFiles", "scanConnectionFailed", "scanInactiveRoots", "scanFailedRoots", "scanRetainedSnapshot", "scanNoSnapshot"]) {
        expect(dashboard[key]?.trim()).toBeTruthy();
      }
      expect(dashboard.filesShown).toContain("{{shown}}");
      expect(dashboard.limitedFiles).toContain("{{shown}}");
      expect(dashboard.clearFilter).toContain("{{name}}");
      expect(dashboard.revealFailed).toContain("{{file}}");
    }
  });
});

describe("configured root presentation", () => {
  it("shows a successfully scanned empty root and its last scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[root]} totalBytes={0} />);
    expect(html).toContain("empty-work");
    expect(html).toContain(en.dashboard.rootLastScan.replace("{{date}}", formatTimestamp(root.lastScanAt)));
    expect(html).not.toContain("{{date}}");
    expect(html).not.toContain("dashboard.noData");
  });

  it("distinguishes an unscanned root from an empty completed scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[{ ...root, status: "unscanned", stale: true, lastScanAt: null }]} totalBytes={0} />);
    expect(html).toContain("dashboard.rootPending");
    expect(html).not.toContain("dashboard.rootStale");
    expect(html).not.toContain(en.dashboard.rootLastScan.split("{{date}}")[0]);
  });

  it("marks a failed empty snapshot stale only if a prior scan completed", () => {
    const failed = { ...root, status: "failed" as const, stale: true, error: "Access denied" };
    const html = renderToStaticMarkup(<StorageTreemap data={[failed]} totalBytes={0} />);
    expect(html).toContain("dashboard.rootStale");
    expect(html).toContain("dashboard.rootFailed");
    expect(html).toContain("Access denied");
    expect(renderToStaticMarkup(<StorageTreemap data={[{ ...failed, lastScanAt: null }]} totalBytes={0} />))
      .not.toContain("dashboard.rootStale");
  });

  it("localizes canonical categories while retaining arbitrary rule labels", () => {
    const ribbon = renderToStaticMarkup(<StorageRibbon data={[{ category: "Documents", files: 1, bytes: 1 }]} totalBytes={1} />);
    expect(ribbon).toContain("Documentos");
    const activity = renderToStaticMarkup(<ActivityTimeline data={[{ file_type: "My receipts", count: 1 }, { file_type: "Documents", count: 1 }]} />);
    expect(activity).toContain("My receipts");
    expect(activity).not.toContain("categories.My receipts");
    expect(activity).toContain("Documents");
    expect(activity).not.toContain("Documentos");
  });
});

describe("watched-root Show in folder", () => {
  it("opens the exact root in directory-only mode", async () => {
    mock.invoke.mockResolvedValueOnce(undefined);
    await revealRoot(renderStorageRoots([root]).tree, "empty-work").click();
    expect(mock.invoke.mock.calls).toEqual([["open_folder_cmd", { path: root.path, directoryOnly: true }]]);
  });

  it("shows a row-specific alert and clears it only when the same root successfully opens", async () => {
    const peer = { ...root, path: "D:/success/working" };
    const selected = vi.fn();
    const current = () => renderStorageRoots([root, peer], selected, root.path);
    mock.invoke.mockRejectedValueOnce(new Error("Fixture directory is missing"));
    await revealRoot(current().tree, "empty-work").click();

    const failed = current();
    expect(failed.html).toContain(`Could not show the watched folder ${root.path}. Check that it exists, then try again.`);
    const alert = rootAlert(failed.tree);
    expect(alert?.props.id).toBeTruthy();
    expect(revealRoot(failed.tree, "empty-work").description).toBe(alert?.props.id);
    expect(revealRoot(failed.tree, "working").description).toBeUndefined();
    const filter = findElement<{
      title?: string; "aria-pressed"?: boolean; onClick?: () => void;
    }>(failed.tree, (element) => element.type === "button" && element.props.title === root.path);
    expect(filter?.props["aria-pressed"]).toBe(true);
    filter?.props.onClick?.();
    expect(selected).toHaveBeenCalledExactlyOnceWith(null);

    mock.invoke.mockResolvedValueOnce(undefined);
    await revealRoot(failed.tree, "working").click();
    expect(rootAlert(current().tree)).toBeDefined();
    const retry = deferredReveal();
    mock.invoke.mockReturnValueOnce(retry.result);
    const pending = revealRoot(current().tree, "empty-work").click();
    expect(rootAlert(current().tree)).toBeDefined();
    retry.resolve();
    await pending;
    expect(rootAlert(current().tree)).toBeUndefined();
  });

  it("ignores older attempts and completions for removed roots", async () => {
    const old = deferredReveal();
    mock.invoke.mockReturnValueOnce(old.result).mockRejectedValueOnce(new Error("Newest reveal failed"));
    const first = revealRoot(renderStorageRoots([root]).tree, "empty-work").click();
    await revealRoot(renderStorageRoots([root]).tree, "empty-work").click();
    expect(rootAlert(renderStorageRoots([root]).tree)).toBeDefined();
    old.resolve();
    await first;
    expect(rootAlert(renderStorageRoots([root]).tree)).toBeDefined();

    const beforeRemoval = deferredReveal();
    mock.invoke.mockReturnValueOnce(beforeRemoval.result);
    const removed = revealRoot(renderStorageRoots([root]).tree, "empty-work").click();
    expect(rootAlert(renderStorageRoots([]).tree)).toBeUndefined();
    expect(rootAlert(renderStorageRoots([root]).tree)).toBeUndefined();
    beforeRemoval.reject(new Error("The previous root went away"));
    await removed;
    expect(rootAlert(renderStorageRoots([root]).tree)).toBeUndefined();
  });
});
