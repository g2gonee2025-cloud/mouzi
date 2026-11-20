import { beforeEach, expect, it, vi } from "vitest";
import { isValidElement, type ReactNode } from "react";
import Popup from "../components/Popup";
import { useAppStore, type WatchedFolder } from "../store/useAppStore";
import en from "../i18n/locales/en.json";

const fixture = vi.hoisted(() => ({ invoke: vi.fn(), setState: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: fixture.invoke }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useEffect: () => {},
  useState: (value: unknown) => [value, fixture.setState],
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
}

function label(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(label).join("");
  if (typeof node === "string") return node;
  return isValidElement<ElementProps>(node) ? label(node.props.children) : "";
}

function findAction(node: ReactNode, name: string): ElementProps["onClick"] {
  if (Array.isArray(node)) return node.map((child) => findAction(child, name)).find(Boolean);
  if (!isValidElement<ElementProps>(node)) return;
  if (node.type === "button" && label(node) === name) return node.props.onClick;
  return findAction(node.props.children, name);
}

const invoices: WatchedFolder = { id: 1, path: "C:/Fixture/Invoices", mode: "manual", enabled: true };
const downloads = "C:/Fixture/Downloads";

beforeEach(() => {
  fixture.invoke.mockReset();
  fixture.setState.mockClear();
  useAppStore.setState(useAppStore.getInitialState(), true);
});

async function loadFolderList(folders: WatchedFolder[]) {
  fixture.invoke.mockResolvedValueOnce(folders);
  await useAppStore.getState().loadFolders();
  fixture.invoke.mockClear();
}

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
