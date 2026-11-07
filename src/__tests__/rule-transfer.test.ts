import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ipc);

import { useAppStore } from "../store/useAppStore";

beforeEach(() => {
  ipc.invoke.mockReset();
});

describe("native rule transfer", () => {
  it("passes no destination path to the export command", async () => {
    ipc.invoke.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const { exportRules } = useAppStore.getState();
    await expect(exportRules()).resolves.toBe(false);
    await expect(exportRules()).resolves.toBe(true);
    expect(ipc.invoke.mock.calls).toEqual([["export_rules_cmd"], ["export_rules_cmd"]]);
  });

  it("refreshes rules for a successful empty import and leaves cancellation alone", async () => {
    ipc.invoke.mockResolvedValueOnce(null).mockResolvedValueOnce(0).mockResolvedValueOnce([]);
    const { importRules } = useAppStore.getState();
    await expect(importRules(true)).resolves.toBeNull();
    await expect(importRules(false)).resolves.toBe(0);
    expect(ipc.invoke.mock.calls).toEqual([
      ["import_rules_cmd", { replace: true }],
      ["import_rules_cmd", { replace: false }],
      ["get_rules_cmd"],
    ]);
  });
});
