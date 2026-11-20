import { useTranslation } from "react-i18next";

export default function UnavailableCleanupRoots({
  paths,
  noMatches,
}: {
  paths?: string[];
  noMatches: boolean;
}) {
  const { t } = useTranslation();
  if (!paths?.length) return null;

  return (
    <div role="status" className="space-y-1 text-sm text-amber-800 dark:text-amber-200">
      {noMatches && <p>{t("cleanup.noMatchesInAvailableFolders")}</p>}
      <p>{t("cleanup.unavailableFolders")}</p>
      <ul className="list-disc space-y-1 pl-5">
        {paths.map((path) => <li key={path} className="break-all">{path}</li>)}
      </ul>
    </div>
  );
}
