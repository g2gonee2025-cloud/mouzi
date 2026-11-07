import { describe, it, expect } from "vitest";
import {
  reclaimableBytes,
  sortGroups,
  buildDuplicateActions,
  resolveKeeper,
  cleanupBreakdown,
  cleanupStatusMark,
  cleanupSummaryRows,
  CLEANUP_STATUS_ORDER,
  type CleanupOutcome,
  type DuplicateGroup,
} from "../utils/cleanup";
import de from "../i18n/locales/de.json";
import en from "../i18n/locales/en.json";
import es from "../i18n/locales/es.json";
import fr from "../i18n/locales/fr.json";
import italian from "../i18n/locales/it.json";
import ja from "../i18n/locales/ja.json";
import pl from "../i18n/locales/pl.json";
import ru from "../i18n/locales/ru.json";
import uk from "../i18n/locales/uk.json";
import vi from "../i18n/locales/vi.json";

const group = (hash: string, sizes: number[]): DuplicateGroup => ({
  hash,
  files: sizes.map((size, i) => ({
    path: `C:/test/${hash}/${i}.txt`,
    size,
    mtime: 1000 + i,
  })),
});

describe("reclaimableBytes", () => {
  it("sums all but one file per group", () => {
    const groups = [group("a", [100, 100, 100]), group("b", [50, 50])];
    expect(reclaimableBytes(groups)).toBe(250);
  });

  it("returns 0 for empty input", () => {
    expect(reclaimableBytes([])).toBe(0);
  });

  it("returns 0 for single-file groups", () => {
    expect(reclaimableBytes([group("a", [100])])).toBe(0);
  });
});

describe("sortGroups", () => {
  it("sorts by reclaimable bytes descending", () => {
    const groups = [group("a", [50, 50]), group("b", [100, 100, 100]), group("c", [10, 10])];
    const sorted = sortGroups(groups);
    expect(sorted.map((g) => g.hash)).toEqual(["b", "a", "c"]);
  });

  it("does not mutate the input", () => {
    const groups = [group("a", [50, 50])];
    sortGroups(groups);
    expect(groups.map((g) => g.hash)).toEqual(["a"]);
  });
});

describe("buildDuplicateActions", () => {
  it("trashes all files except the kept one per group", () => {
    const a = group("hashA", [100, 100]);
    const actions = buildDuplicateActions([a], { hashA: a.files[0].path });
    expect(actions).toHaveLength(1);
    expect(actions[0]).toMatchObject({
      kind: "trash_duplicate",
      path: a.files[1].path,
      keepPath: a.files[0].path,
    });
  });

  it("keeps the first file when no keeper has been chosen", () => {
    const a = group("hashA", [100, 100]);
    const actions = buildDuplicateActions([a], {});
    expect(actions).toHaveLength(1);
    expect(actions[0].path).toBe(a.files[1].path);
    expect(actions[0].keepPath).toBe(a.files[0].path);
  });

  it("never leaves keepPath unset, so the backend self-reference guard still runs", () => {
    const a = group("hashA", [100, 100, 100]);
    for (const action of buildDuplicateActions([a], {})) {
      expect(action.keepPath).toBe(a.files[0].path);
      expect(action.path).not.toBe(action.keepPath);
    }
  });

  it("falls back to the first file when the chosen keeper left the group", () => {
    const a = group("hashA", [100, 100]);
    const actions = buildDuplicateActions([a], { hashA: "C:/gone/missing.txt" });
    expect(actions).toHaveLength(1);
    expect(actions[0].path).toBe(a.files[1].path);
  });

  it("skips a group with no files at all", () => {
    expect(buildDuplicateActions([{ hash: "empty", files: [] }], {})).toHaveLength(0);
  });

  it("returns empty for empty groups", () => {
    expect(buildDuplicateActions([], {})).toHaveLength(0);
  });
});

describe("resolveKeeper", () => {
  it("defaults to the first file", () => {
    const a = group("hashA", [100, 100]);
    expect(resolveKeeper(a, {})).toBe(a.files[0].path);
  });

  it("returns the chosen copy when it is still in the group", () => {
    const a = group("hashA", [100, 100]);
    expect(resolveKeeper(a, { hashA: a.files[1].path })).toBe(a.files[1].path);
  });

  it("agrees with the badge the card renders", () => {
    const a = group("hashA", [100, 100, 100]);
    const keeper = resolveKeeper(a, {});
    expect(a.files.filter((f) => f.path !== keeper)).toHaveLength(
      buildDuplicateActions([a], {}).length,
    );
  });
});

const outcome = (status: CleanupOutcome["status"]): CleanupOutcome => ({
  path: `C:/test/${status}.txt`,
  status,
});

describe("cleanupBreakdown", () => {
  it("keeps a permanently deleted file out of the ok and failed buckets", () => {
    const counts = cleanupBreakdown([
      outcome("ok"),
      outcome("ok"),
      outcome("failed"),
      outcome("permanently_deleted"),
    ]);
    expect(counts.permanentlyDeleted).toBe(1);
    expect(counts.ok).toBe(2);
    expect(counts.failed).toBe(1);
    expect(counts.total).toBe(4);
    expect(counts.unknown).toBe(0);
  });

  it("counts a permanently deleted file on its own even when it is the only failure", () => {
    const counts = cleanupBreakdown([outcome("permanently_deleted")]);
    expect(counts).toMatchObject({ ok: 0, failed: 0, permanentlyDeleted: 1 });
  });

  it("does not let an off-union value borrow a real bucket", () => {
    const counts = cleanupBreakdown([
      outcome("ok"),
      { status: "something_new" },
      { status: "PERMANENTLY_DELETED" },
      { status: "" },
    ]);
    expect(counts.unknown).toBe(3);
    expect(counts.ok).toBe(1);
    expect(counts.failed).toBe(0);
    expect(counts.skipped).toBe(0);
    expect(counts.permanentlyDeleted).toBe(0);
    // `total` is the batch size, not the sum of the buckets, so a row can
    // never be both counted and uncounted at once.
    expect(counts.total).toBe(4);
  });

  it("returns zeroes for an empty batch", () => {
    expect(cleanupBreakdown([])).toEqual({
      total: 0,
      ok: 0,
      skipped: 0,
      failed: 0,
      permanentlyDeleted: 0,
      unknown: 0,
    });
  });
});

describe("cleanupSummaryRows", () => {
  it("gives every status its own row, worst last", () => {
    const rows = cleanupSummaryRows([]);
    expect(rows.map((r) => r.status)).toEqual([
      "ok",
      "skipped",
      "failed",
      "permanently_deleted",
    ]);
  });

  it("pairs the permanent count with the permanent label", () => {
    const rows = cleanupSummaryRows([outcome("permanently_deleted")]);
    const row = rows.find((r) => r.status === "permanently_deleted");
    expect(row?.labelKey).toBe("cleanup.permanentlyDeleted");
    expect(row?.count).toBe(1);
    // The anti-merge guard: counting this under "failed" would hide how many
    // files were destroyed, which is the one number the user cannot recover.
    expect(rows.find((r) => r.status === "failed")?.count).toBe(0);
    expect(rows.find((r) => r.status === "ok")?.count).toBe(0);
  });

  it("never reuses one label for two statuses", () => {
    const keys = cleanupSummaryRows([]).map((r) => r.labelKey);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toHaveLength(CLEANUP_STATUS_ORDER.length);
  });
});

describe("cleanupStatusMark", () => {
  it("gives a permanently deleted file a mark of its own", () => {
    const mark = cleanupStatusMark("permanently_deleted");
    expect(mark.icon).not.toBe(cleanupStatusMark("failed").icon);
    expect(mark.className).not.toBe(cleanupStatusMark("failed").className);
    expect(mark.className).not.toBe(cleanupStatusMark("skipped").className);
    expect(mark.className).not.toBe(cleanupStatusMark("ok").className);
  });

  it("never draws an off-union value as skipped", () => {
    const skipped = cleanupStatusMark("skipped");
    for (const value of ["something_new", "PERMANENTLY_DELETED", "", "ok "]) {
      expect(cleanupStatusMark(value).icon).not.toBe(skipped.icon);
      expect(cleanupStatusMark(value).className).not.toBe(skipped.className);
    }
  });

  it("keeps the four known marks distinct", () => {
    const marks = CLEANUP_STATUS_ORDER.map(cleanupStatusMark);
    expect(new Set(marks.map((m) => m.icon)).size).toBe(marks.length);
    expect(new Set(marks.map((m) => m.className)).size).toBe(marks.length);
  });
});

describe("cleanup.permanentlyDeleted translations", () => {
  const locales = { de, en, es, fr, it: italian, ja, pl, ru, uk, vi };

  it("exists in every shipped locale", () => {
    for (const [name, resource] of Object.entries(locales)) {
      const value = resource.cleanup.permanentlyDeleted;
      expect(typeof value, `${name} is missing cleanup.permanentlyDeleted`).toBe("string");
      expect(value.length, `${name} has an empty translation`).toBeGreaterThan(0);
    }
  });

  it("is a real translation rather than the English string pasted around", () => {
    const english = en.cleanup.permanentlyDeleted;
    for (const [name, resource] of Object.entries(locales)) {
      if (name === "en") continue;
      expect(resource.cleanup.permanentlyDeleted, `${name} copies English`).not.toBe(english);
    }
  });

  it("stays distinct from that locale's own failed and skipped labels", () => {
    for (const [name, resource] of Object.entries(locales)) {
      const { permanentlyDeleted, failed, skipped, ok } = resource.cleanup;
      expect(permanentlyDeleted, `${name} reuses its failed label`).not.toBe(failed);
      expect(permanentlyDeleted, `${name} reuses its skipped label`).not.toBe(skipped);
      expect(permanentlyDeleted, `${name} reuses its ok label`).not.toBe(ok);
    }
  });
});