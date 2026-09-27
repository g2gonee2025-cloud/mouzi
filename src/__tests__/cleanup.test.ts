import { describe, it, expect } from "vitest";
import {
  reclaimableBytes,
  sortGroups,
  buildDuplicateActions,
  resolveKeeper,
  type DuplicateGroup,
} from "../utils/cleanup";

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