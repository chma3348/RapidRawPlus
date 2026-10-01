import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Check, ChevronDown, Eye, EyeOff, FolderInput, X } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';

import { useLibraryStore } from '../../../store/useLibraryStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { RejectsHandling, rejectsHandlingFor } from '../../../utils/rejectsAfterCull';
import { isRejectsFolder, movableRejects, moveRejectsToSubfolders, rejectsFolderFor } from '../../../utils/flags';

const parentOf = (path: string) => path.split('?vc=')[0].replace(/[\\/][^\\/]*$/, '');
const nameOf = (path: string) => path.split(/[\\/]/).pop() || path;

/**
 * A quiet line above the grid when the folder has rejected photos: they are
 * hidden, with Show to bring them back, and Move tidies them into a rejects
 * subfolder ("Tahiti/Tahiti rejects") with their sidecars and copies.
 */
export default function RejectsNotice() {
  const { t } = useTranslation();
  const { imageList, showRejected, currentFolderPath, flagFilter, setLibrary } = useLibraryStore(
    useShallow((s) => ({
      imageList: s.imageList,
      showRejected: s.showRejected,
      currentFolderPath: s.currentFolderPath,
      flagFilter: s.filterCriteria.flag,
      setLibrary: s.setLibrary,
    })),
  );
  const [moving, setMoving] = useState(false);

  const rejected = useMemo(() => imageList.filter((f) => f.flag === 'reject').length, [imageList]);
  const movable = useMemo(() => movableRejects(imageList), [imageList]);
  const folders = useMemo(() => new Set(movable.map(parentOf)), [movable]);

  if (rejected === 0 || flagFilter === 'rejected' || isRejectsFolder(currentFolderPath)) return null;
  const isFolder = !!currentFolderPath && !currentFolderPath.startsWith('Album: ');

  const target =
    folders.size === 1
      ? nameOf(rejectsFolderFor([...folders][0]))
      : t('library.rejects.folders', { defaultValue: 'rejects folders' });

  const move = async () => {
    setMoving(true);
    await moveRejectsToSubfolders(movable);
    setMoving(false);
  };

  const link = 'inline-flex items-center gap-1.5 hover:text-text-primary transition-colors disabled:opacity-50';
  return (
    <div className="shrink-0 flex items-center gap-4 px-4 py-1.5 text-xs text-text-secondary border-b border-surface">
      <span className="inline-flex items-center gap-1.5">
        <X size={13} />
        {showRejected
          ? t('library.rejects.showing', { count: rejected, defaultValue: 'Showing {{count}} rejected' })
          : t('library.rejects.hidden', { count: rejected, defaultValue: '{{count}} rejected hidden' })}
      </span>
      <button className={link} onClick={() => setLibrary({ showRejected: !showRejected })}>
        {showRejected ? <EyeOff size={13} /> : <Eye size={13} />}
        {showRejected
          ? t('library.rejects.hide', { defaultValue: 'Hide' })
          : t('library.rejects.show', { defaultValue: 'Show' })}
      </button>
      {movable.length > 0 && (
        <button
          className={link}
          onClick={move}
          disabled={moving}
          data-tooltip={t('library.rejects.moveTip', {
            defaultValue: 'Move them, with their sidecars and copies, into a subfolder. ⌘Z undoes it.',
          })}
        >
          <FolderInput size={13} />
          {t('library.rejects.move', {
            count: movable.length,
            target,
            defaultValue: 'Move {{count}} to “{{target}}”',
          })}
        </button>
      )}
      {isFolder && <AfterCullChoice folder={currentFolderPath!} />}
    </div>
  );
}

/** This folder's own choice for rejects when culling finishes, or the one for all folders. */
function AfterCullChoice({ folder }: { folder: string }) {
  const { t } = useTranslation();
  const { appSettings, handleSettingsChange } = useSettingsStore(
    useShallow((s) => ({ appSettings: s.appSettings, handleSettingsChange: s.handleSettingsChange })),
  );
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener('pointerdown', close);
    return () => window.removeEventListener('pointerdown', close);
  }, [open]);

  const labels: Record<RejectsHandling, string> = {
    ask: t('settings.general.rejectsAsk'),
    move: t('settings.general.rejectsMove'),
    keep: t('settings.general.rejectsKeep'),
  };
  const own = appSettings?.rejectsFolderChoices?.[folder];
  const general = appSettings?.rejectsAfterCull ?? 'ask';
  const current = rejectsHandlingFor(appSettings, folder);

  const set = (choice: RejectsHandling | null) => {
    if (!appSettings) return;
    const next = { ...(appSettings.rejectsFolderChoices || {}) };
    if (choice === null) delete next[folder];
    else next[folder] = choice;
    handleSettingsChange({ ...appSettings, rejectsFolderChoices: next });
    setOpen(false);
  };

  const item = 'w-full flex items-center gap-2 rounded px-2.5 py-1.5 text-left text-xs hover:bg-card-active';
  return (
    <div ref={ref} className="relative ml-auto">
      <button
        className="inline-flex items-center gap-1 hover:text-text-primary transition-colors"
        onClick={() => setOpen(!open)}
      >
        {t('library.rejects.afterCull')}: <span className="text-text-primary">{labels[current]}</span>
        <ChevronDown size={12} />
      </button>
      {open && (
        <div className="absolute right-0 top-6 z-30 w-64 rounded-lg border border-border-color bg-surface p-1 shadow-xl">
          <button className={item} onClick={() => set(null)}>
            <Check size={12} className={own ? 'invisible' : ''} />
            {t('library.rejects.default', { choice: labels[general] })}
          </button>
          {(['ask', 'move', 'keep'] as RejectsHandling[]).map((choice) => (
            <button key={choice} className={item} onClick={() => set(choice)}>
              <Check size={12} className={own === choice ? '' : 'invisible'} />
              {labels[choice]}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
