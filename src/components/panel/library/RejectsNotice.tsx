import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Eye, EyeOff, FolderInput, X } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';

import { useLibraryStore } from '../../../store/useLibraryStore';
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
    </div>
  );
}
