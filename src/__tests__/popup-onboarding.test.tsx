import { beforeEach, expect, it, vi } from "vitest";
import { isValidElement, type ReactElement, type ReactNode } from "react";
import Popup from "../components/Popup";
import { useAppStore, type WatchedFolder } from "../store/useAppStore";
import en from "../i18n/locales/en.json";

const fixture = vi.hoisted(() => ({
  invoke: vi.fn(),
  setState: vi.fn(),
  retained: null as { index: number; values: unknown[] } | null,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: fixture.invoke }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: () => {},
  useRef: (current: unknown) => ({ current }),
  useState: (value: unknown) => {
    const retained = fixture.retained;
    if (!retained) return [value, fixture.setState];
    const index = retained.index++;
    if (!(index in retained.values)) retained.values[index] = value;
    return [retained.values[index], (next: unknown) => {
      retained.values[index] = typeof next === "function" ? next(retained.values[index]) : next;
    }];
  },
}));
vi.mock("../store/useAppStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useAppStore")>();
  return { ...actual, useAppStore: Object.assign(actual.useAppStore.getState, actual.useAppStore) };
});
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => {
      const [section, name] = key.split(".");
      const translations = en[section as keyof typeof en] as Record<string, unknown> | undefined;
      const message = translations?.[name];
      return typeof message === "string" ? message : key;
    },
  }),
}));

interface ElementProps {
  "aria-label"?: string;
  children?: ReactNode;
  onClick?: () => Promise<void> | void;
  onChange?: (event: { target: { checked: boolean } }) => void;
  checked?: boolean;
  disabled?: boolean;
  role?: string;
  title?: string;
}

function label(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(label).join("");
  if (typeof node === "string") return node;
  return isValidElement<ElementProps>(node) ? label(node.props.children) : "";
}

function findAction(node: ReactNode, name: string): ElementProps["onClick"] {
  return findElement(node, (element) => element.type === "button" && label(element) === name)?.props.onClick;
}

function findElement(node: ReactNode, match: (element: ReactElement<ElementProps>) => boolean): ReactElement<ElementProps> | undefined {
  if (Array.isArray(node)) return node.map((child) => findElement(child, match)).find(Boolean);
  if (!isValidElement<ElementProps>(node)) return;
  if (match(node)) return node;
  return findElement(node.props.children, match);
}

const invoices: WatchedFolder = { id: 1, path: "C:/Fixture/Invoices", mode: "manual", enabled: true };
const downloads = "C:/Fixture/Downloads";

beforeEach(() => {
  fixture.invoke.mockReset();
  fixture.setState.mockClear();
  fixture.retained = null;
  useAppStore.setState(useAppStore.getInitialState(), true);
});

async function loadFolderList(folders: WatchedFolder[]) {
  fixture.invoke.mockResolvedValueOnce(folders);
  await useAppStore.getState().loadFolders();
  fixture.invoke.mockClear();
}

function renderRetainedPopup() {
  fixture.retained ??= { index: 0, values: [] };
  fixture.retained.index = 0;
  return Popup();
}

function renderRecentActivity(destination: string | null, undoVerified = true, action = "move", undone = false) {
  const log = {
    id: 1, timestamp: "2026-11-20", source_path: "C:/Fixture/arrival.txt",
    destination_path: destination, action, file_name: "arrival.txt", file_type: "text", undone,
    undo_verified: undoVerified,
  };
  useAppStore.setState({ logs: [log], logsReadStatus: "ready" });
  return renderRetainedPopup();
}

function recentButton(popup: ReactNode, title: string) {
  return findElement(popup, (element) => element.type === "button" && element.props.title === title);
}

function traySection(popup: ReactNode, name: string) {
  return findElement(popup, (element) => element.type === "section" && element.props["aria-label"] === name);
}

it("reports pending activity and only shows both empty messages after verified replies", async () => {
  let finishRecent!: (rows: unknown[]) => void;
  let finishWeekly!: (rows: unknown[]) => void;
  fixture.invoke.mockImplementation((command: string) => {
    if (command === "get_logs_cmd") return new Promise(resolve => { finishRecent = resolve; });
    if (command === "get_stats_cmd") return new Promise(resolve => { finishWeekly = resolve; });
    return Promise.resolve({ total: 0, undoable: 0 });
  });
  const store = useAppStore.getState();
  const recent = store.loadLogs(), weekly = store.loadStats();
  const loading = renderRetainedPopup();
  finishRecent([]);
  await recent;
  const partlyReady = renderRetainedPopup();
  finishWeekly([]);
  await weekly;
  const empty = renderRetainedPopup();

  expect(label(loading)).not.toContain(en.popup.noActions);
  expect(label(traySection(loading, en.popup.recentActions))).toContain(en.common.loading);
  expect(label(traySection(loading, en.popup.weeklyStats))).toContain(en.dashboard.activityLoading);
  expect(label(traySection(partlyReady, en.popup.recentActions))).toContain(en.popup.noActions);
  expect(label(traySection(partlyReady, en.popup.weeklyStats))).toContain(en.dashboard.activityLoading);
  expect(label(traySection(empty, en.popup.weeklyStats))).toContain(en.dashboard.noOrganized);
});

it("disarms stale recent actions and retries only the failed Recent Actions read", async () => {
  await loadFolderList([invoices]);
  const initial = renderRecentActivity("C:/Fixture/Filed/arrival.txt");
  fixture.invoke.mockResolvedValueOnce([["Documents", 1]]);
  await useAppStore.getState().loadStats();
  fixture.invoke.mockRejectedValueOnce(new Error("Recent read unavailable"));
  await expect(useAppStore.getState().loadLogs()).rejects.toThrow("Recent read unavailable");
  const failed = renderRetainedPopup();
  fixture.invoke.mockClear();
  await recentButton(initial, en.popup.undo)?.props.onClick?.();
  await recentButton(initial, en.cleanup.openFolder)?.props.onClick?.();
  expect(fixture.invoke).not.toHaveBeenCalled();
  const section = traySection(failed, en.popup.recentActions);
  expect(label(section)).toContain("Could not load recent actions.");
  expect(label(section)).not.toContain("arrival.txt");
  expect(label(section)).not.toContain(en.popup.noActions);
  expect(label(traySection(failed, en.popup.weeklyStats))).toContain("Documents");
  expect(findAction(failed, "Organize Now")).toBeTypeOf("function");

  fixture.invoke.mockResolvedValueOnce([]);
  await findAction(section, en.common.retry)?.();
  expect(fixture.invoke.mock.calls).toEqual([["get_logs_cmd", { limit: 50 }]]);
  expect(label(traySection(renderRetainedPopup(), en.popup.recentActions))).toContain(en.popup.noActions);
});

it("withholds stale weekly percentages and retries weekly data without disturbing recent actions", async () => {
  renderRecentActivity("C:/Fixture/Filed/arrival.txt");
  fixture.invoke.mockResolvedValueOnce([["Documents", 1]]);
  await useAppStore.getState().loadStats();
  fixture.invoke.mockRejectedValueOnce(new Error("Weekly read unavailable"));
  await expect(useAppStore.getState().loadStats()).rejects.toThrow("Weekly read unavailable");
  const failed = renderRetainedPopup();
  const section = traySection(failed, en.popup.weeklyStats);
  expect(label(section)).toContain(en.dashboard.activityReadFailed);
  expect(label(section)).not.toContain("Documents");
  expect(recentButton(failed, en.popup.undo)).toBeDefined();

  fixture.invoke.mockClear().mockResolvedValueOnce([["Images", 1]]);
  await findAction(section, en.common.retry)?.();
  expect(fixture.invoke.mock.calls).toEqual([["get_stats_cmd"]]);
  expect(label(traySection(renderRetainedPopup(), en.popup.weeklyStats))).toContain("Images");
});

it("keeps the committed Undo receipt with one inline warning per failed read and blocks a second Undo", async () => {
  const popup = renderRecentActivity("C:/Fixture/Filed/arrival.txt");
  const undo = recentButton(popup, en.popup.undo)?.props.onClick;
  fixture.invoke.mockImplementation((command: string) => {
    if (command === "undo_action_cmd") return Promise.resolve({ status: "ok", restoredTo: "C:/Fixture/arrival.txt" });
    return Promise.reject(new Error("Post-Undo read unavailable"));
  });
  void undo?.();
  void undo?.();
  await vi.waitFor(() => expect(label(renderRetainedPopup())).toContain(en.undo.ok));
  const result = renderRetainedPopup();
  expect(fixture.invoke.mock.calls.filter(([command]) => command === "undo_action_cmd")).toHaveLength(1);
  expect(recentButton(result, en.popup.undo)).toBeUndefined();
  expect(recentButton(result, en.cleanup.openFolder)).toBeUndefined();
  expect(label(traySection(result, en.popup.recentActions))).toContain("Could not load recent actions.");
  expect(label(traySection(result, en.popup.weeklyStats))).toContain(en.dashboard.activityReadFailed);
  expect(findElement(result, (element) => typeof element.type === "function" && element.type.name === "ActionRefreshWarnings"))
    .toBeUndefined();
});

it.each([
  ["C:\\arrival.txt", "C:\\"],
  ["\\\\?\\C:\\arrival.txt", "\\\\?\\C:\\"],
  ["C:\\Fixture\\arrival.txt", "C:\\Fixture"],
])("opens the recorded parent folder for %s", async (destination, parent) => {
  const popup = renderRecentActivity(destination);
  await recentButton(popup, en.cleanup.openFolder)?.props.onClick?.();
  expect(fixture.invoke.mock.calls).toEqual([["open_folder_cmd", { path: parent }]]);
});

it("shows a failed activity-folder open until the next successful open", async () => {
  const popup = renderRecentActivity("C:/Fixture/arrival.txt");
  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    fixture.invoke.mockRejectedValueOnce(new Error("Fixture folder disappeared"));
    await recentButton(popup, en.cleanup.openFolder)?.props.onClick?.();
    const failed = renderRetainedPopup();
    expect(findElement(failed, (element) => element.props.role === "alert")).toBeDefined();
    fixture.invoke.mockResolvedValueOnce(undefined);
    await recentButton(failed, en.cleanup.openFolder)?.props.onClick?.();
    expect(findElement(renderRetainedPopup(), (element) => element.props.role === "alert")).toBeUndefined();
  } finally {
    reporter.mockRestore();
  }
});

it.each([null, ""])("withholds Undo for a verified row with no recorded destination (%s)", (destination) => {
  const popup = renderRecentActivity(destination);
  expect(recentButton(popup, en.cleanup.openFolder)).toBeUndefined();
  expect(recentButton(popup, en.popup.undo)).toBeUndefined();
});

it("keeps the recorded folder but withholds Undo for an unverified legacy move", () => {
  const popup = renderRecentActivity("C:/Fixture/arrival.txt", false);
  expect(recentButton(popup, en.cleanup.openFolder)).toBeDefined();
  expect(recentButton(popup, en.popup.undo)).toBeUndefined();
});

it("withholds Undo for a legacy removal or a completed restore", () => {
  expect(recentButton(renderRecentActivity("C:/Fixture/arrival.txt", true, "delete"), en.popup.undo)).toBeUndefined();
  expect(recentButton(renderRecentActivity("C:/Fixture/arrival.txt", true, "move", true), en.popup.undo)).toBeUndefined();
});

it.each(["move", "rename", "accept"])("sends Undo for a verified saved %s", async (action) => {
  const popup = renderRecentActivity("C:/Fixture/arrival.txt", true, action);
  const undo = recentButton(popup, en.popup.undo);
  expect(undo).toBeDefined();
  fixture.invoke.mockResolvedValueOnce({ status: "ok", restoredTo: "C:/Fixture/arrival.txt", message: null })
    .mockResolvedValue([]);
  await undo?.props.onClick?.();
  expect(fixture.invoke).toHaveBeenNthCalledWith(1, "undo_action_cmd", { id: 1 });
});

function pendingCheckbox(popup: ReactNode, filename: string) {
  const row = findElement(popup, (element) => element.type === "label" && label(element) === filename);
  const box = findElement(row, (element) => element.type === "input");
  if (!box) throw new Error(`Pending checkbox is missing for ${filename}`);
  return box.props;
}

async function publishPendingFiles(...names: string[]) {
  fixture.invoke.mockResolvedValueOnce(names.map((name) => [invoices.path, name]));
  await useAppStore.getState().getPendingFiles();
}

function reviewQueue(popup: ReactNode) {
  return findElement(popup, (element) => element.type === "button" && label(element).startsWith("Review queue ("));
}

function deferredPendingFiles() {
  let resolve!: (rows: [string, string][]) => void;
  let reject!: (error: Error) => void;
  const result = new Promise<[string, string][]>((yes, no) => { resolve = yes; reject = no; });
  return { result, resolve, reject };
}

it("distinguishes a first queue check from a successful empty list", async () => {
  await loadFolderList([invoices]);
  const first = deferredPendingFiles();
  fixture.invoke.mockReturnValueOnce(first.result);
  const read = useAppStore.getState().getPendingFiles();
  const checking = renderRetainedPopup();
  expect(label(checking)).toContain("Checking for waiting files…");
  expect(label(checking)).not.toContain("No files are waiting.");
  expect(reviewQueue(checking)).toBeUndefined();
  first.resolve([]);
  await read;
  const empty = renderRetainedPopup();
  expect(label(empty)).toContain("No files are waiting.");
  expect(label(empty)).not.toContain("Checking for waiting files…");
  expect(findAction(empty, "Organize Now")).toBeTypeOf("function");
});

it("forgets a selected pending path after an empty snapshot even if it returns before rendering", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("arrival.txt");
  pendingCheckbox(renderRetainedPopup(), "arrival.txt").onChange?.({ target: { checked: true } });
  expect(pendingCheckbox(renderRetainedPopup(), "arrival.txt").checked).toBe(true);

  await publishPendingFiles();
  await publishPendingFiles("arrival.txt", "chosen.txt");
  const returned = renderRetainedPopup();
  expect(pendingCheckbox(returned, "arrival.txt").checked).toBe(false);
  pendingCheckbox(returned, "chosen.txt").onChange?.({ target: { checked: true } });

  fixture.invoke.mockClear().mockResolvedValueOnce(undefined);
  const review = reviewQueue(renderRetainedPopup());
  expect(review?.props.disabled).toBe(false);
  await review?.props.onClick?.();
  expect(fixture.invoke.mock.calls).toEqual([["show_review_cmd", { paths: [`${invoices.path}/chosen.txt`] }]]);
});

it("keeps only continuously present selections across reorder, a new arrival, and a failed read", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("removed.txt", "retained.txt");
  const original = renderRetainedPopup();
  for (const name of ["removed.txt", "retained.txt"]) {
    pendingCheckbox(original, name).onChange?.({ target: { checked: true } });
  }
  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    fixture.invoke.mockRejectedValueOnce(new Error("Pending files unavailable"));
    await expect(useAppStore.getState().getPendingFiles()).resolves.toBeUndefined();
  } finally {
    reporter.mockRestore();
  }
  await publishPendingFiles("retained.txt", "new.txt");
  await publishPendingFiles("new.txt", "removed.txt", "retained.txt");
  const refreshed = renderRetainedPopup();
  expect(pendingCheckbox(refreshed, "retained.txt").checked).toBe(true);
  expect(pendingCheckbox(refreshed, "new.txt").checked).toBe(false);
  expect(pendingCheckbox(refreshed, "removed.txt").checked).toBe(false);
});

it("only reviews the selected arrival that stays visible across an exclusion update", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("protected.txt", "sibling.txt");
  const before = renderRetainedPopup();
  for (const name of ["protected.txt", "sibling.txt"]) {
    pendingCheckbox(before, name).onChange?.({ target: { checked: true } });
  }
  expect(reviewQueue(renderRetainedPopup())?.props.children).toContain(2);

  await publishPendingFiles("sibling.txt");
  const excluded = renderRetainedPopup();
  expect(label(excluded)).not.toContain("protected.txt");
  expect(pendingCheckbox(excluded, "sibling.txt").checked).toBe(true);
  expect(reviewQueue(excluded)?.props.children).toContain(1);

  await publishPendingFiles("protected.txt", "sibling.txt");
  const returned = renderRetainedPopup();
  expect(pendingCheckbox(returned, "protected.txt").checked).toBe(false);
  expect(pendingCheckbox(returned, "sibling.txt").checked).toBe(true);
  expect(reviewQueue(returned)?.props.children).toContain(1);
  fixture.invoke.mockClear().mockResolvedValueOnce(undefined);
  await reviewQueue(returned)?.props.onClick?.();
  expect(fixture.invoke.mock.calls).toEqual([["show_review_cmd", { paths: [invoices.path + "/sibling.txt"] }]]);
});

it("blocks an old Review queue click when the current queue read has failed", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("arrival.txt");
  pendingCheckbox(renderRetainedPopup(), "arrival.txt").onChange?.({ target: { checked: true } });
  const review = reviewQueue(renderRetainedPopup());
  expect(review?.props.disabled).toBe(false);
  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    fixture.invoke.mockRejectedValueOnce(new Error("Fixture queue read failed"));
    await useAppStore.getState().getPendingFiles();
    fixture.invoke.mockClear();
    await review?.props.onClick?.();
    expect(fixture.invoke).not.toHaveBeenCalled();
  } finally {
    reporter.mockRestore();
  }
});

it("shows a dedicated queue retry and only passes positively revalidated selected paths", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("left.txt", "retained.txt", "other.txt");
  const original = renderRetainedPopup();
  pendingCheckbox(original, "left.txt").onChange?.({ target: { checked: true } });
  pendingCheckbox(original, "retained.txt").onChange?.({ target: { checked: true } });
  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    fixture.invoke.mockRejectedValueOnce(new Error("Fixture queue read failed"));
    await useAppStore.getState().getPendingFiles();
    const failed = renderRetainedPopup();
    expect(label(findElement(failed, (element) => element.props.role === "alert"))).toContain("Could not check for waiting files.");
    expect(label(failed)).not.toContain("No files are waiting.");
    expect(label(failed)).not.toContain("retained.txt");
    expect(reviewQueue(failed)).toBeUndefined();
    expect(findAction(failed, "Organize Now")).toBeTypeOf("function");
    expect(findAction(failed, "Open Downloads")).toBeTypeOf("function");

    pendingCheckbox(original, "other.txt").onChange?.({ target: { checked: true } });
    fixture.invoke.mockClear().mockResolvedValueOnce([[invoices.path, "retained.txt"], [invoices.path, "other.txt"], [invoices.path, "new.txt"]]);
    const retry = findAction(failed, "Retry");
    expect(retry).toBeTypeOf("function");
    await retry?.();
    expect(fixture.invoke.mock.calls).toEqual([["get_pending_files_cmd"]]);

    const recovered = renderRetainedPopup();
    expect(label(recovered)).not.toContain("Could not check for waiting files.");
    expect(label(recovered)).not.toContain("left.txt");
    expect(pendingCheckbox(recovered, "retained.txt").checked).toBe(true);
    expect(pendingCheckbox(recovered, "other.txt").checked).toBe(false);
    expect(pendingCheckbox(recovered, "new.txt").checked).toBe(false);
    fixture.invoke.mockClear().mockResolvedValueOnce(undefined);
    await reviewQueue(recovered)?.props.onClick?.();
    expect(fixture.invoke.mock.calls).toEqual([["show_review_cmd", { paths: [`${invoices.path}/retained.txt`] }]]);
  } finally {
    reporter.mockRestore();
  }
});

it("keeps healthy selection during overlapping polls and ignores an older rejected read", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("retained.txt");
  pendingCheckbox(renderRetainedPopup(), "retained.txt").onChange?.({ target: { checked: true } });
  const old = deferredPendingFiles();
  const latest = deferredPendingFiles();
  fixture.invoke.mockReturnValueOnce(old.result).mockReturnValueOnce(latest.result);
  const oldRead = useAppStore.getState().getPendingFiles();
  const latestRead = useAppStore.getState().getPendingFiles();
  const pending = renderRetainedPopup();
  expect(reviewQueue(pending)?.props.disabled).toBe(false);
  expect(pendingCheckbox(pending, "retained.txt").checked).toBe(true);
  expect(label(pending)).not.toContain("Checking for waiting files…");
  latest.resolve([[invoices.path, "new.txt"], [invoices.path, "retained.txt"]]);
  await latestRead;

  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    old.reject(new Error("Older read failed"));
    await oldRead;
  } finally {
    reporter.mockRestore();
  }
  const ready = renderRetainedPopup();
  expect(label(ready)).not.toContain("Could not check for waiting files.");
  expect(pendingCheckbox(ready, "retained.txt").checked).toBe(true);
  expect(pendingCheckbox(ready, "new.txt").checked).toBe(false);
});

it("keeps a newer queue error after an older read finally succeeds", async () => {
  await loadFolderList([invoices]);
  await publishPendingFiles("last-known.txt");
  const older = deferredPendingFiles();
  fixture.invoke.mockReturnValueOnce(older.result);
  const oldRead = useAppStore.getState().getPendingFiles();
  const reporter = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    fixture.invoke.mockRejectedValueOnce(new Error("Current read failed"));
    await useAppStore.getState().getPendingFiles();
    older.resolve([[invoices.path, "superseded.txt"]]);
    await oldRead;
  } finally {
    reporter.mockRestore();
  }
  const failed = renderRetainedPopup();
  expect(label(failed)).toContain("Could not check for waiting files.");
  expect(label(failed)).not.toContain("superseded.txt");
  expect(label(failed)).not.toContain("last-known.txt");
});

const personalInbox: WatchedFolder = { ...invoices, id: 2, path: "C:\\Fixture\\Personal\\Inbox" };
const teamInbox: WatchedFolder = { ...invoices, id: 3, path: "D:\\Fixture\\Team\\Inbox" };
const personalPath = (name: string) => personalInbox.path + "\\" + name;
const teamPath = (name: string) => teamInbox.path + "\\" + name;

async function publishInboxFiles(personal: string, team?: string) {
  const pending = [[personalInbox.path, personal], [personalInbox.path, "unique.txt"]];
  if (team) pending.push([teamInbox.path, team]);
  fixture.invoke.mockResolvedValueOnce(pending);
  await useAppStore.getState().getPendingFiles();
}

function pendingRow(popup: ReactNode, path: string) {
  return findElement(popup, (element) => element.type === "label" && element.props.title === path);
}

it.each([
  ["report.pdf", "report.pdf"],
  ["photo.JPG", "PHOTO.jpg"],
])("shows the parent in the checkbox label for %s and %s", async (personal, team) => {
  await loadFolderList([personalInbox, teamInbox]);
  await publishInboxFiles(personal, team);
  const popup = renderRetainedPopup();
  const rows = [pendingRow(popup, personalPath(personal)), pendingRow(popup, teamPath(team)), pendingRow(popup, personalPath("unique.txt"))];
  for (const row of rows) expect(findElement(row, (element) => element.type === "input")).toBeDefined();
  expect(rows.map(label)).toEqual([personal + " " + personalInbox.path, team + " " + teamInbox.path, "unique.txt"]);
});

it("keeps exact selection when a same-named arrival disappears", async () => {
  await loadFolderList([personalInbox, teamInbox]);
  await publishInboxFiles("report.pdf", "report.pdf");
  const popup = renderRetainedPopup();
  const personal = findElement(pendingRow(popup, personalPath("report.pdf")), (element) => element.type === "input");
  const team = findElement(pendingRow(popup, teamPath("report.pdf")), (element) => element.type === "input");
  expect(personal).toBeDefined();
  expect(team).toBeDefined();
  personal?.props.onChange?.({ target: { checked: true } });
  const refreshed = renderRetainedPopup();
  expect(findElement(pendingRow(refreshed, personalPath("report.pdf")), (element) => element.type === "input")?.props.checked).toBe(true);
  expect(findElement(pendingRow(refreshed, teamPath("report.pdf")), (element) => element.type === "input")?.props.checked).toBe(false);

  await publishInboxFiles("report.pdf");
  const remaining = renderRetainedPopup();
  expect(pendingCheckbox(remaining, "report.pdf").checked).toBe(true);
  fixture.invoke.mockClear().mockResolvedValueOnce(undefined);
  const review = findElement(remaining, (element) => element.type === "button" && label(element).startsWith("Review queue ("))?.props.onClick;
  expect(review).toBeTypeOf("function");
  await review?.();
  expect(fixture.invoke.mock.calls).toEqual([["show_review_cmd", { paths: [personalPath("report.pdf")] }]]);
});

it("takes a confirmed zero-folder popup directly to Watched Folders", async () => {
  await loadFolderList([]);
  fixture.invoke.mockResolvedValueOnce(undefined);
  const popup = Popup();
  expect(label(popup)).toContain("Choose a folder to organize.");
  expect(findAction(popup, "Organize Now")).toBeUndefined();
  const setup = findAction(popup, "Add a watched folder");
  expect(setup).toBeTypeOf("function");
  await setup?.();
  expect(fixture.invoke.mock.calls).toEqual([["show_folders_cmd"]]);
});

it("organizes after a watched folder is configured", async () => {
  await loadFolderList([invoices]);
  fixture.invoke.mockResolvedValueOnce(undefined);
  const popup = Popup();
  expect(findAction(popup, "Add a watched folder")).toBeUndefined();
  const organize = findAction(popup, "Organize Now");
  expect(organize).toBeTypeOf("function");
  await organize?.();
  expect(fixture.invoke.mock.calls).toEqual([["show_review_cmd", { paths: [] }]]);
});

it("waits for a folder read and offers Retry when the read fails", async () => {
  await publishPendingFiles("stale.pdf");
  const loading = Popup();
  expect(label(loading)).toContain("Loading…");
  expect(findAction(loading, "Add a watched folder")).toBeUndefined();
  expect(findAction(loading, "Organize Now")).toBeUndefined();
  expect(label(loading)).not.toContain("stale.pdf");

  fixture.invoke.mockRejectedValueOnce(new Error("Fixture unavailable"));
  await expect(useAppStore.getState().loadFolders()).rejects.toThrow("Fixture unavailable");
  const failed = Popup();
  expect(label(failed)).toContain("Could not load watched folders");
  expect(findAction(failed, "Organize Now")).toBeUndefined();
  expect(findAction(failed, "Add a watched folder")).toBeUndefined();
  expect(label(failed)).not.toContain("stale.pdf");

  fixture.invoke.mockResolvedValueOnce([invoices]);
  const retry = findAction(failed, "Retry");
  expect(retry).toBeTypeOf("function");
  await retry?.();
  expect(findAction(Popup(), "Organize Now")).toBeTypeOf("function");
  expect(label(Popup())).toContain("stale.pdf");
});

it("opens system Downloads even if the first watched folder is different", async () => {
  await loadFolderList([invoices]);
  fixture.invoke.mockResolvedValueOnce(downloads).mockResolvedValueOnce(undefined);
  const openDownloads = findAction(Popup(), "Open Downloads");
  expect(openDownloads).toBeTypeOf("function");
  await openDownloads?.();
  expect(fixture.invoke.mock.calls).toEqual([
    ["get_downloads_folder"],
    ["open_folder_cmd", { path: downloads }],
  ]);
});

it("reports an unavailable Downloads folder without opening a watched folder", async () => {
  await loadFolderList([invoices]);
  fixture.invoke.mockRejectedValueOnce(new Error("System Downloads is unavailable"));
  const openDownloads = findAction(Popup(), "Open Downloads");

  await expect(openDownloads?.()).resolves.toBeUndefined();
  expect(fixture.invoke.mock.calls).toEqual([["get_downloads_folder"]]);
  expect(fixture.setState).toHaveBeenCalledWith("Error: System Downloads is unavailable");
});
