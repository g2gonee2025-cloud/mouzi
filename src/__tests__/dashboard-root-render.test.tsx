import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import Dashboard from "../pages/Dashboard";
import StorageTreemap from "../components/dashboard/StorageTreemap";
import StorageRibbon from "../components/dashboard/StorageRibbon";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import { EMPTY_INSIGHTS, useDashboardStore, type DashboardStats, type RootStat } from "../store/useDashboardStore";
import en from "../i18n/locales/en.json";
import { formatTimestamp } from "../utils/format";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: { date?: string }) => {
      if (key === "dashboard.rootLastScan") return en.dashboard.rootLastScan.replace("{{date}}", values?.date ?? "{{date}}");
      return key === "categories.Documents" ? "Documentos" : key;
    },
  }),
}));

vi.mock("../store/useDashboardStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useDashboardStore")>();
  return {
    ...actual,
    useDashboardStore: Object.assign(() => actual.useDashboardStore.getState(), actual.useDashboardStore),
  };
});

const root: RootStat = {
  path: "C:/empty-work", files: 0, bytes: 0, status: "complete", stale: false, lastScanAt: 1, error: null,
};

function renderDashboard(watchedRoots: RootStat[]) {
  const stats: DashboardStats = {
    revision: 1, totalFiles: 0, totalBytes: 0, categoryBreakdown: [],
    largestFiles: [], recentFiles: [], watchedRoots, lastScanAt: null,
    insights: EMPTY_INSIGHTS, ageBuckets: [],
  };
  useDashboardStore.setState({ stats });
  try {
    return renderToStaticMarkup(<Dashboard />);
  } finally {
    useDashboardStore.setState({ stats: null });
  }
}

describe("dashboard entry points", () => {
  it("guides a user with no watched folders to Settings", () => {
    const html = renderDashboard([]);
    expect(html).toContain("dashboard.scanNoRoots");
    expect(html).toContain("dashboard.openSettings");
    expect(html).not.toContain("dashboard.noFiles");
  });

  it("offers labeled search before charts and distinguishes an empty inventory", () => {
    const html = renderDashboard([root]);
    expect(html).toMatch(/<label [^>]*>dashboard\.searchFiles /);
    expect(html.indexOf('role="search"')).toBeLessThan(html.indexOf("dashboard.watchedRoots"));
    expect(html).toContain("dashboard.noIndexedFiles");
    expect(html).not.toContain("dashboard.noFiles");
  });

  it("renders Scan Now disabled before the scan listeners connect", () => {
    const html = renderDashboard([root]);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>(?:(?!<\/button>).)*dashboard\.scanNow<\/button>/);
  });
});

describe("dashboard search translations", () => {
  const locales = import.meta.glob<{ dashboard: Record<string, string> }>(
    "../i18n/locales/*.json", { eager: true, import: "default" },
  );

  it("includes search guidance and preserves placeholders across all shipped locales", () => {
    expect(Object.keys(locales)).toHaveLength(10);
    for (const { dashboard } of Object.values(locales)) {
      for (const key of ["searchFiles", "noIndexedFiles", "refreshingFiles", "scanConnectionFailed"]) {
        expect(dashboard[key]?.trim()).toBeTruthy();
      }
      expect(dashboard.filesShown).toContain("{{shown}}");
      expect(dashboard.limitedFiles).toContain("{{shown}}");
      expect(dashboard.clearFilter).toContain("{{name}}");
    }
  });
});

describe("configured root presentation", () => {
  it("shows a successfully scanned empty root and its last scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[root]} totalBytes={0} />);
    expect(html).toContain("empty-work");
    expect(html).toContain(en.dashboard.rootLastScan.replace("{{date}}", formatTimestamp(root.lastScanAt)));
    expect(html).not.toContain("{{date}}");
    expect(html).not.toContain("dashboard.noData");
  });

  it("distinguishes an unscanned root from an empty completed scan", () => {
    const html = renderToStaticMarkup(<StorageTreemap data={[{ ...root, status: "unscanned", lastScanAt: null }]} totalBytes={0} />);
    expect(html).toContain("dashboard.rootPending");
    expect(html).not.toContain(en.dashboard.rootLastScan.split("{{date}}")[0]);
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
