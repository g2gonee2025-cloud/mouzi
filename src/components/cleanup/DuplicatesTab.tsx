import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useCleanupStore } from "../../store/useCleanupStore";
import {
  formatBytes,
  formatTimestamp,
} from "../../utils/format";
import { fileName, parentDir } from "../../utils/paths";
import {
  duplicateCopyCount,
  CLEANUP_REVIEW_LIMIT,
  sortGroups,
  buildDuplicateActions,
  resolveKeeper,
  type DuplicateGroup,
} from "../../utils/cleanup";
import { Search, Trash2, Check } from "lucide-react";
import ResultsPanel from "./ResultsPanel";
import ConfirmDialog from "./ConfirmDialog";
import { useCleanupConfirmation } from "./useCleanupConfirmation";
import UnavailableCleanupRoots from "./UnavailableCleanupRoots";

const GROUPS_PER_PAGE = 12;
const COPIES_PER_PAGE = 25;

interface DuplicatePages {
  results: DuplicateGroup[] | null;
  groupPage: number;
  copyPages: Record<string, number>;
}

export default function DuplicatesTab() {
  const { t } = useTranslation();
  const store = useCleanupStore();
  const selected = store.duplicateKeepers;
  const victims = store.duplicateVictims;
  const receipt = store.receipts.trash_duplicate;
  const { pending, reviewNeedsSearch, clearReviewNotice, openReview } = useCleanupConfirmation();
  const findButton = useRef<HTMLButtonElement>(null);
  const resultsHeading = useRef<HTMLHeadingElement>(null);
  const [savedPages, setPages] = useState<DuplicatePages>({ results: null, groupPage: 0, copyPages: {} });

  const groups = useMemo(
    () => (store.duplicates ? sortGroups(store.duplicates) : []),
    [store.duplicates],
  );
  const unavailableRoots = store.duplicates && store.unavailableRoots.duplicates;
  const copyCount = useMemo(() => duplicateCopyCount(groups), [groups]);
  const pages = savedPages.results === store.duplicates
    ? savedPages
    : { results: store.duplicates, groupPage: 0, copyPages: {} };
  const groupPages = Math.ceil(groups.length / GROUPS_PER_PAGE);
  const visibleGroups = groups.slice(pages.groupPage * GROUPS_PER_PAGE, (pages.groupPage + 1) * GROUPS_PER_PAGE);
  const locked = store.busy || !!store.preparedPlan;

  const actions = useMemo(
    () => buildDuplicateActions(groups, selected, victims),
    [groups, selected, victims],
  );
  const selection = useMemo(() => {
    const paths = new Set(actions.map(({ path }) => path));
    const byGroup = new Map<string, { count: number; bytes: number }>();
    let bytes = 0;
    if (paths.size) {
      for (const group of groups) {
        let count = 0;
        let groupBytes = 0;
        for (const file of group.files) {
          if (!paths.has(file.path)) continue;
          count++;
          groupBytes += file.size;
        }
        if (count) byGroup.set(group.hash, { count, bytes: groupBytes });
        bytes += groupBytes;
      }
    }
    return { paths, byGroup, bytes };
  }, [actions, groups]);
  const atLimit = actions.length >= CLEANUP_REVIEW_LIMIT;

  const handleFind = () => {
    clearReviewNotice();
    return store.findDuplicates();
  };

  const handleConfirm = async (opener: HTMLButtonElement) => {
    const current = useCleanupStore.getState();
    if (actions.length === 0 || actions.length > CLEANUP_REVIEW_LIMIT || current.duplicates !== store.duplicates
        || current.duplicateKeepers !== selected || current.duplicateVictims !== victims) return;
    await openReview(actions, {
      title: t("confirm.op.trashDuplicates"),
      notice: "trash",
      opener,
      focusFallback: () => resultsHeading.current ?? findButton.current,
    });
  };

  if (receipt && !pending) {
    return (
      <ResultsPanel
        headingRef={resultsHeading}
        results={receipt.outcomes}
        onDone={() => {
          if (store.dismissReceipt("trash_duplicate", receipt.planId) && !useCleanupStore.getState().loading) {
            void handleFind();
          }
        }}
        error={receipt.error}
      />
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs text-text-muted">{t("cleanup.duplicatesHint")}</p>
        <button
          ref={findButton}
          onClick={handleFind}
          disabled={store.loading || store.busy || !!store.preparedPlan}
          className="flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-on-primary hover:bg-primary-hover disabled:opacity-50 transition-colors"
        >
          <Search size={14} />
          {store.loading
            ? t("app.loading")
            : t("cleanup.findDuplicates")}
        </button>
      </div>

      {reviewNeedsSearch && <p role="status" className="text-xs text-text-muted">{t("cleanup.reviewChanged")}</p>}
      {store.error && (
        <div className="rounded-lg bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 px-4 py-3 text-xs text-red-700 dark:text-red-400">
          {store.error}
        </div>
      )}

      <UnavailableCleanupRoots paths={unavailableRoots ?? undefined} noMatches={groups.length === 0} />

      {store.duplicates && groups.length === 0 && !unavailableRoots?.length && (
        <div className="rounded-lg border border-border bg-surface-dark p-6 text-center text-xs text-text-muted">
          {t("cleanup.noDuplicates")}
        </div>
      )}

      {groups.length > 0 && (
        <>
          <div className="flex flex-wrap items-center justify-between gap-2 px-1">
            <div className="text-xs text-text-muted">
              <span>{groups.length} {t("cleanup.groupCount")} &middot;{" "}</span>
              <span aria-live="polite">{t("cleanup.duplicateSelection", { count: actions.length, size: formatBytes(selection.bytes) })}</span>
              {copyCount > CLEANUP_REVIEW_LIMIT && (
                <p>{t("cleanup.duplicateSelectionLimit", { limit: CLEANUP_REVIEW_LIMIT.toLocaleString() })}</p>
              )}
            </div>
            <div className="flex items-center gap-2">
              {copyCount <= CLEANUP_REVIEW_LIMIT && (
                <button
                  type="button"
                  onClick={() => store.selectAllDuplicateVictims(store.duplicates)}
                  disabled={locked || actions.length === copyCount}
                  className="rounded-md px-2 py-1.5 text-xs text-text-muted hover:bg-border disabled:opacity-50"
                >
                  {t("cleanup.selectAll")}
                </button>
              )}
              <button
                type="button"
                onClick={() => store.clearDuplicateVictims(store.duplicates)}
                disabled={locked || actions.length === 0}
                className="rounded-md px-2 py-1.5 text-xs text-text-muted hover:bg-border disabled:opacity-50"
              >
                {t("beta.clearSelection")}
              </button>
              <button
                type="button"
                onClick={(event) => void handleConfirm(event.currentTarget)}
                disabled={locked || actions.length === 0 || actions.length > CLEANUP_REVIEW_LIMIT}
                className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-700 disabled:opacity-50 transition-colors"
              >
                <Trash2 size={14} />
                {store.busy ? t("app.loading") : t("cleanup.confirm", { count: actions.length })}
              </button>
            </div>
          </div>

          {groupPages > 1 && (
            <DuplicatePageNavigation
              label={t("cleanup.duplicateGroupPages")}
              page={pages.groupPage}
              pages={groupPages}
              onPage={(groupPage) => setPages({ ...pages, groupPage })}
            />
          )}
          <div className="space-y-2">
            {visibleGroups.map((g) => (
              <DuplicateGroupCard
                key={g.hash}
                group={g}
                keepPath={resolveKeeper(g, selected)}
                chosen={selection.paths}
                amount={selection.byGroup.get(g.hash)}
                locked={locked}
                atLimit={atLimit}
                page={pages.copyPages[g.hash] ?? 0}
                onPage={(page) => setPages({ ...pages, copyPages: { ...pages.copyPages, [g.hash]: page } })}
                onKeep={(path) => store.keepDuplicate(g, path)}
                onChoose={(path) => store.toggleDuplicateVictim(g, path)}
              />
            ))}
          </div>
        </>
      )}

      {pending && <ConfirmDialog {...pending} />}
    </div>
  );
}

function DuplicateGroupCard({
  group,
  keepPath,
  chosen,
  amount,
  locked,
  atLimit,
  page,
  onPage,
  onKeep,
  onChoose,
}: {
  group: DuplicateGroup;
  keepPath: string | undefined;
  chosen: ReadonlySet<string>;
  amount?: { count: number; bytes: number };
  locked: boolean;
  atLimit: boolean;
  page: number;
  onPage: (page: number) => void;
  onKeep: (path: string) => void;
  onChoose: (path: string) => void;
}) {
  const { t } = useTranslation();
  const hashShort = group.hash.slice(0, 12);
  const copyPages = Math.ceil(group.files.length / COPIES_PER_PAGE);
  const visibleFiles = group.files.slice(page * COPIES_PER_PAGE, (page + 1) * COPIES_PER_PAGE);

  return (
    <div className="rounded-lg border border-border bg-surface-dark p-3">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-[10px] font-mono text-text-muted bg-surface px-1.5 py-0.5 rounded">
          {hashShort}…
        </span>
        <span className="text-xs text-text-muted">
          {formatBytes(group.files[0].size)} &times; {group.files.length}
        </span>
        <span className="text-xs text-text-muted ml-auto">
          {t("cleanup.duplicateSelection", { count: amount?.count ?? 0, size: formatBytes(amount?.bytes ?? 0) })}
        </span>
      </div>
      {copyPages > 1 && (
        <DuplicatePageNavigation label={t("cleanup.duplicateCopyPages")} page={page} pages={copyPages} onPage={onPage} />
      )}
      <div className="space-y-1">
        {visibleFiles.map((f) => {
          const isKept = f.path === keepPath;
          const isChosen = chosen.has(f.path);
          return (
            <div
              key={f.path}
              className={`flex items-center gap-2 rounded-md px-2 py-1.5 text-xs ${
                isKept
                  ? "bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800"
                  : "hover:bg-surface"
              }`}
            >
              <input
                type="checkbox"
                aria-label={t("cleanup.duplicateSelectFile", { path: f.path })}
                checked={isChosen}
                disabled={locked || isKept || (atLimit && !isChosen)}
                onChange={() => onChoose(f.path)}
                className="shrink-0 rounded border-border"
              />
              <button
                type="button"
                aria-label={t("cleanup.keepFile", { path: f.path })}
                aria-pressed={isKept}
                disabled={locked}
                onClick={() => onKeep(f.path)}
                className={`shrink-0 w-4 h-4 rounded-full border-2 flex items-center justify-center transition-colors ${
                  isKept
                    ? "border-green-500 bg-green-500"
                    : "border-border hover:border-primary"
                }`}
              >
                {isKept && <Check size={10} className="text-white" />}
              </button>
              <div className="min-w-0 flex-1">
                <span className="block break-all">{fileName(f.path)}</span>
                <span className="block break-all text-text-muted">{parentDir(f.path)}</span>
              </div>
              <span className="text-text-muted shrink-0">
                {formatTimestamp(f.mtime)}
              </span>
              {isKept && (
                <span className="text-[10px] text-green-600 font-medium shrink-0">
                  {t("cleanup.kept")}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function DuplicatePageNavigation({ label, page, pages, onPage }: {
  label: string;
  page: number;
  pages: number;
  onPage: (page: number) => void;
}) {
  const { t } = useTranslation();
  const buttonClass = "rounded-md px-2 py-1 hover:bg-border disabled:opacity-50";
  return (
    <form
      aria-label={label}
      className="flex flex-wrap items-center gap-2 px-1 py-1 text-xs text-text-muted"
      onSubmit={(event) => {
        event.preventDefault();
        const target = Number(new FormData(event.currentTarget).get("page"));
        if (Number.isInteger(target) && target >= 1 && target <= pages) onPage(target - 1);
      }}
    >
      <button type="button" className={buttonClass} disabled={page === 0} onClick={() => onPage(page - 1)}>
        {t("common.previousPage")}
      </button>
      <input
        key={page}
        name="page"
        type="number"
        aria-label={t("common.pageNumber")}
        min={1}
        max={pages}
        defaultValue={page + 1}
        required
        className="w-20 rounded border border-border bg-surface px-2 py-1"
      />
      <span>{t("common.pageCount", { total: pages })}</span>
      <button type="submit" className={buttonClass}>{t("common.goToPage")}</button>
      <button type="button" className={buttonClass} disabled={page + 1 >= pages} onClick={() => onPage(page + 1)}>
        {t("common.nextPage")}
      </button>
    </form>
  );
}
