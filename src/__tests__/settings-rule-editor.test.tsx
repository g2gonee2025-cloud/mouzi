import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Settings from "../components/Settings";
import { Results, RuleConditions, type OperationResult } from "../components/BetaControls";
import ReviewQueue from "../components/ReviewQueue";
import ConfirmDialog from "../components/cleanup/ConfirmDialog";
import { defaultRuleOptions, useAppStore, type AppSettings, type Rule, type ScheduleSettings, type UndoResult, type WatchedFolder } from "../store/useAppStore";
import { useReviewQueueStore } from "../store/useReviewQueueStore";

const harness = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  open: vi.fn(),
  language: "en",
  slots: [] as unknown[],
  cursor: 0,
  effects: null as (() => void)[] | null,
  disposeEffects: new Set<() => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: harness.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: harness.listen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: harness.open }));
vi.mock("react-i18next", async () => {
  const { default: english } = await import("../i18n/locales/en.json");
  const folderLabels: Record<string, unknown> = english.beta;
  return {
    useTranslation: () => ({
      i18n: { resolvedLanguage: harness.language },
      t: (key: string, values?: Record<string, string | number>) => {
        if (key === "settings.scheduler.time") return `${key} ${values?.number}`;
        if (key === "settings.scheduler.scheduledTimes") return `${key} ${values?.occurrences}`;
        if (key === "settings.scheduler.runCounts") return `${key} ${values?.organized}/${values?.errors}/${values?.deferred}`;
        if (key === "settings.scheduler.pendingRecovery") return `${key} ${values?.count}`;
        if (key === "settings.scheduler.readError") return `${key} ${values?.error}`;
        if (key === "undo.restoredTo") return english.undo.restoredTo.replace("{{path}}", String(values?.path ?? ""));
        if (key === "beta.onlyNewOnAdd" || key === "beta.onlyNewForPath") {
          const label = folderLabels[key.slice("beta.".length)];
          if (typeof label === "string") return label.replace("{{path}}", String(values?.path ?? ""));
        }
        return key;
      },
    }),
  };
});
vi.mock("../i18n", () => ({
  default: { t: (key: string, options?: { defaultValue?: string }) => options?.defaultValue ?? key, language: "en" },
}));
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  function claim(initialize: () => unknown) {
    const index = harness.cursor++;
    if (!(index in harness.slots)) harness.slots[index] = initialize();
    return index;
  }
  return {
    ...actual,
    useEffect: (setup: () => void | (() => void), dependencies: unknown[]) => {
      if (!harness.effects) return;
      const slot = harness.slots[claim(() => ({}))] as { dependencies?: unknown[]; dispose?: () => void };
      if (slot.dependencies?.length === dependencies.length && dependencies.every((value, index) => Object.is(value, slot.dependencies?.[index]))) return;
      harness.effects.push(() => {
        if (slot.dispose) {
          slot.dispose();
          harness.disposeEffects.delete(slot.dispose);
        }
        slot.dependencies = dependencies;
        slot.dispose = setup() || undefined;
        if (slot.dispose) harness.disposeEffects.add(slot.dispose);
      });
    },
    useState: (initial: unknown) => {
      const index = claim(() => typeof initial === "function" ? initial() : initial);
      return [
        harness.slots[index],
        (next: unknown) => { harness.slots[index] = typeof next === "function" ? next(harness.slots[index]) : next; },
      ];
    },
    useRef: (initial: unknown) => harness.slots[claim(() => ({ current: initial }))],
  };
});
vi.mock("../store/useAppStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useAppStore")>();
  const store = actual.useAppStore;
  return { ...actual, useAppStore: Object.assign(store.getState, store) };
});
vi.mock("../store/useReviewQueueStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useReviewQueueStore")>();
  const store = actual.useReviewQueueStore;
  const snapshot = (select: (state: ReturnType<typeof store.getState>) => unknown) => select(store.getState());
  return { ...actual, useReviewQueueStore: Object.assign(snapshot, store) };
});

interface Props {
  children?: ReactNode;
  type?: string;
  value?: string | number;
  disabled?: boolean;
  checked?: boolean;
  label?: string;
  role?: string;
  id?: string;
  title?: string;
  "aria-label"?: string;
  "aria-checked"?: boolean;
  "aria-controls"?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  onClick?: (event: { currentTarget: HTMLButtonElement }) => unknown;
  onChange?: (event: { target: { value?: string; checked?: boolean } }) => void;
  onKeyDown?: (event: { key: string }) => void;
  onConfirm?: () => Promise<void>;
  onClose?: () => void;
}
type Element = ReactElement<Props>;
type Match = (element: Element) => boolean;


function text(node: ReactNode): string {
  if (Array.isArray(node)) return node.map(text).join("");
  if (typeof node === "string") return node;
  return isValidElement<Props>(node) ? text(node.props.children) : "";
}

function find(node: ReactNode, matches: Match): Element | undefined {
  if (Array.isArray(node)) return node.map((child) => find(child, matches)).find(Boolean);
  if (!isValidElement<Props>(node)) return;
  return matches(node) ? node : find(node.props.children, matches);
}

function editor() {
  harness.cursor = 0;
  return Settings();
}

function requireElement(matches: Match, tree: ReactNode = editor()) {
  const element = find(tree, matches);
  if (!element) throw new Error("Fixture control is absent");
  return element;
}

const button = (label: string): Match => (node) => node.type === "button" && text(node.props.children) === label;
const nameField: Match = (node) => node.type === "input" && node.props["aria-label"] === "settings.rules.name";
const field = (label: string) => requireElement((node) => node.type === "input" && node.props["aria-label"] === label);
const checkbox = (label: string) => requireElement(
  (node) => node.type === "input" && !!node.props.onChange,
  requireElement((node) => node.type === "label" && text(node.props.children) === label),
);
const click = (node: Element) => node.props.onClick!({ currentTarget: {} as HTMLButtonElement });
const edit = (name: string) => click(requireElement(({ props }) => props["aria-label"] === `settings.rules.edit: ${name}`));
const toggle = (name: string) => click(requireElement(({ props }) => props.role === "switch" && props["aria-label"]?.startsWith(`${name}: `) === true));
const change = (node: Element, target: { value?: string; checked?: boolean }) => node.props.onChange!({ target });
const submit = () => click(requireElement((node) => button("settings.rules.save")(node) || button("settings.rules.edit")(node)));
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function ruleSizeField(label: string) {
  const conditions = requireElement((node) => node.type === RuleConditions);
  const tree = RuleConditions(conditions.props as unknown as Parameters<typeof RuleConditions>[0]);
  const group = requireElement((node) => node.type === "label" && text(node.props.children) === label, tree);
  return requireElement((node) => node.type === "input", group);
}

async function remove(name: string) {
  click(requireElement(({ props }) => props["aria-label"] === `confirm.op.deleteRule: ${name}`));
  const confirm = requireElement(({ props }) => props.title === "confirm.op.deleteRule" && !!props.onConfirm);
  await confirm.props.onConfirm!();
  confirm.props.onClose!();
}

const invoice: Rule = { id: 1, name: "Invoices", priority: 4, enabled: true, extensions: ["fixture"], pattern: null, destination: "", action: "ignore", folder_id: 0 };
const receipts: Rule = { ...invoice, id: 2, name: "Receipts", priority: 6 };
const db = new Map<number, Rule>();
let nextId = 10;
let ruleReadFails = false;

it("rule size draft refuses exponent notation and leaves the saved guard in place", async () => {
  const guarded = { ...invoice, options: { ...defaultRuleOptions, min_size: 1500, max_size: 5000 } };
  db.set(1, guarded);
  useAppStore.setState({ rules: [guarded] });
  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "1e3" });

  await submit();

  expect(harness.invoke).not.toHaveBeenCalledWith("update_rule_cmd", expect.anything());
  expect(db.get(1)?.options?.min_size).toBe(1500);
  expect(ruleSizeField("beta.minSize").props.value).toBe("1e3");
  expect(requireElement(button("settings.rules.save")).props.disabled).toBe(true);
});

it("rule size draft writes the exact integer beyond the JavaScript safe range", async () => {
  const guarded = { ...invoice, options: { ...defaultRuleOptions, min_size: 1 } };
  db.set(1, guarded);
  useAppStore.setState({ rules: [guarded] });
  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "9007199254740993" });

  await submit();

  expect(db.get(1)?.options?.min_size).toBe("9007199254740993");
});

it.each(["1e", "-", "1.25", "  ", "18446744073709551616"])("keeps invalid byte text %s visible without changing saved bounds", async (raw) => {
  const guarded = { ...invoice, options: { ...defaultRuleOptions, min_size: 20, max_size: 30 } };
  db.set(1, guarded);
  useAppStore.setState({ rules: [guarded] });
  edit("Invoices");
  change(ruleSizeField("beta.maxSize"), { value: raw });

  expect(ruleSizeField("beta.maxSize").props.value).toBe(raw);
  expect(ruleSizeField("beta.maxSize").props["aria-invalid"]).toBe(true);
  await submit();
  expect(db.get(1)).toEqual(guarded);
  expect(harness.invoke).not.toHaveBeenCalledWith("update_rule_cmd", expect.anything());
});

it("blocks an inversion that would round to equal JavaScript numbers", async () => {
  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "9007199254740993" });
  change(ruleSizeField("beta.maxSize"), { value: "9007199254740992" });

  expect(requireElement(button("settings.rules.save")).props.disabled).toBe(true);
  await submit();
  expect(harness.invoke).not.toHaveBeenCalledWith("update_rule_cmd", expect.anything());
});

it("saves zero, positive equal bounds, and then clears only the emptied field", async () => {
  const limits = { ...defaultRuleOptions, min_size: 3, max_size: 5, modified_after: "2026-01-01", rename_template: "{stem}-kept.{extension}" };
  const guarded = { ...invoice, options: limits };
  db.set(1, guarded);
  useAppStore.setState({ rules: [guarded] });
  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "0000" });
  change(ruleSizeField("beta.maxSize"), { value: "000" });
  expect(ruleSizeField("beta.minSize").props.value).toBe("0000");
  await submit();
  expect(db.get(1)?.options).toEqual({ ...limits, min_size: 0, max_size: 0 });

  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "0007" });
  change(ruleSizeField("beta.maxSize"), { value: "7" });
  expect(ruleSizeField("beta.minSize").props.value).toBe("0007");
  await submit();
  expect(db.get(1)?.options).toEqual({ ...limits, min_size: 7, max_size: 7 });

  edit("Invoices");
  change(ruleSizeField("beta.minSize"), { value: "" });
  await submit();
  expect(db.get(1)?.options).toEqual({ ...limits, min_size: null, max_size: 7 });
});

it("shows the exact stored u64 and preserves it on unrelated edits and the enabled toggle", async () => {
  const limits = { ...defaultRuleOptions, min_size: "9007199254740993", max_size: "18446744073709551615" };
  const guarded = { ...invoice, options: limits };
  db.set(1, guarded);
  useAppStore.setState({ rules: [guarded] });
  edit("Invoices");
  expect(ruleSizeField("beta.minSize").props.value).toBe(limits.min_size);
  expect(ruleSizeField("beta.maxSize").props.value).toBe(limits.max_size);
  change(requireElement(nameField), { value: "Exact size" });
  await submit();
  expect(db.get(1)).toMatchObject({ name: "Exact size", options: limits });

  await toggle("Exact size");
  expect(db.get(1)).toMatchObject({ enabled: false, options: limits });
});

beforeEach(() => {
  harness.slots.length = 0;
  harness.invoke.mockReset();
  harness.listen.mockReset().mockResolvedValue(() => {});
  harness.open.mockReset();
  harness.language = "en";
  db.clear();
  db.set(1, { ...invoice });
  db.set(2, { ...receipts });
  nextId = 10;
  ruleReadFails = false;
  harness.invoke.mockImplementation(async (command: string, args?: { rule?: Rule; id?: number; replace?: boolean }) => {
    if (command === "get_rules_cmd") {
      if (ruleReadFails) throw "Fixture rule list unavailable";
      return [...db.values()];
    }
    if (command === "add_rule_cmd" && args?.rule) {
      const id = nextId++;
      db.set(id, { ...args.rule, id });
      return id;
    }
    if (command === "update_rule_cmd" && args?.rule?.id !== undefined) {
      if (!db.has(args.rule.id)) throw "This rule no longer exists";
      db.set(args.rule.id, { ...args.rule });
      return;
    }
    if (command === "delete_rule_cmd" && args?.id !== undefined) {
      db.delete(args.id);
      return;
    }
    if (command === "import_rules_cmd" && args?.replace) {
      db.clear();
      const id = nextId++;
      db.set(id, { ...invoice, id, name: "Replacement" });
      return 1;
    }
    throw new Error(`Unexpected fixture command ${command}`);
  });
  useAppStore.setState(useAppStore.getInitialState(), true);
  useAppStore.setState({ rules: [...db.values()] });
  click(requireElement(({ props }) => props.label === "settings.rules.title"));
});

function disposeIgnoreEffects() {
  harness.disposeEffects.forEach((dispose) => dispose());
  harness.disposeEffects.clear();
}

afterEach(() => {
  disposeIgnoreEffects();
  vi.useRealTimers();
});

const watchedFixture: WatchedFolder = { id: 23, path: "C:/Fixture/Existing", mode: "manual", enabled: true, only_new: false };
const addFolderField = () => field("settings.folders.placeholder");
const folderMode: Match = (node) => node.type === "select" && node.props["aria-label"] === `${watchedFixture.path}: settings.folders.mode`;
const removeFolder: Match = ({ props }) => props["aria-label"] === `confirm.op.removeFolder: ${watchedFixture.path}`;
const folderConfirmation: Match = ({ props }) => props.title === "confirm.op.removeFolder" && !!props.onConfirm;
const alert: Match = ({ props }) => props.role === "alert";

const ruleInbox = { ...watchedFixture, id: 23, path: "C:/Fixture/Inbox" };
const ruleArchive = { ...watchedFixture, id: 24, path: "C:/Fixture/Archive" };
const archiveChild = "c:\\fixture\\ARCHIVE\\Filed";
const destinationField = () => field("settings.rules.destination");

function addMoveRule() {
  useAppStore.setState({ folders: [ruleInbox, ruleArchive], folderRead: { status: "ready" } });
  click(requireElement(button("settings.rules.add")));
  change(requireElement(nameField), { value: "File receipts" });
  change(field("settings.rules.extensions"), { value: "fixture" });
}

function setRuleScope(id: number) {
  const scope = requireElement((node) => node.type === "label" && text(node.props.children).startsWith("settings.folders.title"));
  change(requireElement((node) => node.type === "select", scope), { value: String(id) });
}

async function pickRuleFolder(path: string | null) {
  harness.open.mockResolvedValueOnce(path);
  click(requireElement(button("beta.pickFolder")));
  await settle();
}

it("keeps a picked rule folder's target when the source scope changes after the pick", async () => {
  addMoveRule();
  await pickRuleFolder(archiveChild);
  expect(destinationField().props.value).toBe("Filed");
  setRuleScope(ruleInbox.id);
  expect(destinationField().props.value).toBe(archiveChild);
  setRuleScope(ruleArchive.id);
  expect(destinationField().props.value).toBe("Filed");
  setRuleScope(0);
  expect(destinationField().props.value).toBe("Filed");
  setRuleScope(ruleInbox.id);
  await submit();
  expect(db.get(10)).toMatchObject({ folder_id: ruleInbox.id, destination: archiveChild });
});

it("keeps an exact watched root absolute when saving a picked rule folder", async () => {
  addMoveRule();
  const archive = "c:\\FIXTURE\\Archive";
  await pickRuleFolder(archive);
  expect(destinationField().props.value).toBe(archive);
  await submit();
  expect(db.get(10)).toMatchObject({ folder_id: 0, destination: archive });
});

it("keeps a cross-root pick absolute when the rule is already scoped", async () => {
  addMoveRule();
  setRuleScope(ruleInbox.id);
  await pickRuleFolder(archiveChild);
  expect(destinationField().props.value).toBe(archiveChild);
  await submit();
  expect(db.get(10)).toMatchObject({ folder_id: ruleInbox.id, destination: archiveChild });
});

it("keeps a typed rule folder override through scope changes and picker cancellation", async () => {
  addMoveRule();
  await pickRuleFolder(archiveChild);
  change(destinationField(), { value: "Typed filing folder" });
  setRuleScope(ruleInbox.id);
  await pickRuleFolder(null);
  expect(destinationField().props.value).toBe("Typed filing folder");
  await submit();
  expect(db.get(10)).toMatchObject({ folder_id: ruleInbox.id, destination: "Typed filing folder" });
});

function openWatchedFolders() {
  useAppStore.setState({ folders: [watchedFixture], folderRead: { status: "ready" } });
  click(requireElement(({ props }) => props.label === "settings.folders.title"));
}

it("names the new-files-only checkbox for the add form and each exact watched folder", async () => {
  openWatchedFolders();
  const first = { ...watchedFixture, path: "C:/Home/Downloads" };
  const second = { ...watchedFixture, id: 24, path: "C:/Work/Downloads" };
  useAppStore.setState({ folders: [first, second] });

  const onlyNew = (name: string) => {
    const label = find(editor(), (node) => {
      if (node.type !== "label" || text(node.props.children) !== "beta.onlyNew") return false;
      const input = find(node.props.children, (child) => child.type === "input");
      return (input?.props["aria-label"] ?? text(node.props.children)) === name;
    });
    expect(label, "Missing only-new checkbox named " + name).toBeDefined();
    return requireElement((node) => node.type === "input" && node.props.type === "checkbox", label);
  };

  const newRegistration = onlyNew("Process only new files when adding a folder");
  expect(onlyNew("Process only new files in C:/Home/Downloads").props.checked).toBe(false);
  const workRegistration = onlyNew("Process only new files in C:/Work/Downloads");

  change(newRegistration, { checked: true });
  expect(onlyNew("Process only new files when adding a folder").props.checked).toBe(true);
  expect(harness.invoke).not.toHaveBeenCalled();

  harness.invoke.mockResolvedValueOnce(undefined).mockResolvedValueOnce([first, { ...second, only_new: true }]);
  change(workRegistration, { checked: true });
  await settle();
  expect(harness.invoke.mock.calls).toEqual([
    ["set_only_new_cmd", { id: 24, enabled: true }],
    ["get_folders_cmd"],
  ]);
  expect(onlyNew("Process only new files in C:/Home/Downloads").props.checked).toBe(false);
  expect(onlyNew("Process only new files in C:/Work/Downloads").props.checked).toBe(true);
});

it("clears a committed add draft and reloads the canonical registered path only on a successful read", async () => {
  openWatchedFolders();
  const canonical: WatchedFolder = { id: 42, path: "C:/Fixture/Added", mode: "manual", enabled: true, only_new: true };
  const typed = "c:/fixture/Added/";
  change(addFolderField(), { value: typed });
  change(checkbox("beta.onlyNew"), { checked: true });
  harness.invoke.mockResolvedValueOnce(canonical.id).mockRejectedValueOnce(new Error("Fixture list unavailable"))
    .mockRejectedValueOnce(new Error("Retry still unavailable")).mockResolvedValueOnce([watchedFixture, canonical]);
  await click(requireElement(button("settings.folders.add")));

  expect(addFolderField().props.value).toBe("");
  expect(checkbox("beta.onlyNew").props.checked).toBe(false);
  expect(text(requireElement(alert))).toContain("settings.folders.savedRefreshError");
  expect(text(requireElement(alert))).toContain("Fixture list unavailable");
  expect(text(editor())).not.toContain(canonical.path);
  expect(find(editor(), folderMode)).toBeUndefined();

  await click(requireElement(button("common.retry")));
  expect(text(requireElement(alert))).toContain("settings.folders.savedRefreshError");
  expect(text(requireElement(alert))).toContain("Retry still unavailable");
  expect(text(requireElement(alert))).not.toContain("Fixture list unavailable");
  await click(requireElement(button("common.retry")));
  expect(find(editor(), alert)).toBeUndefined();
  expect(text(editor())).toContain(canonical.path);
  expect(text(editor())).not.toContain(typed);
  expect(harness.invoke.mock.calls).toEqual([
    ["add_folder_cmd", { path: typed, mode: "manual", onlyNew: true }],
    ["get_folders_cmd"],
    ["get_folders_cmd"],
    ["get_folders_cmd"],
  ]);
});

it.each(["remove", "mode", "only-new"])("reports a saved %s independently from a failed folder-list read", async (action) => {
  openWatchedFolders();
  harness.invoke.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("Fixture list unavailable"));
  let command: string;
  let updated: WatchedFolder | null;
  if (action === "remove") {
    command = "remove_folder_cmd";
    updated = null;
    click(requireElement(removeFolder));
    const confirmation = requireElement(folderConfirmation);
    await expect(confirmation.props.onConfirm!()).resolves.toBeUndefined();
    confirmation.props.onClose!();
    expect(find(editor(), folderConfirmation)).toBeUndefined();
  } else if (action === "mode") {
    command = "update_folder_mode_cmd";
    updated = { ...watchedFixture, mode: "paused" };
    change(requireElement(folderMode), { value: "paused" });
  } else {
    command = "set_only_new_cmd";
    updated = { ...watchedFixture, only_new: true };
    const row = requireElement((node) => node.type === "div" && Children.toArray(node.props.children)
      .some((child) => isValidElement<Props>(child) && removeFolder(child)));
    change(requireElement((node) => node.type === "input" && node.props.type === "checkbox", row), { checked: true });
  }
  await settle();

  expect(text(requireElement(alert))).toContain("settings.folders.savedRefreshError");
  expect(find(editor(), folderMode)).toBeUndefined();
  expect(find(editor(), removeFolder)).toBeUndefined();
  harness.invoke.mockResolvedValueOnce(updated ? [updated] : []);
  await click(requireElement(button("common.retry")));
  expect(useAppStore.getState().folders).toEqual(updated ? [updated] : []);
  expect(find(editor(), alert)).toBeUndefined();
  expect(harness.invoke.mock.calls.map(([name]) => name)).toEqual([command, "get_folders_cmd", "get_folders_cmd"]);
});

it("keeps the draft and only the write error when the native add itself fails", async () => {
  openWatchedFolders();
  change(addFolderField(), { value: "C:/Fixture/Unregistered" });
  harness.invoke.mockRejectedValueOnce(new Error("Fixture write rejected"));
  await click(requireElement(button("settings.folders.add")));
  expect(addFolderField().props.value).toBe("C:/Fixture/Unregistered");
  expect(text(requireElement(alert))).toContain("Fixture write rejected");
  expect(find(editor(), button("common.retry"))).toBeUndefined();
  expect(find(editor(), folderMode)).toBeDefined();
  expect(harness.invoke.mock.calls.map(([name]) => name)).toEqual(["add_folder_cmd"]);
});

it("withholds rows while reading and labels an ordinary read failure separately from a committed write", async () => {
  openWatchedFolders();
  let rejectRead!: (reason: Error) => void;
  harness.invoke.mockReturnValueOnce(new Promise((_resolve, reject) => { rejectRead = reject; }));
  const read = useAppStore.getState().loadFolders();
  const loading = editor();
  rejectRead(new Error("Fixture refresh failure"));
  await expect(read).rejects.toThrow("Fixture refresh failure");
  expect(text(loading)).toContain("common.loading");
  expect(find(loading, folderMode)).toBeUndefined();
  expect(find(loading, removeFolder)).toBeUndefined();

  expect(text(requireElement(alert))).toContain("settings.folders.loadError");
  expect(text(requireElement(alert))).toContain("Fixture refresh failure");
  expect(find(editor(), removeFolder)).toBeUndefined();
  harness.invoke.mockResolvedValueOnce([watchedFixture]);
  await click(requireElement(button("common.retry")));
  expect(find(editor(), alert)).toBeUndefined();
  expect(find(editor(), folderMode)).toBeDefined();

  harness.invoke.mockRejectedValueOnce(new Error("Later read fails"));
  await expect(useAppStore.getState().loadFolders()).rejects.toThrow("Later read fails");
  expect(text(requireElement(alert))).toContain("settings.folders.loadError");
  expect(text(requireElement(alert))).toContain("Later read fails");
});

it("does not let a captured folder mode or remove confirmation act after the list becomes unverified", async () => {
  openWatchedFolders();
  const capturedMode = requireElement(folderMode);
  click(requireElement(removeFolder));
  const confirmation = requireElement(folderConfirmation);
  harness.invoke.mockRejectedValueOnce(new Error("Fixture list unavailable"));
  await expect(useAppStore.getState().loadFolders()).rejects.toThrow("Fixture list unavailable");
  harness.invoke.mockClear().mockResolvedValue([]);
  change(capturedMode, { value: "paused" });
  await expect(confirmation.props.onConfirm!()).rejects.toThrow("settings.folders.removeChanged");
  await settle();
  expect(harness.invoke).not.toHaveBeenCalled();
});

it("does not let a late failed post-write read replace a newer verified list", async () => {
  openWatchedFolders();
  let failOld!: (reason: Error) => void;
  const olderRead = new Promise((_resolve, reject) => { failOld = reject; });
  harness.invoke.mockResolvedValueOnce(undefined).mockReturnValueOnce(olderRead);
  const modeChange = useAppStore.getState().updateFolderMode(watchedFixture.id!, "paused");
  await settle();
  const committed = { ...watchedFixture, mode: "paused" };
  harness.invoke.mockResolvedValueOnce([committed]);
  await useAppStore.getState().loadFolders();
  failOld(new Error("Superseded fixture read failure"));
  await expect(modeChange).resolves.toBeUndefined();
  expect(find(editor(), alert)).toBeUndefined();
  expect(requireElement(folderMode).props.value).toBe("paused");
});

const scheduleFixture: AppSettings = {
  id: 1, revision: 4, language: "en", theme: "system", telemetry_enabled: false,
  first_run: false, autostart: false, grace_period_seconds: 300,
  lock_check_enabled: true, recursive_watch: false, local_ai_enabled: false, local_ai_model: "llama3.2",
  schedule_enabled: true, schedule_times_per_day: 2, schedule_time_1: "21:17", schedule_time_2: "23:19",
  schedule_time_3: null, schedule_time_4: null,
};

it("keeps a bulk collision path available past the routine timeout until dismissed", async () => {
  vi.useFakeTimers();
  harness.slots.length = 0;
  const original = "C:/Fixture/report.pdf";
  const actual = "C:/Fixture/report_0.pdf";
  let outcome: UndoResult = { status: "collision", restoredTo: actual, message: null };
  harness.invoke.mockImplementation(async (command: string) => {
    switch (command) {
      case "preview_undo_all_cmd": return [{ id: 12, source: original }];
      case "undo_all_cmd": return { count: 1, results: [outcome] };
      case "get_settings_cmd": return scheduleFixture;
      case "get_history_counts_cmd": return { total: 1, undoable: 1 };
      case "get_rules_cmd":
      case "get_folders_cmd":
      case "get_logs_cmd":
      case "get_stats_cmd": return [];
      case "take_review_request_cmd":
      case "take_add_folder_request_cmd": return null;
      case "get_watcher_issues_cmd": return {};
      case "explorer_integration_status_cmd": return [false, false];
      default: throw new Error(`Unexpected fixture command ${command}`);
    }
  });
  const renderSettings = () => renderWithEffects(editor);
  const receipt = (node: ReactNode) => find(node, (element) => element.props.role === "status" &&
    text(element).includes("undo.allRestored"));
  click(requireElement(({ props }) => props.label === "settings.history.title", renderSettings()));
  await vi.advanceTimersByTimeAsync(0);

  const restoreAll = async () => {
    click(requireElement(button("settings.history.revertAll"), renderSettings()));
    await vi.advanceTimersByTimeAsync(0);
    const dialog = requireElement(({ props }) => props.title === "confirm.op.undoAll", renderSettings());
    await dialog.props.onConfirm!();
    dialog.props.onClose!();
    return renderSettings();
  };
  const first = await restoreAll();
  expect(harness.invoke).toHaveBeenCalledWith("undo_all_cmd", { reviewedIds: [12] });
  expect.soft(text(receipt(first))).toContain(`Now at ${actual}`);
  expect(text(receipt(first))).toContain("undo.allCollision");

  await vi.advanceTimersByTimeAsync(8_001);
  const afterTimeout = renderSettings();
  const retainedReceipt = receipt(afterTimeout);
  expect.soft(text(retainedReceipt)).toContain(`Now at ${actual}`);
  const dismiss = requireElement(button("common.dismiss"), afterTimeout);
  expect(retainedReceipt?.props.id).toBe("bulk-undo-receipt");
  expect(dismiss.props["aria-controls"]).toBe(retainedReceipt?.props.id);
  expect(find(retainedReceipt, button("common.dismiss"))).toBeUndefined();
  click(dismiss);
  expect(receipt(renderSettings())).toBeUndefined();

  outcome = { status: "ok", restoredTo: original, message: null };
  const routine = await restoreAll();
  expect(text(receipt(routine))).toContain("undo.allRestored");
  expect(text(receipt(routine))).not.toContain(original);
  await vi.advanceTimersByTimeAsync(8_001);
  expect(receipt(renderSettings())).toBeUndefined();
});

function openScheduler(patch: Partial<AppSettings> = {}) {
  let persisted = { ...scheduleFixture, ...patch };
  useAppStore.getState().applySettings(persisted);
  harness.invoke.mockImplementation(async (command: string, args: { schedule: ScheduleSettings }) => {
    if (command !== "update_schedule_cmd") throw new Error(`Unexpected fixture command ${command}`);
    persisted = { ...persisted, ...args.schedule, revision: persisted.revision + 1 };
    return persisted;
  });
  click(requireElement(({ props }) => props.label === "settings.general.title"));
  return () => persisted;
}

const scheduleTime = (number: number) => field(`settings.scheduler.time ${number}`);
const schedulerPanel = () => requireElement(({ props }) => {
  const [intro] = Children.toArray(props.children);
  return isValidElement<Props>(intro) && Children.toArray(intro.props.children).some(
    (node) => isValidElement<Props>(node) && node.type === "h3" && text(node) === "settings.scheduler.title",
  );
});
const scheduleAction = (action: string) => click(requireElement(button(action), schedulerPanel()));
const saveSchedule = () => scheduleAction("common.save");
const scheduleCount = () => requireElement((node) => node.type === "select" && node.props["aria-label"] === "settings.scheduler.timesPerDay");
const scheduleSwitch = () => requireElement(({ props }) => props.role === "switch" && props["aria-label"] === "settings.scheduler.enable");
const graceNumber = () => requireElement((node) => node.type === "input" && node.props.type === "number" && node.props["aria-label"] === "settings.general.gracePeriod");
const startupSwitch: Match = ({ props }) => props.role === "switch" && props["aria-label"] === "settings.general.startWithSystem";

const catchupReceipt = {
  id: 6, status: "completed", claimed_at: "2026-11-20T14:17:23.987654321Z",
  occurrences: 2, organized: 0, errors: 0, deferred: 0, uncertain: 0, message: null,
};

function schedulerReceipts() {
  return renderEffectfulChild(requireElement(
    (node) => typeof node.type === "function" && node.type.name === "SchedulerRunStatus",
    schedulerPanel(),
  ));
}

it("reports scheduled occurrences and work separately in the selected app language", async () => {
  openScheduler();
  harness.language = "de";
  const ordinary = { ...catchupReceipt, id: 5, occurrences: 1 };
  harness.invoke.mockResolvedValueOnce([catchupReceipt, ordinary]);
  schedulerReceipts();
  await settle();

  const list = requireElement((node) => node.type === "ul", schedulerReceipts());
  const [catchup, single] = Children.toArray(list.props.children).map(text);
  expect.soft(catchup).toContain("settings.scheduler.scheduledTimes 2");
  expect.soft(single).toContain("settings.scheduler.scheduledTimes 1");
  expect.soft(catchup).toContain(new Date(catchupReceipt.claimed_at).toLocaleString("de"));
  expect(catchup).toContain("settings.scheduler.status.completed");
  expect(catchup).toContain("settings.scheduler.runCounts 0/0/0");

  harness.language = "ja";
  expect(text(schedulerReceipts())).toContain(new Date(catchupReceipt.claimed_at).toLocaleString("ja"));
  expect(harness.invoke).toHaveBeenCalledExactlyOnceWith("get_recent_scheduler_runs_cmd");
});

it("translates exact interrupted reasons while retaining unknown records and actual failures", async () => {
  openScheduler();
  const recovery = {
    ...catchupReceipt, status: "interrupted", organized: 3, errors: 1, deferred: 2, uncertain: 4,
    message: "The app closed before this run finished",
  };
  const settingsChanged = { ...recovery, id: 5, message: "Schedule settings changed during the run" };
  const failed = { ...recovery, id: 4, status: "failed", message: "C:/Fixture: access denied." };
  const legacy = { ...recovery, id: 3, status: "old-status", message: "The app closed before this run finished (legacy detail)" };
  harness.invoke.mockResolvedValueOnce([recovery, settingsChanged, failed, legacy]);
  schedulerReceipts();
  await settle();

  const list = requireElement((node) => node.type === "ul", schedulerReceipts());
  const [interrupted, revoked, failure, unknown] = Children.toArray(list.props.children).map(text);
  expect.soft(interrupted).toContain("settings.scheduler.appClosed");
  expect.soft(revoked).toContain("settings.scheduler.settingsChanged");
  expect(interrupted).toContain("settings.scheduler.status.interrupted");
  expect(interrupted).toContain("settings.scheduler.runCounts 3/1/2");
  expect(interrupted).toContain("settings.scheduler.pendingRecovery 4");
  expect(failure).toContain("settings.scheduler.status.failed");
  expect(failure).toContain(failed.message);
  expect.soft(unknown).toContain(legacy.status);
  expect(unknown).not.toContain(`status.${legacy.status}`);
  expect(unknown).toContain(legacy.message);
});

it("keeps saved receipts on a read failure and retries without clearing an unsaved-result warning", async () => {
  openScheduler();
  let emitScheduledDone!: (event: { payload: { result_saved: boolean } }) => void;
  harness.listen.mockImplementation(async (_name, listener) => {
    emitScheduledDone = listener;
    return () => {};
  });
  harness.invoke.mockResolvedValueOnce([catchupReceipt]);
  schedulerReceipts();
  await settle();

  harness.invoke.mockRejectedValueOnce("Fixture database read failed");
  emitScheduledDone({ payload: { result_saved: false } });
  await settle();
  const failedRead = schedulerReceipts();
  expect(text(failedRead)).toContain("settings.scheduler.status.completed");
  expect.soft(text(failedRead)).toContain("settings.scheduler.readError Fixture database read failed");
  expect.soft(text(failedRead)).toContain("settings.scheduler.resultUnsaved");

  harness.invoke.mockResolvedValueOnce([catchupReceipt]);
  click(requireElement(button("common.retry"), failedRead));
  await settle();
  expect(text(schedulerReceipts())).not.toContain("Fixture database read failed");
  expect(text(schedulerReceipts())).toContain("settings.scheduler.resultUnsaved");

  harness.invoke.mockResolvedValueOnce([catchupReceipt]);
  emitScheduledDone({ payload: { result_saved: true } });
  await settle();
  expect(text(schedulerReceipts())).not.toContain("settings.scheduler.resultUnsaved");
  expect(harness.invoke.mock.calls).toEqual(Array.from({ length: 4 }, () => ["get_recent_scheduler_runs_cmd"]));
});

it("waits for the Windows startup status even if an older profile saved On", () => {
  openScheduler({ autostart: true });
  const general = editor();
  expect.soft(find(general, startupSwitch)).toBeUndefined();
  expect.soft(text(general)).toContain("settings.general.startupLoading");
  expect(requireElement(({ props }) => props["aria-label"] === "settings.general.theme", general).props.disabled).toBe(false);
});

it("retries a failed startup lookup and can explicitly enable a Windows-blocked registration", async () => {
  openScheduler({ autostart: true });
  harness.invoke.mockRejectedValueOnce("Registration read was denied");
  await useAppStore.getState().loadStartupStatus();
  expect(find(editor(), startupSwitch)).toBeUndefined();
  expect(text(editor())).toContain("settings.general.startupReadError Registration read was denied");

  harness.invoke.mockResolvedValueOnce("blockedBySystem");
  click(requireElement(button("common.retry")));
  await settle();
  expect(requireElement(startupSwitch).props).toMatchObject({ "aria-checked": false, "aria-describedby": "startup-blocked-note" });
  expect(text(editor())).toContain("settings.general.startupBlocked");
  expect(text(editor())).not.toContain("settings.general.startupReadError");

  harness.invoke.mockResolvedValueOnce("enabled").mockResolvedValueOnce("enabled");
  click(requireElement(startupSwitch));
  expect(find(editor(), startupSwitch)).toBeUndefined();
  expect(text(editor())).toContain("settings.general.startupSaving");
  await settle();
  expect(requireElement(startupSwitch).props["aria-checked"]).toBe(true);
  expect(harness.invoke.mock.calls).toEqual([
    ["get_startup_status_cmd"], ["get_startup_status_cmd"], ["set_startup_enabled_cmd", { enabled: true }], ["get_startup_status_cmd"],
  ]);
});

it("offers explicit adoption when Windows points to another Mouzi location", async () => {
  const persisted = openScheduler({ autostart: true });
  harness.invoke.mockResolvedValueOnce("differentLocation");
  await useAppStore.getState().loadStartupStatus();
  const general = editor();
  expect.soft(text(general)).toContain("settings.general.startupDifferentLocation");
  expect.soft(text(general)).not.toContain("settings.general.startupReadError");
  expect.soft(find(general, startupSwitch)?.props).toMatchObject({
    "aria-checked": false, "aria-describedby": "startup-location-note",
  });
  expect(harness.invoke.mock.calls).toEqual([["get_startup_status_cmd"]]);
  expect(requireElement(({ props }) => props.id === "startup-location-note", general).props.role).toBe("status");

  harness.invoke.mockRejectedValueOnce("Windows refused this copy").mockResolvedValueOnce("differentLocation");
  click(requireElement(startupSwitch));
  expect(find(editor(), startupSwitch)).toBeUndefined();
  expect(text(editor())).toContain("settings.general.startupSaving");
  expect(text(editor())).not.toContain("settings.general.startupDifferentLocation");
  await settle();
  expect(requireElement(startupSwitch).props["aria-checked"]).toBe(false);
  expect(text(editor())).toContain("settings.general.startupWriteError Windows refused this copy");
  expect(text(editor())).toContain("settings.general.startupDifferentLocation");

  harness.invoke.mockResolvedValueOnce("enabled").mockResolvedValueOnce("enabled");
  click(requireElement(startupSwitch));
  await settle();
  expect(requireElement(startupSwitch).props["aria-checked"]).toBe(true);
  expect(requireElement(startupSwitch).props["aria-describedby"]).toBeUndefined();
  expect(text(editor())).not.toContain("settings.general.startupDifferentLocation");
  expect(text(editor())).not.toContain("Windows refused this copy");
  expect(harness.invoke.mock.calls).toEqual([
    ["get_startup_status_cmd"],
    ["set_startup_enabled_cmd", { enabled: true }], ["get_startup_status_cmd"],
    ["set_startup_enabled_cmd", { enabled: true }], ["get_startup_status_cmd"],
  ]);
  expect(persisted()).toMatchObject({ autostart: true, revision: 4 });
});

it("keeps unavailable startup and a verified failed change separate from other preferences", async () => {
  openScheduler({ autostart: true });
  harness.invoke.mockResolvedValueOnce("unavailable");
  await useAppStore.getState().loadStartupStatus();
  expect(find(editor(), startupSwitch)).toBeUndefined();
  expect(text(editor())).toContain("settings.general.startupUnavailable");

  harness.invoke.mockResolvedValueOnce("enabled").mockRejectedValueOnce("Windows refused the change").mockResolvedValueOnce("enabled");
  await useAppStore.getState().loadStartupStatus();
  click(requireElement(startupSwitch));
  await settle();
  expect(requireElement(startupSwitch).props["aria-checked"]).toBe(true);
  expect(text(editor())).toContain("settings.general.startupWriteError Windows refused the change");
  expect(requireElement(({ props }) => props["aria-label"] === "settings.general.theme").props.disabled).toBe(false);
  expect(harness.invoke.mock.calls).not.toContainEqual(["update_settings_cmd", { settings: { autostart: false } }]);
});

it("keeps a cleared active time visible while preserving Cancel and an unrelated grace draft", () => {
  const persisted = openScheduler();
  change(graceNumber(), { value: "13" });
  change(scheduleTime(1), { value: "" });
  expect(scheduleTime(1).props.value).toBe("");
  expect(scheduleTime(2).props.value).toBe("23:19");
  expect(find(editor(), ({ props }) => props.role === "alert")).toBeUndefined();

  scheduleAction("common.cancel");
  expect(scheduleTime(1).props.value).toBe("21:17");
  expect(scheduleTime(2).props.value).toBe("23:19");
  expect(graceNumber().props.value).toBe("13");
  expect(persisted().revision).toBe(4);
  expect(harness.invoke).not.toHaveBeenCalled();
});

it("blocks saving a blank active time, then commits only the refilled schedule", async () => {
  const persisted = openScheduler();
  change(scheduleTime(2), { value: "" });
  await saveSchedule();
  expect(harness.invoke).not.toHaveBeenCalled();
  expect(persisted()).toMatchObject({ schedule_time_2: "23:19", revision: 4 });
  expect(scheduleTime(2).props["aria-invalid"]).toBe(true);
  expect(text(requireElement(({ props }) => props.role === "alert"))).toContain("settings.scheduler.timeRequired");

  change(scheduleTime(2), { value: "22:31" });
  expect(find(editor(), ({ props }) => props.role === "alert")).toBeUndefined();
  await saveSchedule();
  expect(harness.invoke).toHaveBeenCalledExactlyOnceWith("update_schedule_cmd", {
    schedule: {
      schedule_enabled: true, schedule_times_per_day: 2, schedule_time_1: "21:17",
      schedule_time_2: "22:31", schedule_time_3: null, schedule_time_4: null,
    },
  });
  expect(persisted()).toMatchObject({ schedule_time_2: "22:31", revision: 5 });
  expect(scheduleTime(2).props.value).toBe("22:31");
});

it("still supplies defaults for legacy empty slots on enabling and adding times", async () => {
  const persisted = openScheduler({
    schedule_enabled: false, schedule_time_1: null, schedule_time_2: "",
  });
  click(scheduleSwitch());
  expect(scheduleTime(1).props.value).toBe("08:00");
  expect(scheduleTime(2).props.value).toBe("14:00");
  change(scheduleCount(), { value: "4" });
  expect(scheduleTime(3).props.value).toBe("20:00");
  expect(scheduleTime(4).props.value).toBe("23:00");
  await saveSchedule();
  expect(persisted()).toMatchObject({
    schedule_enabled: true, schedule_times_per_day: 4, schedule_time_1: "08:00",
    schedule_time_2: "14:00", schedule_time_3: "20:00", schedule_time_4: "23:00",
  });
});

it("defaults a newly active slot without filling an explicitly cleared slot", async () => {
  const persisted = openScheduler();
  change(scheduleTime(2), { value: "" });
  change(scheduleCount(), { value: "3" });
  expect(scheduleTime(2).props.value).toBe("");
  expect(scheduleTime(3).props.value).toBe("20:00");

  change(scheduleCount(), { value: "1" });
  await saveSchedule();
  expect(persisted()).toMatchObject({
    schedule_enabled: true, schedule_times_per_day: 1, schedule_time_1: "21:17", schedule_time_2: null,
  });
});

it("allows a blank time to be saved while scheduling is disabled", async () => {
  const persisted = openScheduler({ schedule_enabled: false });
  change(scheduleTime(1), { value: "" });
  expect(scheduleTime(1).props.value).toBe("");
  await saveSchedule();
  expect(persisted()).toMatchObject({ schedule_enabled: false, schedule_time_1: null, revision: 5 });
  expect(find(editor(), ({ props }) => props.role === "alert")).toBeUndefined();
});

it("keeps both a same-row toggle and other changes from the open draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Invoices revised" });
  change(field("settings.rules.priority"), { value: "17" });
  toggle("Invoices");
  await settle();
  expect(db.get(1)?.enabled).toBe(false);
  const draftEnabledAfterToggle = checkbox("settings.rules.enabled").props.checked;
  await submit();

  expect(db.get(1)).toEqual({ ...invoice, name: "Invoices revised", priority: 17, enabled: false });
  expect(db.get(2)).toEqual(receipts);
  expect(draftEnabledAfterToggle).toBe(false);
  edit("Invoices revised");
  expect(checkbox("settings.rules.enabled").props.checked).toBe(false);
});

it.each(["", "-", "1.75", "2e2", "-2147483649", "2147483648"])(
  "keeps invalid priority %j and prevents saving any field",
  async (raw) => {
    edit("Invoices");
    change(field("settings.rules.name"), { value: "Unsaved name" });
    change(field("settings.rules.priority"), { value: raw });

    await submit();

    expect(harness.invoke).not.toHaveBeenCalledWith("update_rule_cmd", expect.anything());
    expect(db.get(1)).toEqual(invoice);
    expect(db.get(2)).toEqual(receipts);
    expect(field("settings.rules.name").props.value).toBe("Unsaved name");
    expect(String(field("settings.rules.priority").props.value)).toBe(raw);
    expect(field("settings.rules.priority").props["aria-invalid"]).toBe(true);
    expect(text(requireElement(({ props }) => props.role === "alert"))).toBe("settings.rules.priorityInvalid");
    expect(requireElement(button("settings.rules.save")).props.disabled).toBe(true);
  },
);

it.each([
  { value: -2147483648, raw: "-2147483648" },
  { value: -12, raw: "-12" },
  { value: 0, raw: "0" },
  { value: 42, raw: "+0042" },
  { value: 2147483647, raw: "2147483647" },
])(
  "saves whole priority $value exactly and reopens with the saved value",
  async ({ value, raw }) => {
    edit("Invoices");
    change(field("settings.rules.priority"), { value: "1.75" });
    change(field("settings.rules.priority"), { value: raw });
    expect(String(field("settings.rules.priority").props.value)).toBe(raw);
    expect(requireElement(button("settings.rules.save")).props.disabled).toBe(false);

    await submit();

    expect(db.get(1)).toEqual({ ...invoice, priority: value });
    expect(db.get(2)).toEqual(receipts);
    edit("Invoices");
    expect(String(field("settings.rules.priority").props.value)).toBe(String(value));
  },
);

it("resets a canceled priority draft and preserves the default priority on create", async () => {
  edit("Invoices");
  change(field("settings.rules.priority"), { value: "" });
  click(requireElement(button("common.cancel")));
  edit("Invoices");
  expect(String(field("settings.rules.priority").props.value)).toBe(String(invoice.priority));
  expect(db.get(1)).toEqual(invoice);
  click(requireElement(button("common.cancel")));

  click(requireElement(button("settings.rules.add")));
  expect(String(field("settings.rules.priority").props.value)).toBe("0");
  change(field("settings.rules.name"), { value: "New fixture rule" });
  change(field("settings.rules.extensions"), { value: "fixture" });
  change(field("settings.rules.destination"), { value: "Filed" });
  await submit();

  expect(harness.invoke).toHaveBeenCalledWith("add_rule_cmd", { rule: expect.objectContaining({ priority: 0 }) });
  expect(db.get(1)).toEqual(invoice);
});

it("honors a later editor enable change and saves a different row independently", async () => {
  edit("Invoices");
  toggle("Invoices");
  await settle();
  change(checkbox("settings.rules.enabled"), { checked: true });
  toggle("Receipts");
  await settle();
  expect(checkbox("settings.rules.enabled").props.checked).toBe(true);
  await submit();

  expect(db.get(1)).toEqual(invoice);
  expect(db.get(2)).toEqual({ ...receipts, enabled: false });
});

it("cancel discards the name draft while keeping the committed switch on reopen", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Abandoned name" });
  toggle("Invoices");
  await settle();
  click(requireElement(button("common.cancel")));

  expect(db.get(1)).toEqual({ ...invoice, enabled: false });
  edit("Invoices");
  expect(field("settings.rules.name").props.value).toBe("Invoices");
  expect(checkbox("settings.rules.enabled").props.checked).toBe(false);
});

it("dismisses the confirmed deleted rule draft and preserves a different draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Unsaved invoice draft" });
  await remove("Receipts");
  expect(field("settings.rules.name").props.value).toBe("Unsaved invoice draft");
  await remove("Invoices");

  expect(db.size).toBe(0);
  expect(find(editor(), nameField)).toBeUndefined();
});

it("dismisses a replaced saved draft and preserves an unrelated new draft", async () => {
  vi.useFakeTimers();
  edit("Invoices");
  change(checkbox("settings.rules.replaceOnImport"), { checked: true });
  await click(requireElement(button("settings.rules.import")));
  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();

  click(requireElement(button("settings.rules.add")));
  change(field("settings.rules.name"), { value: "Unsaved new draft" });
  await click(requireElement(button("settings.rules.import")));
  expect(field("settings.rules.name").props.value).toBe("Unsaved new draft");
});

it("labels the submit action Save and reserves Edit for opening rows", () => {
  edit("Invoices");
  expect(find(editor(), button("settings.rules.save"))).toBeDefined();
  expect(find(editor(), button("settings.rules.edit"))).toBeUndefined();
});

it("shows a missing-target rejection without closing the unsaved draft", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Unsaved invoice draft" });
  db.delete(1);
  await submit();

  expect(field("settings.rules.name").props.value).toBe("Unsaved invoice draft");
  expect(text(requireElement(({ props }) => props.role === "alert"))).toBe("This rule no longer exists");
  expect(db.has(1)).toBe(false);
});

it("retains an acknowledged switch when the subsequent rule list read fails", async () => {
  edit("Invoices");
  change(field("settings.rules.name"), { value: "Invoices revised" });
  ruleReadFails = true;
  toggle("Invoices");
  await settle();
  expect(db.get(1)?.enabled).toBe(false);
  await submit();

  expect(db.get(1)).toEqual({ ...invoice, name: "Invoices revised", enabled: false });
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("finishes an acknowledged new rule despite a failed reload and retries only the list", async () => {
  click(requireElement(button("settings.rules.add")));
  change(field("settings.rules.name"), { value: "New fixture rule" });
  change(field("settings.rules.extensions"), { value: "mzt" });
  change(field("settings.rules.destination"), { value: "Sorted" });
  ruleReadFails = true;
  await submit();

  expect([...db.values()].filter((rule) => rule.name === "New fixture rule")).toHaveLength(1);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
  ruleReadFails = false;
  await click(requireElement(button("common.retry")));
  expect(text(editor())).not.toContain("settings.rules.refreshAfterWriteError");
  expect(harness.invoke.mock.calls.filter(([command]) => command === "add_rule_cmd")).toHaveLength(1);
});

it("invalidates a replaced persisted draft even when its post-commit list read fails", async () => {
  vi.useFakeTimers();
  edit("Invoices");
  change(checkbox("settings.rules.replaceOnImport"), { checked: true });
  ruleReadFails = true;
  await click(requireElement(button("settings.rules.import")));

  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("dismisses an acknowledged deleted draft even if the new list was unavailable", async () => {
  edit("Invoices");
  ruleReadFails = true;
  await expect(remove("Invoices")).resolves.toBeUndefined();

  expect(db.has(1)).toBe(false);
  expect(find(editor(), nameField)).toBeUndefined();
  expect(text(editor())).toContain("settings.rules.refreshAfterWriteError");
});

it("does not allow an older list read to restore a previous rule state", async () => {
  let finishOldRead!: (rows: Rule[]) => void;
  const oldReply = new Promise<Rule[]>((resolve) => { finishOldRead = resolve; });
  harness.invoke.mockReturnValueOnce(oldReply);
  const stale = useAppStore.getState().loadRules();
  toggle("Invoices");
  await settle();

  finishOldRead([invoice, receipts]);
  await stale;
  expect(useAppStore.getState().rules.find(({ id }) => id === 1)?.enabled).toBe(false);
});

it.each(["loading", "failed", "ready"] as const)("keeps an unknown folder ID distinct from global scope when the folder list is %s", async (status) => {
  const folderId = 37;
  useAppStore.setState({
    rules: [{ ...invoice, folder_id: folderId }],
    folders: [],
    folderRead: status === "failed" ? { status, error: "Fixture read failed" } : { status },
  });
  edit("Invoices");

  const form = editor();
  const option = requireElement((node) => node.type === "option" && node.props.value === folderId, form);
  expect(text(option)).toBe(status === "ready" ? "beta.removedRuleFolder" : "beta.ruleFolderById");
  expect(option.props.disabled).toBe(true);
  const scope = requireElement((node) => node.type === "select" && node.props.value === folderId, form);
  expect(text(requireElement((node) => node.type === "option" && node.props.value === 0, scope))).toBe("beta.allWatched");
  const save = requireElement(button("settings.rules.save"), form);
  expect(save.props.disabled).toBe(true);
  await click(save);
  expect(harness.invoke).not.toHaveBeenCalledWith("update_rule_cmd", expect.anything());
});

it("displays a live rule's exact folder and permits an explicit scope change", async () => {
  const folder = { id: 41, path: "C:/Fixture/Receipts", enabled: true, mode: "manual" };
  const scopedInvoice = { ...invoice, id: 1, folder_id: folder.id };
  db.set(scopedInvoice.id, scopedInvoice);
  useAppStore.setState({ rules: [scopedInvoice], folders: [folder], folderRead: { status: "ready" } });
  expect(text(editor())).toContain(folder.path);
  edit("Invoices");
  const select = requireElement((node) => node.type === "select" && node.props.value === folder.id);
  expect(text(requireElement((node) => node.type === "option" && node.props.value === folder.id, select))).toBe(folder.path);
  expect(requireElement(button("settings.rules.save")).props.disabled).toBe(false);
  change(select, { value: "0" });
  await submit();
  expect(harness.invoke).toHaveBeenCalledWith("update_rule_cmd", { rule: { ...scopedInvoice, folder_id: 0 } });
  expect(db.get(scopedInvoice.id)?.folder_id).toBe(0);
});

const extensionsField = () => field("settings.rules.extensions");

it("preserves extension keystrokes and sends the existing trimmed array on Save", async () => {
  edit("Invoices");
  expect(extensionsField().props.value).toBe("fixture");
  for (const value of ["fixture,", "fixture, ", "fixture,", "fixture", " .JPEG,  tif , JPEG,, "]) {
    change(extensionsField(), { value });
    expect(extensionsField().props.value).toBe(value);
  }
  change(field("settings.rules.name"), { value: "Photo invoices" });
  expect(extensionsField().props.value).toBe(" .JPEG,  tif , JPEG,, ");
  expect(harness.invoke).not.toHaveBeenCalled();

  ruleReadFails = true;
  await submit();
  expect(harness.invoke).toHaveBeenCalledWith("update_rule_cmd", {
    rule: { ...invoice, name: "Photo invoices", extensions: [".JPEG", "tif", "JPEG", "", ""] },
  });
  edit("Photo invoices");
  expect(extensionsField().props.value).toBe("jpeg, tif");
});

it("keeps raw Extensions through tab and row edits, then resets on Cancel or a new rule", async () => {
  edit("Invoices");
  const draft = "  acme,    b";
  change(extensionsField(), { value: draft });
  click(requireElement(({ props }) => props.label === "settings.general.title"));
  expect(find(editor(), (node) => node.props["aria-label"] === "settings.rules.extensions")).toBeUndefined();
  click(requireElement(({ props }) => props.label === "settings.rules.title"));
  expect(extensionsField().props.value).toBe(draft);

  toggle("Invoices");
  await settle();
  expect(extensionsField().props.value).toBe(draft);
  expect(db.get(1)).toEqual({ ...invoice, enabled: false });
  click(requireElement(button("common.cancel")));
  edit("Invoices");
  expect(extensionsField().props.value).toBe("fixture");
  change(extensionsField(), { value: draft });
  edit("Receipts");
  expect(extensionsField().props.value).toBe("fixture");
  change(extensionsField(), { value: draft });
  click(requireElement(button("settings.rules.add")));
  expect(extensionsField().props.value).toBe("");
  change(field("settings.rules.name"), { value: "New exclusions" });
  chooseRuleAction("ignore");
  change(extensionsField(), { value: "  .PNG,jpg, " });
  await submit();
  expect(db.get(10)?.extensions).toEqual([".PNG", "jpg", ""]);
  expect(db.get(2)).toEqual(receipts);
});

it("keeps the exact extension draft after Save rejects it, then sends the corrected array", async () => {
  edit("Invoices");
  change(extensionsField(), { value: "  ,," });
  harness.invoke.mockRejectedValueOnce("Provide at least one extension or *");
  await submit();
  expect(harness.invoke).toHaveBeenCalledWith("update_rule_cmd", { rule: { ...invoice, extensions: ["", "", ""] } });
  expect(extensionsField().props.value).toBe("  ,,");
  expect(db.get(1)).toEqual(invoice);

  change(extensionsField(), { value: " .PDF, jpg " });
  await submit();
  expect(db.get(1)?.extensions).toEqual([".PDF", "jpg"]);
});

const normalizationLabel = "settings.rules.normalizeExtensions";
const normalizationMapping = "JPEG:JPG, tif:tiff";

function chooseRuleAction(value: string) {
  change(requireElement((node) => node.type === "select"
    && !!find(node, (option) => option.type === "option" && option.props.value === "rename")), { value });
}

function expectNormalizationHidden() {
  const form = editor();
  const checkboxLabel = (node: Element) => node.type === "label" && text(node.props.children) === normalizationLabel;
  const mappingInput = (node: Element) => node.type === "input" && node.props["aria-label"] === normalizationLabel;
  expect(find(form, checkboxLabel)).toBeUndefined();
  expect(find(form, mappingInput)).toBeUndefined();
}

it("keeps a new Move mapping while switching the rule draft through Rename and Ignore", async () => {
  click(requireElement(button("settings.rules.add")));
  change(field("settings.rules.name"), { value: "Normalized photos" });
  change(field("settings.rules.extensions"), { value: "jpeg, tif" });
  change(field("settings.rules.destination"), { value: "Photos" });
  expect(checkbox(normalizationLabel).props.checked).toBe(false);
  change(checkbox(normalizationLabel), { checked: true });
  change(field(normalizationLabel), { value: normalizationMapping });

  for (const action of ["rename", "ignore"]) {
    chooseRuleAction(action);
    expectNormalizationHidden();
    chooseRuleAction("move");
    expect(checkbox(normalizationLabel).props.checked).toBe(true);
    expect(field(normalizationLabel).props.value).toBe(normalizationMapping);
  }

  expect(harness.invoke).not.toHaveBeenCalled();
  await submit();
  expect(db.get(10)).toMatchObject({ action: "move", normalize_extensions: true, extension_mappings: normalizationMapping });
  edit("Normalized photos");
  expect(field(normalizationLabel).props.value).toBe(normalizationMapping);
});

it.each(["rename", "ignore", "delete"])("hides and preserves a saved inactive mapping on an existing %s rule", async (action) => {
  const existing = { ...invoice, action, normalize_extensions: true, extension_mappings: normalizationMapping };
  db.set(1, existing);
  useAppStore.setState({ rules: [...db.values()] });
  edit("Invoices");
  expectNormalizationHidden();
  change(field("settings.rules.name"), { value: "Updated invoices" });
  await submit();
  expect(db.get(1)).toEqual({ ...existing, name: "Updated invoices" });

  edit("Updated invoices");
  chooseRuleAction("move");
  expect(checkbox(normalizationLabel).props.checked).toBe(true);
  expect(field(normalizationLabel).props.value).toBe(normalizationMapping);
});

const folderA = "C:/Fixture/Manual A";
const folderB = "C:/Fixture/Manual B";
const ignoreFolder = () => requireElement((node) => node.type === "select" && node.props["aria-label"] === "settings.ignore.folder", ignoreEditor());
const ignoreButton = (key: string) => requireElement(button(`settings.ignore.${key}`), ignoreEditor());
const ignoreField = () => requireElement((node) => node.type === "input" && node.props["aria-label"] === "settings.ignore.patterns", ignoreEditor());
const typePattern = (value: string) => change(ignoreField(), { value });
const patternRow = (pattern: string): Match => (node) => node.props["aria-label"] === `settings.ignore.remove: ${pattern}`;
const removePattern = (pattern: string) => click(requireElement(patternRow(pattern), ignoreEditor()));

function ignoreEditor() {
  const child = find(editor(), (node) => typeof node.type === "function" && node.type.name === "IgnoreTab");
  if (!child) {
    disposeIgnoreEffects();
    harness.slots.length = harness.cursor;
    return null;
  }
  return renderEffectfulChild(child);
}

function renderEffectfulChild(child: Element) {
  return renderWithEffects(() => (child.type as (props: Props) => ReactNode)(child.props));
}

function renderWithEffects(render: () => ReactNode) {
  const effects: (() => void)[] = [];
  harness.effects = effects;
  try {
    return render();
  } finally {
    harness.effects = null;
    effects.forEach((run) => run());
  }
}

function reviewSettingsReply(command: string) {
  switch (command) {
    case "get_settings_cmd": return scheduleFixture;
    case "get_rules_cmd":
    case "get_folders_cmd":
    case "get_logs_cmd":
    case "get_stats_cmd":
    case "get_pending_files_cmd": return [];
    case "get_watcher_issues_cmd": return {};
    case "explorer_integration_status_cmd": return [false, false];
    default: return null;
  }
}

it("reveals a routed review across a Settings mount and keeps the ordinary sidebar queue broad", async () => {
  const scoped = ["C:/Fixture/selected.txt"];
  let initialPaths: string[] | null = scoped;
  useReviewQueueStore.setState(useReviewQueueStore.getInitialState());
  harness.invoke.mockImplementation(async (command: string) => {
    if (command === "take_review_request_cmd") {
      const paths = initialPaths;
      initialPaths = null;
      return paths;
    }
    if (command === "preview_cmd") return { id: "fixture-preview", entries: [] };
    return reviewSettingsReply(command);
  });

  // The native reader from a prior Settings mount has already taken the request.
  await useReviewQueueStore.getState().readReviewRequest();
  const settings = () => renderWithEffects(editor);
  const review = () => find(settings(), (node) => node.type === ReviewQueue);
  settings();
  await settle();
  expect(review(), "the retained request should open the review tab").toBeDefined();
  renderEffectfulChild(review()!);
  await settle();
  expect(harness.invoke).toHaveBeenLastCalledWith("preview_cmd", { paths: scoped });

  click(requireElement(({ props }) => props.label === "settings.folders.title", settings()));
  expect(review()).toBeUndefined();
  click(requireElement(({ props }) => props.label === "beta.review", settings()));
  renderEffectfulChild(review()!);
  await settle();
  expect(harness.invoke).toHaveBeenLastCalledWith("preview_cmd", { paths: null });
});

it("keeps an incoming scope behind an approved receipt and later reopens the full sidebar queue", async () => {
  const firstScope = ["C:/Fixture/first.txt"];
  const nextScope = ["C:/Fixture/next.txt"];
  let requested: string[] | null = firstScope;
  let resolveApply!: (rows: OperationResult[]) => void;
  const applying = new Promise<OperationResult[]>(resolve => { resolveApply = resolve; });
  const destination = "C:/Fixture/Filed/first.txt";
  const confirmedRow = {
    id: "chosen", selectionKey: firstScope[0], source: firstScope[0], destination,
    rule: "Fixture route", action: "move", size: 1, warning: false, error: null,
  };
  useReviewQueueStore.setState(useReviewQueueStore.getInitialState());
  harness.invoke.mockImplementation(async (command: string, args?: { paths: string[] | null }) => {
    if (command === "take_review_request_cmd") {
      const paths = requested;
      requested = null;
      return paths;
    }
    if (command === "preview_cmd") return { id: "current-plan", entries: args?.paths === firstScope ? [confirmedRow] : [] };
    if (command === "apply_preview_cmd") return applying;
    return reviewSettingsReply(command);
  });
  const settings = () => renderWithEffects(editor);
  const sidebar = (label: string) => click(requireElement(({ props }) => props.label === label, settings()));
  const review = () => renderEffectfulChild(requireElement(node => node.type === ReviewQueue, settings()));
  const requestedPreviews = () => harness.invoke.mock.calls.filter(([command]) => command === "preview_cmd");

  settings();
  await settle();
  review();
  await settle();
  expect(requestedPreviews()).toEqual([["preview_cmd", { paths: firstScope }]]);

  click(requireElement(button("beta.apply"), review()));
  const confirmation = requireElement(node => node.type === ConfirmDialog, review());
  const submitted = confirmation.props.onConfirm!();
  expect(harness.invoke).toHaveBeenCalledWith("apply_preview_cmd", { id: "current-plan", selected: ["chosen"] });
  sidebar("settings.folders.title");
  requested = nextScope;
  const onReview = harness.listen.mock.calls.find(([event]) => event === "open-review")?.[1];
  expect(onReview).toBeTypeOf("function");
  onReview();
  await settle();
  expect(text(review())).toContain("confirm.working");
  expect(requireElement(button("beta.refresh"), review()).props.disabled).toBe(true);

  sidebar("settings.folders.title");
  const receipt = { id: "chosen", source: firstScope[0], actualDestination: destination, success: true, error: null };
  resolveApply([receipt]);
  await submitted;
  confirmation.props.onClose!();
  sidebar("beta.review");
  const result = requireElement(node => node.type === Results, review());
  expect(result.props).toMatchObject({ results: [receipt] });
  expect(requestedPreviews()).toHaveLength(1);

  requested = [...nextScope];
  onReview();
  await settle();
  expect(requireElement(node => node.type === Results, review()).props).toMatchObject({ results: [receipt] });
  expect(requestedPreviews()).toHaveLength(1);

  await click(requireElement(button("beta.refresh"), review()));
  expect(requestedPreviews()).toEqual([["preview_cmd", { paths: firstScope }], ["preview_cmd", { paths: nextScope }]]);
  sidebar("settings.folders.title");
  sidebar("beta.review");
  review();
  await settle();
  expect(requestedPreviews().slice(-1)[0]).toEqual(["preview_cmd", { paths: null }]);
});

it("immediately opens the next native review after a same-view receipt was presented and the user visited History", async () => {
  const first = "C:/Fixture/seen.txt";
  const next = "C:/Fixture/fresh.txt";
  let requested: string[] | null = [first];
  const proposal = (source: string) => ({
    id: source, selectionKey: source, source, destination: source + ".filed",
    rule: "Fixture route", action: "move", size: 1, warning: false, error: null,
  });
  const outcome = { id: first, source: first, success: true, error: null, actualDestination: first + ".filed" };
  useReviewQueueStore.setState(useReviewQueueStore.getInitialState());
  harness.invoke.mockImplementation(async (command: string, args?: { paths: string[] | null }) => {
    if (command === "take_review_request_cmd") {
      const paths = requested;
      requested = null;
      return paths;
    }
    if (command === "preview_cmd") return { id: "fixture-plan", entries: (args?.paths ?? []).map(proposal) };
    if (command === "apply_preview_cmd") return [outcome];
    return reviewSettingsReply(command);
  });
  const settings = () => renderWithEffects(editor);
  const sidebar = (label: string) => click(requireElement(({ props }) => props.label === label, settings()));
  const review = () => renderEffectfulChild(requireElement(node => node.type === ReviewQueue, settings()));

  settings();
  await settle();
  review();
  await settle();
  click(requireElement(button("beta.apply"), review()));
  const confirmation = requireElement(node => node.type === ConfirmDialog, review());
  await confirmation.props.onConfirm!();
  confirmation.props.onClose!();
  expect(requireElement(node => node.type === Results, review()).props).toMatchObject({ results: [outcome] });

  sidebar("settings.history.title");
  sidebar("beta.review");
  expect(requireElement(node => node.type === Results, review()).props).toMatchObject({ results: [outcome] });
  sidebar("settings.history.title");

  requested = [next];
  const onReview = harness.listen.mock.calls.find(([event]) => event === "open-review")?.[1];
  expect(onReview).toBeTypeOf("function");
  onReview();
  await settle();
  review();
  await settle();
  expect(harness.invoke).toHaveBeenCalledWith("preview_cmd", { paths: [next] });
  expect(text(review())).toContain(next);
  expect(requireElement(node => node.type === Results, review()).props).toMatchObject({ results: [] });
  expect(harness.invoke.mock.calls.filter(([command]) => command === "apply_preview_cmd")).toHaveLength(1);
});

function visitSettingsTab(label: string) {
  click(requireElement(({ props }) => props.label === label));
  return ignoreEditor();
}

async function selectIgnoreFolder(value: string) {
  const select = ignoreFolder();
  expect(select.props.disabled).toBe(false);
  change(select, { value });
  ignoreEditor();
  await settle();
}

async function openIgnore() {
  const files = new Map([[folderA, ["existing.tmp"]], [folderB, ["sibling.txt"]]]);
  useAppStore.setState({
    folders: [
      { id: 1, path: folderA, enabled: true, mode: "manual" },
      { id: 2, path: folderB, enabled: true, mode: "manual" },
    ],
    folderRead: { status: "ready" },
  });
  harness.invoke.mockImplementation(async (command: string, { folderPath, patterns, expectedPatterns }: {
    folderPath: string; patterns?: string[]; expectedPatterns?: string[];
  }) => {
    if (!files.has(folderPath)) throw new Error(`Unexpected fixture folder ${folderPath}`);
    if (command === "load_mouziignore_cmd") return [...files.get(folderPath)!];
    if (command === "save_mouziignore_cmd" && patterns) {
      const current = files.get(folderPath)!;
      if (expectedPatterns && JSON.stringify(current) !== JSON.stringify(expectedPatterns)) {
        return { status: "conflict", patterns: [...current] };
      }
      files.set(folderPath, [...patterns]);
      return expectedPatterns ? { status: "saved", patterns: [...patterns] } : undefined;
    }
    throw new Error(`Unexpected fixture command ${command}`);
  });
  visitSettingsTab("settings.ignore.title");
  await selectIgnoreFolder(folderA);
  return files;
}

it("keeps editor changes and the Ignore draft until the user reviews and saves again", async () => {
  const files = await openIgnore();
  typePattern("draft.tmp");
  files.set(folderA, ["existing.tmp", "editor.tmp"]);

  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["existing.tmp", "editor.tmp"]);
  expect(ignoreField().props.value).toBe("draft.tmp");
  expect(ignoreButton("save").props.disabled).toBe(true);
  await click(ignoreButton("reviewWithChanges"));
  expect(requireElement(patternRow("editor.tmp"), ignoreEditor())).toBeDefined();
  expect(ignoreField().props.value).toBe("draft.tmp");

  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["existing.tmp", "editor.tmp", "draft.tmp"]);
  expect(ignoreButton("saved")).toBeDefined();
});

it("keeps externally removed Ignore rules out of the reviewed draft", async () => {
  const files = await openIgnore();
  typePattern("draft.tmp");
  await click(ignoreButton("add"));
  files.set(folderA, ["editor.tmp"]);

  await click(ignoreButton("save"));
  await click(ignoreButton("reviewWithChanges"));
  expect(find(ignoreEditor(), patternRow("existing.tmp"))).toBeUndefined();
  expect(requireElement(patternRow("draft.tmp"), ignoreEditor())).toBeDefined();
  await click(ignoreButton("save"));

  expect(files.get(folderA)).toEqual(["editor.tmp", "draft.tmp"]);
});

it("saves the new pattern typed into Ignore through the visible Save button", async () => {
  const files = await openIgnore();
  typePattern("  *.tmp  ");
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(harness.invoke.mock.calls).toEqual([["load_mouziignore_cmd", { folderPath: folderA }]]);
  const save = ignoreButton("save");
  expect(save.props.disabled).toBe(false);
  await click(save);
  expect(ignoreButton("saved")).toBeDefined();
  expect(files.get(folderA)).toEqual(["existing.tmp", "*.tmp"]);
  expect(files.get(folderB)).toEqual(["sibling.txt"]);
  expect(ignoreField().props.value).toBe("");
  await selectIgnoreFolder(folderB);
  expect(requireElement(patternRow("sibling.txt"), ignoreEditor())).toBeDefined();
  await selectIgnoreFolder(folderA);
  expect(requireElement(patternRow("*.tmp"), ignoreEditor())).toBeDefined();
});

it("stages Add and Enter without writing and saves duplicate and empty-list edits", async () => {
  const files = await openIgnore();
  typePattern(" caches/  ");
  click(ignoreButton("add"));
  typePattern("receipt.tmp");
  ignoreField().props.onKeyDown!({ key: "Enter" });
  typePattern("  receipt.tmp ");
  click(ignoreButton("add"));
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(harness.invoke).toHaveBeenCalledTimes(1);
  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["existing.tmp", "caches/", "receipt.tmp"]);

  removePattern("existing.tmp");
  removePattern("caches/");
  removePattern("receipt.tmp");
  typePattern("  ");
  expect(ignoreButton("add").props.disabled).toBe(true);
  expect(files.get(folderA)).toEqual(["existing.tmp", "caches/", "receipt.tmp"]);
  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual([]);
});

it("clears Ignore's Saved feedback when the input changes", async () => {
  const files = await openIgnore();
  await click(ignoreButton("save"));
  expect(ignoreButton("saved")).toBeDefined();
  typePattern("fresh.tmp");
  expect(ignoreButton("save")).toBeDefined();
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(harness.invoke).toHaveBeenCalledTimes(2);
});

it("keeps staged removals and typed Ignore text after a write failure and retries them", async () => {
  const files = await openIgnore();
  removePattern("existing.tmp");
  typePattern("retry.tmp");
  harness.invoke.mockRejectedValueOnce("Fixture write failed");
  const failed = click(ignoreButton("save"));
  expect(requireElement(button("common.saving"), ignoreEditor()).props.disabled).toBe(true);
  await failed;
  expect(text(requireElement((node) => node.props.role === "alert", ignoreEditor()))).toBe("Fixture write failed");
  expect(ignoreField().props.value).toBe("retry.tmp");
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(find(ignoreEditor(), patternRow("existing.tmp"))).toBeUndefined();
  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["retry.tmp"]);
  expect(ignoreButton("saved")).toBeDefined();
  expect(find(ignoreEditor(), (node) => node.props.role === "alert")).toBeUndefined();
});

it("keeps independent unsaved Ignore drafts across folder selections", async () => {
  const files = await openIgnore();
  removePattern("existing.tmp");
  typePattern("cache/");
  click(ignoreButton("add"));
  typePattern("  *.log  ");

  await selectIgnoreFolder(folderB);
  expect(ignoreField().props.value).toBe("");
  expect(find(ignoreEditor(), patternRow("cache/"))).toBeUndefined();
  typePattern("b-only.txt");
  ignoreField().props.onKeyDown!({ key: "Enter" });
  typePattern("  b-pending.txt ");

  await selectIgnoreFolder(folderA);
  expect(ignoreField().props.value).toBe("  *.log  ");
  expect(find(ignoreEditor(), patternRow("cache/"))).toBeDefined();
  expect(find(ignoreEditor(), patternRow("existing.tmp"))).toBeUndefined();
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(files.get(folderB)).toEqual(["sibling.txt"]);
  expect(harness.invoke.mock.calls.some(([command]) => command === "save_mouziignore_cmd")).toBe(false);

  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["cache/", "*.log"]);
  expect(files.get(folderB)).toEqual(["sibling.txt"]);
  await selectIgnoreFolder(folderB);
  expect(ignoreField().props.value).toBe("  b-pending.txt ");
  expect(find(ignoreEditor(), patternRow("b-only.txt"))).toBeDefined();
  await click(ignoreButton("save"));
  expect(files.get(folderB)).toEqual(["sibling.txt", "b-only.txt", "b-pending.txt"]);
});

it("preserves the selected folder and unsaved Ignore draft through another Settings tab", async () => {
  const files = await openIgnore();
  removePattern("existing.tmp");
  typePattern("  pending.tmp ");
  expect(visitSettingsTab("settings.folders.title")).toBeNull();
  visitSettingsTab("settings.ignore.title");
  await settle();
  expect(ignoreFolder().props.value).toBe(folderA);
  expect(ignoreField().props.value).toBe("  pending.tmp ");
  expect(find(ignoreEditor(), patternRow("existing.tmp"))).toBeUndefined();
  expect(files.get(folderA)).toEqual(["existing.tmp"]);
  expect(harness.invoke.mock.calls).toEqual([["load_mouziignore_cmd", { folderPath: folderA }]]);
});

it("rereads a clean Ignore folder after reverting changes and after saving", async () => {
  const files = await openIgnore();
  typePattern("reverted.tmp");
  click(ignoreButton("add"));
  removePattern("reverted.tmp");
  typePattern("reverted input");
  typePattern("");
  await selectIgnoreFolder(folderB);
  files.set(folderA, ["external-first.tmp"]);
  await selectIgnoreFolder(folderA);
  expect(find(ignoreEditor(), patternRow("external-first.tmp"))).toBeDefined();
  typePattern("saved.tmp");
  await click(ignoreButton("save"));
  expect(files.get(folderA)).toEqual(["external-first.tmp", "saved.tmp"]);
  visitSettingsTab("settings.folders.title");
  files.set(folderA, ["external-second.tmp"]);
  visitSettingsTab("settings.ignore.title");
  if (!ignoreFolder().props.value) await selectIgnoreFolder(folderA);
  await settle();
  expect(find(ignoreEditor(), patternRow("external-second.tmp"))).toBeDefined();
  expect(find(ignoreEditor(), patternRow("saved.tmp"))).toBeUndefined();
});

it.each([true, false])("drops the old Ignore draft after folder re-registration (observed removal: %s)", async (observedRemoval) => {
  const files = await openIgnore();
  const [original, sibling] = useAppStore.getState().folders;
  removePattern("existing.tmp");
  typePattern("old-registration.tmp");
  visitSettingsTab("settings.folders.title");
  if (observedRemoval) {
    useAppStore.setState({ folders: [sibling] });
    ignoreEditor();
  }
  files.set(folderA, ["new-registration.tmp"]);
  useAppStore.setState({ folders: [{ ...original, id: 17 }, sibling] });
  visitSettingsTab("settings.ignore.title");
  if (!ignoreFolder().props.value) await selectIgnoreFolder(folderA);
  await settle();
  expect(ignoreField().props.value).toBe("");
  expect(find(ignoreEditor(), patternRow("new-registration.tmp"))).toBeDefined();
  expect(files.get(folderA)).toEqual(["new-registration.tmp"]);
  expect(harness.invoke.mock.calls.some(([command]) => command === "save_mouziignore_cmd")).toBe(false);
});

it("does not restore an unsaved Ignore draft after a write completes on another Settings tab", async () => {
  const files = await openIgnore();
  typePattern("saved-while-away.tmp");
  let finishWrite!: () => void;
  harness.invoke.mockImplementationOnce((command, args) => {
    expect(command).toBe("save_mouziignore_cmd");
    expect(args).toEqual({ folderPath: folderA, patterns: ["existing.tmp", "saved-while-away.tmp"], expectedPatterns: ["existing.tmp"] });
    return new Promise<void>((resolve) => {
      finishWrite = () => {
        files.set(folderA, args.patterns);
        resolve();
      };
    });
  });
  const pending = click(ignoreButton("save"));
  visitSettingsTab("settings.folders.title");
  finishWrite();
  await pending;
  expect(files.get(folderA)).toEqual(["existing.tmp", "saved-while-away.tmp"]);
  files.set(folderA, ["external-after-write.tmp"]);
  visitSettingsTab("settings.ignore.title");
  if (!ignoreFolder().props.value) await selectIgnoreFolder(folderA);
  await settle();
  expect(ignoreField().props.value).toBe("");
  expect(find(ignoreEditor(), patternRow("external-after-write.tmp"))).toBeDefined();
});
