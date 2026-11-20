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

function folderAction(node: ReactNode): FolderButton["onClick"] {
  if (Array.isArray(node)) return node.map(folderAction).find(Boolean);
  if (!isValidElement<FolderButton>(node)) return;
  const { children, onClick, "aria-label": label } = node.props;
  if (node.type === "button" && (children === "cleanup.openFolder" || label === "cleanup.openFolder")) {
    return onClick;
  }
  return folderAction(children);
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
      const action = folderAction(render(view));
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
  mocks.invoke.mockResolvedValueOnce([saved]).mockResolvedValueOnce(undefined);

  await clickSavedFolder(HistoryPanel);

  expect(mocks.invoke).toHaveBeenNthCalledWith(1, "get_cleanup_logs_cmd", { limit: 100 });
  expect(mocks.invoke).toHaveBeenNthCalledWith(2, "open_folder_cmd", { path: parent });
});
