import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import DuplicatesTab from "../components/cleanup/DuplicatesTab";
import EmptyDirsTab from "../components/cleanup/EmptyDirsTab";
import FileListTab from "../components/cleanup/FileListTab";
import { initI18n } from "../i18n";
import en from "../i18n/locales/en.json";
import { useCleanupStore } from "../store/useCleanupStore";

vi.mock("../store/useCleanupStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useCleanupStore")>();
  return {
    ...actual,
    useCleanupStore: Object.assign(() => actual.useCleanupStore.getState(), actual.useCleanupStore),
  };
});

beforeAll(() => initI18n("en"));
afterEach(() => useCleanupStore.setState(useCleanupStore.getInitialState()));

const files = ["C:/inbox/report.pdf", "C:/archive/report.pdf"].map((path) => ({
  path,
  size: 1024,
  mtime: 1000,
}));

describe("cleanup selection", () => {
  it("names duplicate keepers by path and exposes which copy is kept", () => {
    useCleanupStore.setState({ duplicates: [{ hash: "same-content", files }] });
    const html = renderToStaticMarkup(<DuplicatesTab />);
    const buttons = html.match(/<button\b[^>]*aria-pressed="[^"]+"[^>]*>/g) ?? [];
    expect(buttons).toHaveLength(2);
    expect(buttons[0]).toContain(`aria-label="Keep ${files[0].path}"`);
    expect(buttons[0]).toContain('aria-pressed="true"');
    expect(buttons[1]).toContain(`aria-label="Keep ${files[1].path}"`);
    expect(buttons[1]).toContain('aria-pressed="false"');
    expect(html).toContain(">C:/inbox</span>");
    expect(html).toContain(">C:/archive</span>");
  });

  it.each([
    { kind: "large", label: en.cleanup.minBytes },
    { kind: "stale", label: en.cleanup.staleDays },
  ] as const)("names the $kind threshold and file checkboxes", ({ kind, label }) => {
    useCleanupStore.setState({ largeFiles: files, staleFiles: files });
    const html = renderToStaticMarkup(
      <FileListTab
        kind={kind}
        defaultThreshold={100}
        findLabel="Find files"
        hint="Review files"
        emptyKey="cleanup.noLargeFiles"
        operationKey="confirm.op.trashLarge"
      />,
    );
    const input = html.match(/<input\b[^>]*type="number"[^>]*>/)?.[0];
    const id = input?.match(/\bid="([^"]+)"/)?.[1];
    expect(id).toBeTruthy();
    expect(html.match(/<label\b[^>]*>[^<]*<\/label>/)?.[0]).toContain(`for="${id}"`);
    expect(html).toContain(`>${label}</label>`);
    for (const { path } of files) {
      const checkbox = html.match(/<input\b[^>]*type="checkbox"[^>]*>/g)
        ?.find((tag) => tag.includes(`aria-label="${path}"`));
      expect(checkbox).toBeTruthy();
    }
  });

  it("names every empty folder checkbox by path", () => {
    const paths = ["C:/inbox/old", "C:/archive/old"];
    useCleanupStore.setState({ emptyDirs: paths });
    const html = renderToStaticMarkup(<EmptyDirsTab />);
    for (const path of paths) {
      const checkbox = html.match(/<input\b[^>]*type="checkbox"[^>]*>/g)
        ?.find((tag) => tag.includes(`aria-label="${path}"`));
      expect(checkbox).toBeTruthy();
    }
  });
});
