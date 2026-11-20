import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, expect, it, vi } from "vitest";
import FileListTab from "../components/cleanup/FileListTab";
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
