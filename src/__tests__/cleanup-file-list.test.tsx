import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import FileListTab from "../components/cleanup/FileListTab";
import DuplicatesTab from "../components/cleanup/DuplicatesTab";
import EmptyDirsTab from "../components/cleanup/EmptyDirsTab";
import { useCleanupStore } from "../store/useCleanupStore";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  return {
    useCleanupStore: Object.assign(() => actual.useCleanupStore.getState(), actual.useCleanupStore),
  };
});

beforeEach(() => {
  ipc.invoke.mockReset();
  useCleanupStore.getState().invalidateCandidates();
});

function renderFileList(kind: "large" | "stale", defaultThreshold: number) {
  return renderToStaticMarkup(
    <FileListTab
      kind={kind}
      defaultThreshold={defaultThreshold}
      findLabel="Find"
      hint="Find cleanup candidates"
      emptyKey="empty"
      operationKey="review"
    />,
  );
}

it("distinguishes an unsearched or interrupted duplicate search from a completed empty result", async () => {
  const renderDuplicates = () => renderToStaticMarkup(<DuplicatesTab />);
  expect(renderDuplicates()).toContain("cleanup.notSearched");
  ipc.invoke.mockResolvedValueOnce([]);
  await useCleanupStore.getState().findDuplicates();
  expect(renderDuplicates()).toContain("cleanup.noDuplicates");
  useCleanupStore.getState().invalidateFileCandidates();
  expect(renderDuplicates()).toContain("cleanup.notSearched");
  expect(renderDuplicates()).not.toContain("cleanup.noDuplicates");
});

it("shows the current duplicate search phase and ignores late progress", async () => {
  const renderDuplicates = () => renderToStaticMarkup(<DuplicatesTab />);
  let finish!: (result: unknown[]) => void;
  ipc.invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
  const pending = useCleanupStore.getState().findDuplicates();
  const { searchId } = ipc.invoke.mock.calls[0][1];
  const report = useCleanupStore.getState().reportDuplicateProgress;
  expect(renderDuplicates()).toContain("cleanup.duplicateStarting");
  report(searchId, { stage: "inspect", checked: 5, total: 10 });
  expect(renderDuplicates()).toContain("cleanup.duplicateInspecting");
  report(searchId, { stage: "compare", checked: 4, total: 8 });
  report(searchId, { stage: "inspect", checked: 10, total: 10 });
  report(searchId, { stage: "compare", checked: 2, total: 8 });
  expect(useCleanupStore.getState().duplicateProgress).toEqual({ stage: "compare", checked: 4, total: 8 });
  expect(renderDuplicates()).toContain("cleanup.duplicateComparing");
  finish([]);
  await pending;
  report(searchId, { stage: "compare", checked: 8, total: 8 });
  expect(renderDuplicates()).toContain("cleanup.noDuplicates");
  expect(useCleanupStore.getState().duplicateProgress).toBeNull();
});

it("explains when a new inventory interrupts an active duplicate search", async () => {
  let finish!: (result: unknown[]) => void;
  ipc.invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
  const pending = useCleanupStore.getState().findDuplicates();
  useCleanupStore.getState().invalidateFileCandidates();
  finish([]);
  await pending;
  const result = renderToStaticMarkup(<DuplicatesTab />);
  expect(result).toContain("cleanup.duplicateInterrupted");
  expect(result).not.toContain("cleanup.noDuplicates");
});

it("reports locked files until a complete duplicate search succeeds", async () => {
  const renderDuplicates = () => renderToStaticMarkup(<DuplicatesTab />);
  ipc.invoke.mockResolvedValueOnce({ candidates: [], unavailableRoots: ["D:/Offline"], uncheckedFiles: 1 });
  await useCleanupStore.getState().findDuplicates();
  expect(renderDuplicates()).toContain("cleanup.duplicateLocked");
  expect(renderDuplicates()).toContain("D:/Offline");
  expect(renderDuplicates()).not.toContain("cleanup.noMatchesInAvailableFolders");
  expect(renderDuplicates()).not.toContain("cleanup.noDuplicates");
  ipc.invoke.mockResolvedValueOnce([]);
  await useCleanupStore.getState().findDuplicates();
  expect(renderDuplicates()).toContain("cleanup.noDuplicates");
  expect(renderDuplicates()).not.toContain("cleanup.duplicateLocked");
});

it("restores the successful large-file threshold when the tab mounts again", async () => {
  ipc.invoke.mockResolvedValueOnce([{ path: "fixture-large.bin", size: 20 * 1_048_576, mtime: 1 }]);
  await useCleanupStore.getState().findLargeFiles(10 * 1_048_576);

  const html = renderFileList("large", 100);
  expect(html).toMatch(/<input[^>]*type="number"[^>]*value="10"/);
  expect(html).toContain("fixture-large.bin");
});

it("restores the searched age even when no stale files matched", async () => {
  ipc.invoke.mockResolvedValueOnce([]);
  await useCleanupStore.getState().findStaleFiles(45);

  const html = renderFileList("stale", 365);
  expect(html).toMatch(/<input[^>]*type="number"[^>]*value="45"/);
  expect(html).toContain("empty");
});

it("qualifies empty results when a watched folder is unavailable", async () => {
  ipc.invoke.mockResolvedValueOnce({ candidates: [], unavailableRoots: ["D:/Disconnected"] });
  await useCleanupStore.getState().findStaleFiles(45);

  const html = renderFileList("stale", 365);
  expect(html).toContain("cleanup.noMatchesInAvailableFolders");
  expect(html).toContain("cleanup.unavailableFolders");
  expect(html).toContain("D:/Disconnected");
  expect(html).not.toContain(">empty<");
});

it("shows partial candidate results without carrying an omission into a different finder", async () => {
  const group = { hash: "fixture-hash", files: [
    { path: "C:/Watched/keeper.txt", size: 5000, mtime: 1 },
    { path: "C:/Watched/copy.txt", size: 5000, mtime: 1 },
  ] };
  ipc.invoke.mockResolvedValueOnce({ candidates: [group], unavailableRoots: ["D:/Disconnected"] });
  await useCleanupStore.getState().findDuplicates();
  ipc.invoke.mockResolvedValueOnce(["C:/Watched/empty"]);
  await useCleanupStore.getState().findEmptyDirs();

  const duplicates = renderToStaticMarkup(<DuplicatesTab />);
  expect(duplicates).toContain("D:/Disconnected");
  expect(duplicates).toContain("copy.txt");
  expect(duplicates).not.toContain("cleanup.noMatchesInAvailableFolders");
  const directories = renderToStaticMarkup(<EmptyDirsTab />);
  expect(directories).toContain("C:/Watched/empty");
  expect(directories).not.toContain("D:/Disconnected");
});

it("shows no empty-directory success state when the finder could not run", async () => {
  ipc.invoke.mockRejectedValueOnce("The enabled watched folders are unavailable.");
  await useCleanupStore.getState().findEmptyDirs();

  const html = renderToStaticMarkup(<EmptyDirsTab />);
  expect(html).toContain("The enabled watched folders are unavailable.");
  expect(html).not.toContain("cleanup.noEmptyDirs");
});
