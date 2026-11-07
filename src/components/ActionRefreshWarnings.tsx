import { useTranslation } from "react-i18next";
import { useAppStore } from "../store/useAppStore";

export default function ActionRefreshWarnings() {
  const { t } = useTranslation();
  const { history, historyCounts, stats } = useAppStore((state) => state.actionRefreshErrors);
  if (!history && !historyCounts && !stats) return null;

  return (
    <div role="status" className="rounded-md border border-amber-300 px-3 py-2 text-xs text-text whitespace-pre-wrap">
      {history && <p>{t("popup.recentActions")}: {history}</p>}
      {historyCounts && <p>{t("settings.history.title")}: {historyCounts}</p>}
      {stats && <p>{t("popup.weeklyStats")}: {stats}</p>}
    </div>
  );
}
