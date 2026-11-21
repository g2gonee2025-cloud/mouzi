import { renderToStaticMarkup } from "react-dom/server";
import { beforeAll, expect, it } from "vitest";
import ResultsPanel from "../components/cleanup/ResultsPanel";
import { initI18n } from "../i18n";
import type { CleanupOutcome } from "../utils/cleanup";

const unknown = "future_cleanup_wire_value" as CleanupOutcome["status"];

const results: CleanupOutcome[] = [
  { path: "C:\\First\\report.pdf", status: "ok", message: "Source removed; Recycle Bin recovery could not be verified" },
  { path: "C:\\Second\\report.pdf", status: "failed", message: "Access denied" },
  { path: "C:\\Fixture\\empty-folder", status: "ok" },
  { path: "C:\\Fixture\\changed.pdf", status: "skipped", message: "File changed since review" },
  { path: "C:\\Fixture\\removed.pdf", status: "permanently_deleted" },
  { path: "C:\\Fixture\\unverified.pdf", status: unknown },
];

beforeAll(() => initI18n("en"));

function render(outcomes = results, error: string | null = null) {
  return renderToStaticMarkup(<ResultsPanel results={outcomes} error={error} onDone={() => {}} />);
}

it("shows every outcome next to its own path even when different outcomes share a filename", () => {
  const html = render();
  const rows = html.match(/<li\b[^>]*>[\s\S]*?<\/li>/g) ?? [];
  expect(rows).toHaveLength(results.length);

  const expected = ["done", "failed", "done", "skipped", "deleted permanently", "Unknown outcome"];
  for (const [index, result] of results.entries()) {
    const row = rows.find(fragment => fragment.includes(">" + result.path + "<"));
    expect(row).toContain(">" + expected[index] + "<");
    expect(row).toMatch(/<svg\b[^>]*aria-hidden="true"/);
    if (result.message) expect(row).toContain(result.message);
  }
  const emptyDirectory = rows.find(row => row.includes("empty-folder"));
  expect(emptyDirectory).not.toMatch(/recycl|trash|deleted permanently/i);
  expect(html).not.toContain(unknown);
});

it("preserves aggregate counts, the panel error and existing actions", () => {
  const html = render(results, "Cleanup history could not be refreshed");
  for (const summary of ["done: 2", "skipped: 1", "failed: 1", "deleted permanently: 1"]) {
    expect(html).toContain(">" + summary + "<");
  }
  expect(html).toContain("Cleanup history could not be refreshed");
  expect(html).toContain("Back to list");
});

it("localizes a known and an unknown per-path outcome in the selected language", async () => {
  try {
    await initI18n("de");
    const html = render([results[0], results[5]]);
    expect(html).toContain(">erledigt<");
    expect(html).toContain(">Unbekanntes Ergebnis<");
    expect(html).not.toContain(">Unknown outcome<");
    expect(html).not.toContain(unknown);
  } finally {
    await initI18n("en");
  }
});
