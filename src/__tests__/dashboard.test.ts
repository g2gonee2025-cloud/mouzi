import { describe, it, expect } from "vitest";
import { fileName, parentDir, parseHash, navigateHash } from "../utils/paths";
import { leadCategory, percentOf, categoryClass, formatShare, CATEGORY_CLASSES } from "../utils/dashboard";

describe("fileName / parentDir", () => {
  it("splits Windows paths", () => {
    expect(fileName("C:\\Users\\me\\photo.jpg")).toBe("photo.jpg");
    expect(parentDir("C:\\Users\\me\\photo.jpg")).toBe("C:\\Users\\me");
  });

  it("splits posix paths", () => {
    expect(fileName("/home/me/photo.jpg")).toBe("photo.jpg");
    expect(parentDir("/home/me/photo.jpg")).toBe("/home/me");
  });

  it("returns the path itself when there is no parent", () => {
    expect(fileName("readme")).toBe("readme");
    expect(parentDir("readme")).toBe("readme");
  });

  it("keeps the separator on a drive root so Explorer opens the root", () => {
    expect(parentDir("C:\\photo.jpg")).toBe("C:\\");
    expect(parentDir("D:/photo.jpg")).toBe("D:/");
    expect(parentDir("C:\\Users\\me\\photo.jpg")).toBe("C:\\Users\\me");
  });

  it("resolves a posix root parent", () => {
    expect(parentDir("/photo.jpg")).toBe("/");
  });

  it("ignores a trailing separator instead of returning an empty name", () => {
    expect(fileName("C:\\Users\\me\\")).toBe("me");
    expect(fileName("C:/Users/me/")).toBe("me");
    expect(fileName("photos/")).toBe("photos");
    expect(fileName("C:\\")).toBe("C:");
  });
});

describe("parseHash", () => {
  it("defaults to popup", () => {
    expect(parseHash("").route).toBe("popup");
    expect(parseHash("#/").route).toBe("popup");
  });

  it("reads a route and query", () => {
    const { route, params } = parseHash("#/cleanup?tab=large");
    expect(route).toBe("cleanup");
    expect(params.get("tab")).toBe("large");
  });

  it("handles a route without query", () => {
    expect(parseHash("#/dashboard").route).toBe("dashboard");
  });

  it("does not treat a doubled hash from location.hash = '/#/dashboard' as dashboard", () => {
    expect(parseHash("#/#/dashboard").route).not.toBe("dashboard");
    expect(parseHash("#/dashboard").route).toBe("dashboard");
  });

  it("keeps extra query keys", () => {
    const { route, params } = parseHash("#/cleanup?tab=stale&x=1");
    expect(route).toBe("cleanup");
    expect(params.get("tab")).toBe("stale");
    expect(params.get("x")).toBe("1");
  });
});

describe("navigateHash", () => {
  it("writes the route and query onto location.hash", () => {
    const g = globalThis as unknown as { window?: { location: { hash: string } } };
    const prev = g.window;
    const loc = { hash: "" };
    g.window = { location: loc };
    try {
      navigateHash("cleanup", { tab: "large" });
      expect(loc.hash).toBe("#/cleanup?tab=large");
      navigateHash("dashboard");
      expect(loc.hash).toBe("#/dashboard");
    } finally {
      g.window = prev;
    }
  });
});

describe("leadCategory", () => {
  it("returns null for empty data", () => {
    expect(leadCategory([], 100)).toBeNull();
    expect(leadCategory([{ category: "Videos", files: 1, bytes: 10 }], 0)).toBeNull();
  });

  it("picks the largest category and rounds percent", () => {
    const lead = leadCategory(
      [
        { category: "Videos", files: 2, bytes: 60 },
        { category: "Images", files: 4, bytes: 40 },
      ],
      100,
    );
    expect(lead).toEqual({ category: "Videos", percent: 60 });
  });
});

describe("percentOf / categoryClass", () => {
  it("clamps percent", () => {
    expect(percentOf(0, 100)).toBe(0);
    expect(percentOf(50, 100)).toBe(50);
    expect(percentOf(10, 0)).toBe(0);
    expect(percentOf(150, 100)).toBe(100);
  });

  it("falls back to Other for unknown categories", () => {
    expect(categoryClass("Videos")).toBe("bg-cat-videos");
    expect(categoryClass("Unknown")).toBe(categoryClass("Other"));
  });

  it("resolves category colours through theme utilities, not hardcoded hexes", () => {
    for (const name of Object.keys(CATEGORY_CLASSES)) {
      expect(categoryClass(name)).toMatch(/^bg-cat-[a-z]+$/);
    }
  });
});

describe("formatShare", () => {
  it("rounds normally above the half-point", () => {
    expect(formatShare(50)).toBe("50%");
    expect(formatShare(99.6)).toBe("100%");
  });

  it("does not report a real share as a flat 0%", () => {
    expect(formatShare(0.2)).toBe("<1%");
    expect(formatShare(0.49)).toBe("<1%");
  });

  it("still reports a genuine zero as 0%", () => {
    expect(formatShare(0)).toBe("0%");
  });
});
