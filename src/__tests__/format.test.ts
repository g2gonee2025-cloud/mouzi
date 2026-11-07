import { describe, it, expect } from "vitest";
import { formatBytes, formatTimestamp } from "../utils/format";

describe("formatBytes", () => {
  it("formats 0 bytes", () => {
    expect(formatBytes(0)).toBe("0 B");
  });

  it("formats negative as 0 B", () => {
    expect(formatBytes(-1)).toBe("0 B");
  });

  it("formats bytes", () => {
    expect(formatBytes(512)).toBe("512 B");
  });

  it("formats KB", () => {
    expect(formatBytes(1536)).toBe("1.50 KB");
  });

  it("formats MB", () => {
    expect(formatBytes(3_145_728)).toBe("3.00 MB");
  });

  it("formats GB", () => {
    expect(formatBytes(2_147_483_648)).toBe("2.00 GB");
  });

  it("formats TB", () => {
    expect(formatBytes(1_099_511_627_776)).toBe("1.00 TB");
  });

  it("handles fractional values", () => {
    expect(formatBytes(1_500_000)).toBe("1.43 MB");
  });
});

describe("formatTimestamp", () => {
  it("returns em dash for null", () => {
    expect(formatTimestamp(null)).toBe("—");
  });

  it("returns em dash for undefined", () => {
    expect(formatTimestamp(undefined)).toBe("—");
  });

  it("returns a formatted date for old timestamps", () => {
    // Jan 15 2024
    const result = formatTimestamp(1_705_299_200);
    expect(result).toMatch(/2024/);
  });
});