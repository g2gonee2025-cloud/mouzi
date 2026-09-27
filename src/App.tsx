import { useEffect, useState } from "react";
import { parseHash } from "./utils/paths";
import { useTranslation } from "react-i18next";
import { initI18n, SupportedLang } from "./i18n";
import { useAppStore } from "./store/useAppStore";
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
  const { loadSettings, settings } = useAppStore();

  const [hash, setHash] = useState(() => parseHash().route);

  useEffect(() => {
    const onHash = () => setHash(parseHash().route);
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    async function boot() {
      await loadSettings();
    }
    boot();
  }, [loadSettings]);

  useEffect(() => {
    if (!settings) return;
    const lang = (settings.language || "en") as SupportedLang;
    initI18n(lang).then(() => setReady(true));
  }, [settings]);

  useEffect(() => {
    if (!settings) return;
    applyTheme(settings.theme);
  }, [settings?.theme]);

  useEffect(() => {
    const unlisten = listen("file-organized", () => {
      useAppStore.getState().loadLogs();
      useAppStore.getState().loadStats();
    });

    let actionListener: { unregister: () => Promise<void> } | null = null;

    onAction((notification) => {
      const destFolder = (notification.extra as Record<string, unknown> | undefined)?.destFolder as string | undefined;
      if (destFolder) {
        invoke("open_folder_cmd", { path: destFolder })
          .catch((e) => console.error("open_folder_cmd from onAction failed:", e));
      }
    }).then((listener) => {
      actionListener = listener;
    }).catch(console.error);

    const handleFocus = async () => {
      try {
        const folder = await invoke<string | null>("get_pending_open_folder_cmd");
        if (folder) {
          await invoke("open_folder_cmd", { path: folder }).catch(console.error);
        }
      } catch (e) {
        console.error("get_pending_open_folder_cmd error:", e);
      }
    };

    window.addEventListener("focus", handleFocus);

    return () => {
      unlisten.then((f) => f());
      window.removeEventListener("focus", handleFocus);
      if (actionListener) {
        actionListener.unregister().catch(console.error);
      }
    };
  }, []);

  if (!ready) {
    return (
      <div className="flex h-full items-center justify-center bg-surface text-text">
        <div className="animate-pulse text-sm">{t("app.loading")}</div>
      </div>
    );
  }

  return (
    <div className="h-full w-full">
      {hash === "settings" ? (
          <Settings />
        ) : hash === "dashboard" ? (
          <Dashboard />
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
