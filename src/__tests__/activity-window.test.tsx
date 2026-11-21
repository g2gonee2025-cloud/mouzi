import { afterEach, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import ActivityTimeline from "../components/dashboard/ActivityTimeline";
import Popup from "../components/Popup";
import { useAppStore } from "../store/useAppStore";

type ActivityMessages = Record<string, Record<string, string>>;
const locales = import.meta.glob<ActivityMessages>("../i18n/locales/*.json", {
  eager: true, import: "default",
});
let messages: ActivityMessages;
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => {
      const [scope, label] = key.split(".");
      return messages[scope]?.[label] ?? key;
    },
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));
vi.mock("../store/useAppStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../store/useAppStore")>();
  const readSnapshot = (select?: (state: ReturnType<typeof actual.useAppStore.getState>) => unknown) => {
    const state = actual.useAppStore.getState();
    return select ? select(state) : state;
  };
  return { ...actual, useAppStore: Object.assign(readSnapshot, actual.useAppStore) };
});
const activity = [{ file_type: "Friday invoices", count: 2 }];

afterEach(() => {
  useAppStore.setState(useAppStore.getInitialState(), true);
});

function renderActivity(locale: ActivityMessages) {
  messages = locale;
  useAppStore.setState({ stats: activity, folderRead: { status: "ready" } });
  const populated = renderToStaticMarkup(<ActivityTimeline data={activity} />);
  const empty = renderToStaticMarkup(<ActivityTimeline data={[]} />);
  const tray = renderToStaticMarkup(<Popup />);
  useAppStore.setState({ stats: [] });
  return { populated, empty, tray, emptyTray: renderToStaticMarkup(<Popup />) };
}

it.each(Object.entries(locales))("renders a seven-day window in Dashboard and tray for %s", (_path, locale) => {
  const { populated, empty, tray, emptyTray } = renderActivity(locale);
  const heading = locale.dashboard.organizedThisWeek;
  const noActivity = locale.dashboard.noOrganized;
  const trayHeading = locale.popup.weeklyStats;

  expect(populated).toContain(heading);
  expect(populated).toContain("Friday invoices");
  expect(populated).toContain(">2<");
  expect(empty).toContain(heading);
  expect(empty).toContain(noActivity);
  expect(empty).not.toContain("Friday invoices");
  expect(tray).toContain(trayHeading);
  expect(tray).toContain("Friday invoices");
  expect(emptyTray).not.toContain(trayHeading);
  expect([heading, noActivity, trayHeading].every((label) => label.includes("7"))).toBe(true);
});

it("names the elapsed window in the visible English labels", () => {
  const { populated, empty, tray } = renderActivity(locales["../i18n/locales/en.json"]);
  expect(populated).toContain(">Organized in the last 7 days</h3>");
  expect(empty).toContain(">No files organized in the last 7 days</div>");
  expect(tray).toContain(">Last 7 days</div>");
});
