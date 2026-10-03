import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import StorageTreemap from "../components/dashboard/StorageTreemap";
import StorageRibbon from "../components/dashboard/StorageRibbon";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import type { RootStat } from "../store/useDashboardStore";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key === "categories.Documents" ? "Documentos" : key }),
}));

const root: RootStat = {
  path: "C:/empty-work", files: 0, bytes: 0, status: "complete", stale: false, lastScanAt: 1, error: null,
};

describe("configured root presentation", () => {
  it("shows a successfully scanned empty root and its last scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[root]} totalBytes={0} />);
    expect(html).toContain("empty-work");
    expect(html).toContain("dashboard.rootLastScan");
    expect(html).not.toContain("dashboard.noData");
  });

  it("distinguishes an unscanned root from an empty completed scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[{ ...root, status: "unscanned", lastScanAt: null }]} totalBytes={0} />);
    expect(html).toContain("dashboard.rootPending");
    expect(html).not.toContain("dashboard.rootLastScan");
  });

  it("shows retained stale snapshot health and the failure", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[{ ...root, status: "failed", stale: true, error: "Access denied" }]} totalBytes={0} />);
    expect(html).toContain("dashboard.rootStale");
    expect(html).toContain("dashboard.rootFailed");
    expect(html).toContain("Access denied");
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
