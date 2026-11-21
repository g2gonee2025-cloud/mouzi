import { beforeEach, expect, it, vi } from "vitest";
import { isValidElement, type ReactNode, type SetStateAction } from "react";
import HistoryView from "../components/HistoryView";
import HistoryPanel from "../components/cleanup/HistoryPanel";
import type { OrganizationLog } from "../store/useAppStore";
import { useCleanupStore, type CleanupLogEntry } from "../store/useCleanupStore";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  state: [] as unknown[],
  cursor: 0,
  effects: [] as Array<() => void | (() => void)>,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("react", async (importOriginal) => ({
  ...await importOriginal<typeof import("react")>(),
  useState: <T,>(initial: T) => {
    const slot = mocks.cursor++;
    if (!(slot in mocks.state)) mocks.state[slot] = initial;
    const set = (next: SetStateAction<T>) => {
      mocks.state[slot] = typeof next === "function"
        ? (next as (previous: T) => T)(mocks.state[slot] as T)
        : next;
    };
    return [mocks.state[slot], set];
  },
  useEffect: (effect: () => void | (() => void)) => { mocks.effects.push(effect); },
  useCallback: (callback: unknown) => callback,
  useRef: (current: unknown) => ({ current }),
}));
vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  return { ...actual, useCleanupStore: Object.assign(actual.useCleanupStore.getState, actual.useCleanupStore) };
});

interface FolderButton {
  children?: ReactNode;
  "aria-label"?: string;
  onClick?: () => void | Promise<void>;
}

function buttonAction(node: ReactNode, key: string): FolderButton["onClick"] {
  if (Array.isArray(node)) return node.map((child) => buttonAction(child, key)).find(Boolean);
  if (!isValidElement<FolderButton>(node)) return;
  const { children, onClick, "aria-label": label } = node.props;
  if (node.type === "button" && (children === key || label === key)) {
    return onClick;
  }
  return buttonAction(children, key);
}

function render(view: () => ReactNode) {
  mocks.cursor = 0;
  return view();
}

async function clickSavedFolder(view: () => ReactNode) {
  render(view);
  const cleanups = mocks.effects.splice(0).map((effect) => effect());
  try {
    const click = await vi.waitFor(() => {
      const action = buttonAction(render(view), "cleanup.openFolder");
      if (!action) throw new Error("Saved history row did not show an Open folder button");
      return action;
    });
    await click();
  } finally {
    cleanups.forEach((cleanup) => cleanup?.());
  }
}

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.listen.mockReset().mockResolvedValue(vi.fn());
  mocks.state.length = 0;
  mocks.effects.length = 0;
  useCleanupStore.setState(useCleanupStore.getInitialState(), true);
});

it("opens Cleanup History and shows older entries after the first 100", async () => {
  const row = (id: number): CleanupLogEntry => ({
    id, timestamp, path: "C:/Watched/record-" + id + ".bin", prevPath: null, dest: null,
    action: "trash_large", status: "ok", undoable: false,
  });
  const first = Array.from({ length: 100 }, (_, index) => row(103 - index));
  const cursor = { timestamp, id: 4, throughId: 103 };
  mocks.invoke.mockImplementation(async (command, args) => {
    if (command === "get_cleanup_logs_cmd") return first;
    if (command === "get_cleanup_history_page_cmd") {
      return args.cursor ? { entries: [row(3), row(2), row(1)], nextCursor: null } : { entries: first, nextCursor: cursor };
    }
    throw new Error("Unexpected command " + command);
  });

  render(HistoryPanel);
  mocks.effects.splice(0).forEach((effect) => effect());
  await vi.waitFor(() => expect(useCleanupStore.getState().history).toHaveLength(100));

  const older = buttonAction(render(HistoryPanel), "beta.more");
  expect(older).toBeTypeOf("function");
  await older?.();
  expect(useCleanupStore.getState().history?.map(({ id }) => id)).toEqual(
    Array.from({ length: 103 }, (_, index) => 103 - index),
  );
  expect(mocks.invoke).toHaveBeenLastCalledWith("get_cleanup_history_page_cmd", { limit: 100, cursor });
  expect(buttonAction(render(HistoryPanel), "beta.more")).toBeUndefined();
});

it("retries the same history cursor and lets refresh supersede an in-flight older page", async () => {
  const saved = (id: number): CleanupLogEntry => ({
    id, timestamp, path: "C:/Watched/history-" + id, prevPath: null, dest: null,
    action: "trash_large", status: "ok", undoable: false,
  });
  const initial = { entries: [saved(2)], nextCursor: { timestamp, id: 2, throughId: 2 } };
  const refreshed = { entries: [saved(3), saved(2)], nextCursor: null };
  mocks.invoke.mockResolvedValueOnce(initial).mockRejectedValueOnce(new Error("Temporary history read failure"));
  await useCleanupStore.getState().loadHistory();
  await useCleanupStore.getState().loadOlderHistory();
  expect(useCleanupStore.getState()).toMatchObject({
    history: initial.entries, historyCursor: initial.nextCursor, historyLoading: false,
    historyError: "Error: Temporary history read failure",
  });
  expect(buttonAction(render(HistoryPanel), "beta.more")).toBeTypeOf("function");

  let resolveOlder!: (page: { entries: CleanupLogEntry[]; nextCursor: null }) => void;
  const older = new Promise<{ entries: CleanupLogEntry[]; nextCursor: null }>((resolve) => { resolveOlder = resolve; });
  mocks.invoke.mockReturnValueOnce(older).mockResolvedValueOnce(refreshed);
  const retry = useCleanupStore.getState().loadOlderHistory();
  await useCleanupStore.getState().loadOlderHistory();
  expect(mocks.invoke).toHaveBeenCalledTimes(3);
  expect(mocks.invoke).toHaveBeenNthCalledWith(3, "get_cleanup_history_page_cmd", { limit: 100, cursor: initial.nextCursor });

  await buttonAction(render(HistoryPanel), "cleanup.refresh")?.();
  expect(mocks.invoke).toHaveBeenNthCalledWith(4, "get_cleanup_history_page_cmd", { limit: 100, cursor: null });
  resolveOlder({ entries: [saved(1)], nextCursor: null });
  await retry;
  expect(useCleanupStore.getState()).toMatchObject({
    history: refreshed.entries, historyCursor: null, historyLoading: false, historyError: null,
  });
  expect(buttonAction(render(HistoryPanel), "beta.more")).toBeUndefined();
});

const timestamp = "2026-11-20T12:00:00Z";

it("opens the containing drive root from a canonical destination saved in History", async () => {
  const saved: OrganizationLog = {
    id: 1, timestamp, source_path: "C:\\Watched\\receipt.pdf", destination_path: "\\\\?\\C:\\receipt.pdf",
    action: "move", file_name: "receipt.pdf", file_type: "PDFs", undone: false,
    run_id: "1", trigger: "manual", file_extension: "pdf", file_size: 8,
    fingerprint: null, undo_verified: true,
  };
  mocks.invoke.mockResolvedValueOnce([saved]).mockResolvedValueOnce(undefined);

  await clickSavedFolder(HistoryView);

  expect(mocks.invoke).toHaveBeenNthCalledWith(1, "history_cmd", {
    filter: { query: "", rule: "", extension: "", from: "", to: "", run_id: "" },
  });
  expect(mocks.invoke).toHaveBeenNthCalledWith(2, "open_folder_cmd", { path: "\\\\?\\C:\\" });
});

it.each([
  ["ordinary Windows drive", "C:\\removed.pdf", "C:\\"],
  ["canonical Windows drive", "\\\\?\\C:\\removed.pdf", "\\\\?\\C:\\"],
  ["POSIX root", "/removed.pdf", "/"],
])("opens the %s containing a saved Cleanup History source", async (_case, path, parent) => {
  const saved: CleanupLogEntry = {
    id: 1, timestamp, path, prevPath: null, dest: null,
    action: "trash_large", status: "ok", undoable: false, message: null,
  };
  mocks.invoke.mockResolvedValueOnce({ entries: [saved], nextCursor: null }).mockResolvedValueOnce(undefined);

  await clickSavedFolder(HistoryPanel);

  expect(mocks.invoke).toHaveBeenNthCalledWith(1, "get_cleanup_history_page_cmd", { limit: 100, cursor: null });
  expect(mocks.invoke).toHaveBeenNthCalledWith(2, "open_folder_cmd", { path: parent });
});
