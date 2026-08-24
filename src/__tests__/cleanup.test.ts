import { describe, it, expect } from "vitest";
import {
  reclaimableBytes,
  sortGroups,
  buildDuplicateActions,
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

  it("trashes everything when no keeper is chosen", () => {
    const a = group("hashA", [100, 100]);
    const actions = buildDuplicateActions([a], {});
    expect(actions).toHaveLength(2);
    expect(actions.every((x) => x.kind === "trash_duplicate")).toBe(true);
  });

  it("returns empty for empty groups", () => {
    expect(buildDuplicateActions([], {})).toHaveLength(0);
  });
});