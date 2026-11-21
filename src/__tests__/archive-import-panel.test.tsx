import { Children, isValidElement, type ReactNode } from "react";
import { expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import ArchiveImportPanel from "../components/ArchiveImportPanel";
import Settings from "../components/Settings";
import { useAppStore, type ArchiveImportState } from "../store/useAppStore";
import en from "../i18n/locales/en.json";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

vi.mock("../store/useAppStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useAppStore")>();
  const store = actual.useAppStore;
  // The server renderer normally reads Zustand's initial snapshot. Use the
  // current snapshot here, as newly mounted workspace components do.
  const useCurrentStore = (select?: (state: ReturnType<typeof store.getState>) => unknown) => {
    const state = store.getState();
    return select ? select(state) : state;
  };
  return { ...actual, useAppStore: Object.assign(useCurrentStore, store) };
});

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values: Record<string, string | number> = {}) => {
      const message = key.startsWith("settings.archive.")
        ? en.settings.archive[key.slice("settings.archive.".length) as keyof typeof en.settings.archive] : undefined;
      const localized = message ?? (key === "common.cancel" ? en.common.cancel : key);
      return Object.entries(values).reduce((text, [name, value]) => text.replace("{{" + name + "}}", String(value)), localized);
    },
  }),
}));

function renderSettings(archiveImportState: ArchiveImportState) {
  const previous = useAppStore.getState();
  useAppStore.setState({ archiveImportState });
  try {
    // Separate mounts exercise what a replacement Settings route reads from the store.
    return renderToStaticMarkup(<Settings />);
  } finally {
    useAppStore.setState(previous, true);
  }
}

const previousReceipt = {
  sourceName: "first.zip",
  receipt: { extractedCount: 2, sortedCount: 1, stagingPath: "C:/Archive Recovery/first", issue: null },
};

const noStaging: ArchiveImportState = {
  revision: 10, jobId: 7, sourceName: "notes.txt.gz", status: "failed", phase: null,
  bytes: 0, files: 0, cancelRequested: false, error: "Unsupported archive format", result: null,
  previousReceipt,
};

it("identifies the failed archive separately from the previous recoverable import on reopening", () => {
  const html = renderSettings(noStaging);
  const alert = html.match(/<p role="alert"[^>]*>(.*?)<\/p>/s)?.[1];
  expect(alert).toContain("notes.txt.gz");
  expect(alert).toContain("Unsupported archive format");
  expect(alert).not.toContain("first.zip");
  expect(html).toContain("Previous import: first.zip");
  expect(html).toContain("Extracted 2 file(s) and organized 1 file(s).");
  expect(html).toContain("Open import folder");
});

it("offers no recovery folder if the first archive failed before staging", () => {
  const html = renderSettings({ ...noStaging, previousReceipt: null });
  expect(html).toContain("notes.txt.gz");
  expect(html).toContain("Unsupported archive format");
  expect(html).not.toContain("Previous import");
  expect(html).not.toContain("Open import folder");
});

it("a reopened Settings page shows the running job and a Cancel action", () => {
  const html = renderSettings({
    revision: 2, status: "running", jobId: 1, phase: "extracting",
    sourceName: "running.zip", previousReceipt: null,
    bytes: 2048, files: 3, cancelRequested: false, result: null, error: null,
  });
  expect(html).toContain("Extracted 3 files");
  expect(html).toContain(">Cancel</button>");
  expect(html).toContain("Importing...");
});

it("shows the sorting phase and disables repeat cancellation after the request", () => {
  const html = renderSettings({
    revision: 3, status: "running", jobId: 1, phase: "sorting",
    sourceName: "sorting.zip", previousReceipt: null,
    bytes: 2048, files: 3, cancelRequested: true, result: null, error: null,
  });
  expect(html).toContain("Organizing extracted files…");
  expect(html).toMatch(/<button type="button" disabled=""[^>]*>Cancelling…<\/button>/);
});

it("a reopened Settings page retains partial counts and recovery action", () => {
  const html = renderSettings({
    revision: 3, status: "completed", jobId: 1, phase: null,
    sourceName: "partial.zip", previousReceipt: null,
    bytes: 2048, files: 2, cancelRequested: true,
    result: { extractedCount: 2, sortedCount: 1, stagingPath: "C:/Archive Recovery", issue: "Fixture cancellation" },
    error: null,
  });
  expect(html).toContain("Extracted 2 file(s) and organized 1 file(s).");
  expect(html).toContain("Fixture cancellation");
  expect(html).toContain("Open import folder");
});

it("preserves a recovery action without presenting unknown counts as zero", () => {
  const html = renderSettings({
    revision: 4, status: "completed", jobId: 2, phase: null,
    sourceName: "recovery.zip", previousReceipt: null,
    bytes: 0, files: 0, cancelRequested: false,
    result: { extractedCount: null, sortedCount: 0, stagingPath: "C:/Archive Recovery", issue: "Fixture unreadable" },
    error: null,
  });
  expect(html).not.toContain("Extracted 0 file(s)");
  expect(html).toContain("Organized 0 file(s).");
  expect(html).toContain("Fixture unreadable");
  expect(html).toContain("Open import folder");
});

function openFolderAction(tree: ReactNode): (() => void) | undefined {
  for (const node of Children.toArray(tree)) {
    if (!isValidElement<{ children?: ReactNode; onClick?: () => void }>(node)) continue;
    if (node.type === "button" && Children.toArray(node.props.children).includes(en.settings.archive.openImportFolder)) {
      return node.props.onClick;
    }
    const nested = openFolderAction(node.props.children);
    if (nested) return nested;
  }
}

it.each(["current", "previous"])("requires the exact directory when opening a %s archive import receipt", (which) => {
  const previous = useAppStore.getState();
  const completed: ArchiveImportState = {
    ...noStaging, revision: 14, jobId: 11, sourceName: "third.zip", status: "completed", files: 3,
    result: { extractedCount: 3, sortedCount: 2, stagingPath: "C:/Archive Recovery/third", issue: null },
    error: null, previousReceipt: null,
  };
  const state = which === "current" ? completed : noStaging;
  const stagingPath = which === "current" ? "C:/Archive Recovery/third" : "C:/Archive Recovery/first";
  useAppStore.setState({ archiveImportState: state });
  let open: (() => void) | undefined;
  function CapturePanel() {
    const panel = ArchiveImportPanel();
    open = openFolderAction(panel);
    return panel;
  }

  invoke.mockResolvedValueOnce(undefined);
  try {
    const html = renderToStaticMarkup(<CapturePanel />);
    if (which === "current") {
      expect(html).toContain("third.zip");
      expect(html).not.toContain("first.zip");
      expect(html).not.toContain("Unsupported archive format");
    }
    expect(open).toBeTypeOf("function");
    open!();
    expect(invoke).toHaveBeenCalledExactlyOnceWith("open_folder_cmd", { path: stagingPath, directoryOnly: true });
  } finally {
    invoke.mockReset();
    useAppStore.setState(previous, true);
  }
});
