import { afterEach, describe, it, expect, vi } from "vitest";
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

  it("formats KiB", () => {
    expect(formatBytes(1536)).toBe("1.50 KiB");
  });

  it("formats MiB", () => {
    expect(formatBytes(3_145_728)).toBe("3.00 MiB");
  });

  it("formats GiB", () => {
    expect(formatBytes(2_147_483_648)).toBe("2.00 GiB");
  });

  it("formats TiB", () => {
    expect(formatBytes(1_099_511_627_776)).toBe("1.00 TiB");
  });

  it("handles fractional values", () => {
    expect(formatBytes(1_500_000)).toBe("1.43 MiB");
  });
});

describe("formatTimestamp", () => {
  afterEach(() => vi.useRealTimers());

  it("uses the local calendar date across midnight and daylight-saving changes", () => {
    vi.useFakeTimers();
    // The US spring transition shortens this interval to less than 24 hours.
    vi.setSystemTime(new Date(2026, 2, 9, 0, 30));
    const yesterday = new Date(2026, 2, 8, 23, 50);
    const earlierYesterday = new Date(2026, 2, 8, 0, 15);
    const laterToday = new Date(2026, 2, 9, 23, 45);
    const nextDay = new Date(2026, 2, 10, 0, 45);
    const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
    for (const [date, offset] of [[yesterday, -1], [earlierYesterday, -1], [laterToday, 0], [nextDay, 1]] as const) {
      const time = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
      expect(formatTimestamp(date.getTime() / 1000)).toBe(`${relative.format(offset, "day")} ${time}`);
    }
  });

  it("shows the date when a file falls outside the week on either side", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 3, 8));
    for (const date of [new Date(2026, 9, 27, 20), new Date(2026, 10, 10, 20)]) {
      expect(formatTimestamp(date.getTime() / 1000)).toBe(
        date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" }),
      );
    }
  });

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
