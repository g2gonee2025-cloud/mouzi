import { describe, it, expect } from "vitest";
import en from "../i18n/locales/en.json";
import {
  undoNotice,
  undoAllNotices,
  undoAllBreakdown,
  UNDO_TONE_CLASS,
  type UndoTone,
} from "../utils/undo";
import type { UndoAllResult, UndoResult, UndoStatus } from "../store/useAppStore";

/** Every status the Rust side can return. Kept in sync with `UndoStatus`. */
const ALL_STATUSES: UndoStatus[] = ["ok", "collision", "missing", "failed"];

const result = (
  status: UndoStatus,
  overrides: Partial<UndoResult> = {},
): UndoResult => ({ status, message: null, restoredTo: null, ...overrides });

const all = (results: UndoResult[], count?: number): UndoAllResult => ({
  count: count ?? results.length,
  results,
});

/** Resolve a dotted i18n key against the real `en.json`. */
function lookup(source: unknown, path: string): string | undefined {
  let node: unknown = source;
  for (const segment of path.split(".")) {
    if (typeof node !== "object" || node === null) return undefined;
    node = (node as Record<string, unknown>)[segment];
  }
  return typeof node === "string" ? node : undefined;
}

describe("undoNotice", () => {
  it("maps every status to a distinct, non-empty, translated key", () => {
    const keys = ALL_STATUSES.map((status) => undoNotice(result(status))[0].key);
    expect(new Set(keys).size).toBe(ALL_STATUSES.length);
    for (const key of keys) {
      expect(key).not.toBe("");
      const text = lookup(en, key);
      expect(text, `en.json is missing ${key}`).toBeDefined();
      expect(text!.trim()).not.toBe("");
    }
  });

  it("maps each status to its own key", () => {
    expect(undoNotice(result("ok"))[0].key).toBe("undo.ok");
    expect(undoNotice(result("collision"))[0].key).toBe("undo.collision");
    expect(undoNotice(result("missing"))[0].key).toBe("undo.missing");
    expect(undoNotice(result("failed"))[0].key).toBe("undo.failed");
  });

  // The whole defect: `missing` is a row the backend marked undone while the
  // file stayed in its organised folder. If it ever shares a key or a tone with
  // `ok` the user is told a lie again.
  it("never presents missing as success", () => {
    const success = undoNotice(result("ok"))[0];
    const missing = undoNotice(result("missing"))[0];
    expect(missing.key).not.toBe(success.key);
    expect(missing.tone).not.toBe("success");
    expect(undoAllNotices(all([result("missing")]))[0].tone).not.toBe("success");
  });

  it("gives success to ok alone", () => {
    const bySuccessTone = ALL_STATUSES.filter(
      (status) => undoNotice(result(status))[0].tone === "success",
    );
    expect(bySuccessTone).toEqual(["ok"]);
  });

  it("reports the renamed path for a collision", () => {
    const notices = undoNotice(
      result("collision", { restoredTo: "C:/Downloads/report (1).pdf" }),
    );
    expect(notices[0].tone).toBe("warning");
    expect(notices[1]).toEqual({
      tone: "success",
      key: "undo.restoredTo",
      values: { path: "C:/Downloads/report (1).pdf" },
    });
  });

  // A `restoredTo` on a non-move status would be the exact claim the file came
  // back, and the file did not move.
  it("hides restoredTo for statuses where nothing moved back", () => {
    for (const status of ["missing", "failed"] as UndoStatus[]) {
      const notices = undoNotice(result(status, { restoredTo: "C:/Organised/a.pdf" }));
      expect(notices.map((n) => n.key)).not.toContain("undo.restoredTo");
    }
  });

  it("surfaces the backend's reason only for failed", () => {
    const failed = undoNotice(result("failed", { message: "ACCESS_DENIED" }));
    expect(failed[1]).toEqual({
      tone: "error",
      key: "undo.reason",
      values: { message: "ACCESS_DENIED" },
    });
    for (const status of ["ok", "collision", "missing"] as UndoStatus[]) {
      const notices = undoNotice(result(status, { message: "ACCESS_DENIED" }));
      expect(notices.map((n) => n.key)).not.toContain("undo.reason");
    }
  });

  it("returns a single line when there is nothing to add", () => {
    expect(undoNotice(result("missing"))).toHaveLength(1);
  });
});

describe("undoAllBreakdown", () => {
  it("counts collisions as restored, missing as neither", () => {
    const breakdown = undoAllBreakdown([
      result("ok"),
      result("collision"),
      result("missing"),
      result("failed"),
    ]);
    expect(breakdown).toEqual({
      total: 4,
      restored: 2,
      collision: 1,
      missing: 1,
      failed: 1,
    });
  });

  it("handles an empty run", () => {
    expect(undoAllBreakdown([])).toEqual({
      total: 0,
      restored: 0,
      collision: 0,
      missing: 0,
      failed: 0,
    });
  });
});

describe("undoAllNotices", () => {
  it("reports a clean run as a single success line", () => {
    const notices = undoAllNotices(all([result("ok"), result("ok")]));
    expect(notices).toEqual([
      { tone: "success", key: "undo.allRestored", values: { count: 2 } },
    ]);
  });

  // `UndoAllResult.count` is every row that is not `failed`, so a `missing`
  // row is inside it. Reporting that number as "restored" is the bulk form of
  // the same lie the green badge tells.
  it("does not report the backend count as the restored count", () => {
    const notices = undoAllNotices(
      all([result("ok"), result("missing"), result("failed")], 2),
    );
    expect(notices[0]).toEqual({
      tone: "warning",
      key: "undo.allRestored",
      values: { count: 1 },
    });
  });

  it("calls out missing and failed even on a partial success", () => {
    const notices = undoAllNotices(
      all([result("ok"), result("ok"), result("missing"), result("failed")]),
    );
    expect(notices.map((n) => n.key)).toEqual([
      "undo.allRestored",
      "undo.allMissing",
      "undo.allFailed",
    ]);
    expect(notices.find((n) => n.key === "undo.allMissing")!.values).toEqual({ count: 1 });
    expect(notices.find((n) => n.key === "undo.allMissing")!.tone).toBe("error");
    expect(notices.find((n) => n.key === "undo.allFailed")!.values).toEqual({ count: 1 });
  });

  it("mentions renamed files without lowering the restored headline", () => {
    const notices = undoAllNotices(all([result("collision"), result("ok")]));
    expect(notices[0].values).toEqual({ count: 2 });
    expect(notices[1]).toEqual({
      tone: "warning",
      key: "undo.allCollision",
      values: { count: 1 },
    });
  });

  it("omits the restored line when nothing was restored", () => {
    const notices = undoAllNotices(all([result("missing"), result("missing")]));
    expect(notices.map((n) => n.key)).toEqual(["undo.allMissing"]);
    expect(notices[0].values).toEqual({ count: 2 });
  });

  it("falls back to the existing disabled hint on an empty run", () => {
    expect(undoAllNotices(all([]))).toEqual([
      { tone: "warning", key: "settings.history.revertAllDisabled" },
    ]);
  });

  it("resolves every emitted key and placeholder against en.json", () => {
    const runs: UndoAllResult[] = [
      all([]),
      all([result("ok")]),
      all([result("collision"), result("missing"), result("failed")]),
    ];
    for (const run of runs) {
      for (const notice of undoAllNotices(run)) {
        const text = lookup(en, notice.key);
        expect(text, `en.json is missing ${notice.key}`).toBeDefined();
        if (notice.values) {
          for (const name of Object.keys(notice.values)) {
            expect(text, `${notice.key} is missing {{${name}}}`).toContain(`{{${name}}}`);
          }
        }
      }
    }
  });
});

describe("UNDO_TONE_CLASS", () => {
  it("styles every tone a notice can carry", () => {
    const tones = new Set<UndoTone>();
    for (const status of ALL_STATUSES) {
      for (const notice of undoNotice(result(status))) tones.add(notice.tone);
    }
    for (const key of ["undo.allRestored", "undo.allMissing"]) {
      for (const notice of undoAllNotices(all([result("ok"), result("missing")]))) {
        if (notice.key === key) tones.add(notice.tone);
      }
    }
    for (const tone of tones) {
      expect(UNDO_TONE_CLASS[tone], `no class for tone ${tone}`).toBeTruthy();
    }
  });
});
