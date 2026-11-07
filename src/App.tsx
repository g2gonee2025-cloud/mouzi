import { useEffect, useState } from "react";
import { parseHash } from "./utils/paths";
import { DEFAULT_DASHBOARD_VIEW } from "./utils/dashboard";
import { useTranslation } from "react-i18next";
import { initI18n, SupportedLang } from "./i18n";
import { useAppStore, type AppSettings } from "./store/useAppStore";
import { useDashboardStore } from "./store/useDashboardStore";
import { useCleanupStore } from "./store/useCleanupStore";
import { useSuggestionsStore } from "./store/useSuggestionsStore";
import Popup from "./components/Popup";
import Settings from "./components/Settings";
import Dashboard from "./pages/Dashboard";
import Cleanup from "./pages/Cleanup";
import Suggestions from "./pages/Suggestions";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { onAction } from "@tauri-apps/plugin-notification";

function applyTheme(theme: string) {
  const root = document.documentElement;
  if (theme === "dark") {
    root.classList.add("dark");
  } else if (theme === "light") {
    root.classList.remove("dark");
  } else {
    const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    if (prefersDark) {
      root.classList.add("dark");
    } else {
      root.classList.remove("dark");
    }
  }
}

function App() {
  const { t } = useTranslation();
  const [ready, setReady] = useState(false);
  const [bootError, setBootError] = useState<string | null>(null);
  const { loadSettings, settings } = useAppStore();

  const [hash, setHash] = useState(() => parseHash().route);
  const [dashboardView, setDashboardView] = useState(DEFAULT_DASHBOARD_VIEW);

  useEffect(() => {
    const onHash = () => setHash(parseHash().route);
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    loadSettings().catch((e) => setBootError(String(e)));
  }, [loadSettings]);

  useEffect(() => {
    if (!settings) return;
    const lang = (settings.language || "en") as SupportedLang;
    let disposed = false;
    initI18n(lang).then(() => {
      if (!disposed) setReady(true);
    }).catch((e) => {
      if (!disposed) setBootError(String(e));
    });
    return () => { disposed = true; };
  }, [settings?.language]);

  useEffect(() => {
    if (!settings) return;
    applyTheme(settings.theme);
    if (settings.theme !== "system") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyTheme("system");
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [settings?.theme]);

  useEffect(() => {
    let disposed = false;
    const listeners: (() => void)[] = [];
    const keepListener = (unlisten: () => void) => {
      if (disposed) unlisten();
      else listeners.push(unlisten);
    };
    const refreshActivity = () => {
      if (disposed) return;
      const store = useAppStore.getState();
      Promise.all([store.loadLogs(), store.loadStats(), store.getPendingFiles()])
        .catch(console.error);
    };
    const invalidateInventory = () => {
      if (disposed) return;
      useDashboardStore.getState().refresh().catch(console.error);
      useDashboardStore.getState().syncScanState().catch(console.error);
      useCleanupStore.getState().invalidateCandidates();
      const suggestions = useSuggestionsStore.getState();
      if (!suggestions.busy) suggestions.loadSuggestions().catch(console.error);
    };
    listen("file-organized", refreshActivity).then(keepListener).catch(console.error);
    listen("mutation-completed", () => {
      refreshActivity();
      invalidateInventory();
    }).then(keepListener).catch(console.error);
    listen("folders-changed", () => {
      if (disposed) return;
      useAppStore.getState().loadFolders().catch(console.error);
      useAppStore.getState().getPendingFiles().catch(console.error);
      invalidateInventory();
    }).then(keepListener).catch(console.error);
    listen<AppSettings>("settings-changed", (event) => {
      if (disposed) return;
      if (event.payload && typeof event.payload.revision === "number") {
        useAppStore.getState().applySettings(event.payload);
      } else {
        useAppStore.getState().loadSettings().catch(console.error);
      }
    }).then(keepListener).catch(console.error);
    let actionListener: { unregister: () => Promise<void> } | null = null;

    onAction((notification) => {
      const destFolder = (notification.extra as Record<string, unknown> | undefined)?.destFolder as string | undefined;
      if (!disposed && destFolder) {
        invoke("open_folder_cmd", { path: destFolder })
          .catch((e) => console.error("open_folder_cmd from onAction failed:", e));
      }
    }).then((listener) => {
      if (disposed) listener.unregister().catch(console.error);
      else actionListener = listener;
    }).catch(console.error);

    const handleFocus = () => {
      useAppStore.getState().loadSettings().catch(console.error);
      useAppStore.getState().loadFolders().catch(console.error);
      refreshActivity();
    };

    window.addEventListener("focus", handleFocus);

    return () => {
      disposed = true;
      listeners.forEach((unlisten) => unlisten());
      window.removeEventListener("focus", handleFocus);
      if (actionListener) {
        actionListener.unregister().catch(console.error);
      }
    };
  }, []);

  if (!ready) {
    return (
      <div className="flex h-full items-center justify-center bg-surface text-text">
        {bootError ? (
          <div role="alert" className="p-4 text-sm text-red-600">
            {bootError}
            <button className="block mt-2 underline" onClick={() => {
              setBootError(null);
              loadSettings().catch((e) => setBootError(String(e)));
            }}>{t("common.retry", { defaultValue: "Retry" })}</button>
          </div>
        ) : <div className="animate-pulse text-sm">{t("app.loading")}</div>}
      </div>
    );
  }

  return (
    <div className="h-full w-full">
      {hash === "settings" ? (
          <Settings />
        ) : hash === "dashboard" ? (
          <Dashboard view={dashboardView} setView={setDashboardView} />
        ) : hash === "cleanup" ? (
          <Cleanup />
        ) : hash === "suggestions" ? (
          <Suggestions />
        ) : (
          <Popup />
        )}
    </div>
  );
}

export default App;
