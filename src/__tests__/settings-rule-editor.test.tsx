import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Settings from "../components/Settings";
import { useAppStore, type AppSettings, type Rule, type ScheduleSettings } from "../store/useAppStore";

const harness = vi.hoisted(() => ({
  invoke: vi.fn(),
  slots: [] as unknown[],
  cursor: 0,
  effects: null as (() => void)[] | null,
  disposeEffects: new Set<() => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: harness.invoke }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: { number?: number }) => key === "settings.scheduler.time"
      ? `${key} ${values?.number}`
      : key,
  }),
}));
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

interface Props {
  children?: ReactNode;
  type?: string;
  value?: string | number;
  disabled?: boolean;
  checked?: boolean;
  label?: string;
  role?: string;
  title?: string;
  "aria-label"?: string;
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

beforeEach(() => {
  harness.slots.length = 0;
  harness.invoke.mockReset();
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

const scheduleFixture: AppSettings = {
  id: 1, revision: 4, language: "en", theme: "system", telemetry_enabled: false,
  first_run: false, autostart: false, grace_period_seconds: 300,
  lock_check_enabled: true, recursive_watch: false, local_ai_enabled: false, local_ai_model: "llama3.2",
  schedule_enabled: true, schedule_times_per_day: 2, schedule_time_1: "21:17", schedule_time_2: "23:19",
  schedule_time_3: null, schedule_time_4: null,
};

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
  const effects: (() => void)[] = [];
  harness.effects = effects;
  try {
    return (child.type as (props: Props) => ReactNode)(child.props);
  } finally {
    harness.effects = null;
    effects.forEach((run) => run());
  }
}

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
  harness.invoke.mockImplementation(async (command: string, { folderPath, patterns }: { folderPath: string; patterns?: string[] }) => {
    if (!files.has(folderPath)) throw new Error(`Unexpected fixture folder ${folderPath}`);
    if (command === "load_mouziignore_cmd") return [...files.get(folderPath)!];
    if (command === "save_mouziignore_cmd" && patterns) {
      files.set(folderPath, [...patterns]);
      return;
    }
    throw new Error(`Unexpected fixture command ${command}`);
  });
  visitSettingsTab("settings.ignore.title");
  await selectIgnoreFolder(folderA);
  return files;
}

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
    expect(args).toEqual({ folderPath: folderA, patterns: ["existing.tmp", "saved-while-away.tmp"] });
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
