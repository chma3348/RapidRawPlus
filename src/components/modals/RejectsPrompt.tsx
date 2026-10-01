import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { EyeOff, FolderInput } from 'lucide-react';
import clsx from 'clsx';

import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { moveRejectsToSubfolders, rejectsFolderFor } from '../../utils/flags';
import { parentOf, rememberRejectsHandling, useRejectsPrompt } from '../../utils/rejectsAfterCull';

const nameOf = (path: string) => path.split(/[\\/]/).pop() || path;

type Remember = 'once' | 'folders' | 'all';

/**
 * Asked when a cull finishes with rejects: move them into the folder's
 * rejects subfolder, or keep them hidden where they are, and whether to
 * remember that for this folder or for every folder.
 */
export default function RejectsPrompt() {
  const { t } = useTranslation();
  const { paths, close } = useRejectsPrompt();
  const [remember, setRemember] = useState<Remember>('once');

  useEffect(() => {
    if (paths) setRemember('once');
  }, [paths]);

  useEffect(() => {
    if (!paths) return;
    // Esc leaves them hidden where they are, asking again next time.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopImmediatePropagation();
      close();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [paths, close]);

  if (!paths) return null;

  const folders = [...new Set(paths.map(parentOf))];
  const one = folders.length === 1;
  const target = one ? nameOf(rejectsFolderFor(folders[0])) : t('rejectsPrompt.theirFolders');
  const folderLabel = one ? nameOf(folders[0]) : t('rejectsPrompt.theseFolders');

  const choose = (choice: 'move' | 'keep') => {
    if (remember !== 'once') rememberRejectsHandling(choice, remember, folders);
    close();
    if (choice === 'move') moveRejectsToSubfolders(paths);
  };

  const option =
    'flex-1 flex flex-col items-start gap-1.5 rounded-lg bg-bg-primary/60 hover:bg-card-active border border-border-color p-4 text-left transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-accent';
  const scopes: [Remember, string][] = [
    ['once', t('rejectsPrompt.once')],
    ['folders', one ? t('rejectsPrompt.forFolder', { name: folderLabel }) : folderLabel],
    ['all', t('rejectsPrompt.forAll')],
  ];

  return (
    <div
      className="fixed inset-0 z-[110] flex items-center justify-center bg-black/40 backdrop-blur-xs"
      role="dialog"
      aria-modal="true"
      aria-labelledby="rejects-prompt-title"
      onClick={close}
    >
      <div className="w-full max-w-lg rounded-xl bg-surface p-6 shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <Text variant={TextVariants.title} id="rejects-prompt-title">
          {t('rejectsPrompt.title', { count: paths.length })}
        </Text>
        <Text color={TextColors.secondary} className="mt-1 mb-5">
          {t('rejectsPrompt.question')}
        </Text>

        <div className="flex gap-3">
          <button className={option} onClick={() => choose('move')} autoFocus>
            <FolderInput size={20} className="text-accent" />
            <Text weight={TextWeights.semibold}>{t('rejectsPrompt.move', { target })}</Text>
            <Text variant={TextVariants.small} color={TextColors.secondary}>
              {t('rejectsPrompt.moveDesc')}
            </Text>
          </button>
          <button className={option} onClick={() => choose('keep')}>
            <EyeOff size={20} className="text-text-secondary" />
            <Text weight={TextWeights.semibold}>{t('rejectsPrompt.keep')}</Text>
            <Text variant={TextVariants.small} color={TextColors.secondary}>
              {t('rejectsPrompt.keepDesc')}
            </Text>
          </button>
        </div>

        <div className="mt-5 flex items-center gap-3">
          <Text variant={TextVariants.small} color={TextColors.secondary} className="shrink-0">
            {t('rejectsPrompt.remember')}
          </Text>
          <div className="flex rounded-md bg-bg-primary/60 p-0.5" role="radiogroup">
            {scopes.map(([value, label]) => (
              <button
                key={value}
                role="radio"
                aria-checked={remember === value}
                onClick={() => setRemember(value)}
                className={clsx(
                  'rounded px-2.5 py-1 text-xs whitespace-nowrap transition-colors',
                  remember === value
                    ? 'bg-card-active text-text-primary'
                    : 'text-text-secondary hover:text-text-primary',
                )}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
        <Text variant={TextVariants.small} color={TextColors.secondary} className="mt-3 opacity-80">
          {t('rejectsPrompt.changeLater')}
        </Text>
      </div>
    </div>
  );
}
