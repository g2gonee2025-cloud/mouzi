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

function renderRecentActivity(destination: string | null) {
  useAppStore.setState({ logs: [{
    id: 1, timestamp: "2026-11-20", source_path: "C:/Fixture/arrival.txt",
    destination_path: destination, action: "move", file_name: "arrival.txt", file_type: "text", undone: false,
  }] });
  return renderRetainedPopup();
}

function recentButton(popup: ReactNode, title: string) {
  return findElement(popup, (element) => element.type === "button" && element.props.title === title);
}

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

it("keeps Undo available for recent activity without a destination", () => {
  const popup = renderRecentActivity(null);
  expect(recentButton(popup, en.cleanup.openFolder)).toBeUndefined();
  expect(recentButton(popup, en.popup.undo)).toBeDefined();
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
  const review = findElement(renderRetainedPopup(), (element) => element.type === "button" && label(element).startsWith("Review queue ("));
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
  expect(pendingCheckbox(renderRetainedPopup(), "retained.txt").checked).toBe(true);
  expect(pendingCheckbox(renderRetainedPopup(), "removed.txt").checked).toBe(true);

  await publishPendingFiles("retained.txt", "new.txt");
  await publishPendingFiles("new.txt", "removed.txt", "retained.txt");
  const refreshed = renderRetainedPopup();
  expect(pendingCheckbox(refreshed, "retained.txt").checked).toBe(true);
  expect(pendingCheckbox(refreshed, "new.txt").checked).toBe(false);
  expect(pendingCheckbox(refreshed, "removed.txt").checked).toBe(false);
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
  useAppStore.setState({ pendingFiles: [["C:/Fixture/Invoices", "stale.pdf"]] });
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
