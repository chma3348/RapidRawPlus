import { ReactNode, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import { create } from 'zustand';
import { Check, CircleDashed, Folder, Info, Sparkles, Undo2, X } from 'lucide-react';

import Button from '../ui/Button';
import Switch from '../ui/Switch';
import Text from '../ui/Text';
import { ImageFile, Invokes } from '../ui/AppProperties';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { AdobeDevelop, adobeToAdjustments, hasAdobeEdits } from '../../utils/adobeImport';
import { LIBRARY_REFRESH_EVENT } from '../../utils/stacks';

/** Why the window opened by itself, when it did. */
type Reason = 'imported' | 'folderAdded' | null;

/**
 * Open the Lightroom import, optionally starting at a folder, or for just the
 * photos given (those an import brought in).
 */
export const useAdobeImport = create<{
  folder: string | null;
  paths: string[] | null;
  reason: Reason;
  open: boolean;
  show: (folder?: string | null, paths?: string[] | null, reason?: Reason) => void;
  hide: () => void;
}>((set) => ({
  folder: null,
  paths: null,
  reason: null,
  open: false,
  show: (folder, paths, reason) =>
    set({ open: true, folder: folder ?? null, paths: paths ?? null, reason: reason ?? null }),
  hide: () => set({ open: false }),
}));

const isPhoto = (path: string) => !path.includes('?vc=') && !/\.(mov|mp4|m4v)$/i.test(path);

/** The photos among `files` that have Lightroom edits not yet brought over. */
async function photosToBringOver(files: ImageFile[]) {
  const photos = files.filter((f) => isPhoto(f.path) && !f.is_edited);
  if (photos.length === 0) return 0;
  const settings: Record<string, AdobeDevelop> = await invoke(Invokes.ReadAdobeDevelop, {
    paths: photos.map((f) => f.path),
  });
  return Object.values(settings).filter(hasAdobeEdits).length;
}

/**
 * After photos come into the app, offer to bring over their Lightroom edits
 * if any have some: for an import, just the photos it brought in; for a
 * folder added to the library, the folder (its own photos are checked, and
 * the window can look through its subfolders).
 */
export async function offerAdobeImport(target: { paths: string[] } | { folder: string }) {
  try {
    if ('paths' in target) {
      const paths = target.paths.filter(isPhoto);
      if (paths.length === 0) return;
      const folders = [...new Set(paths.map((p) => p.replace(/[\\/][^\\/]*$/, '')))];
      const wanted = new Set(paths);
      const files = (await Promise.all(folders.map((f) => invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: f }))))
        .flat()
        .filter((f) => wanted.has(f.path));
      if ((await photosToBringOver(files)) > 0) useAdobeImport.getState().show(folders[0], paths, 'imported');
    } else {
      const files = await invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: target.folder });
      if ((await photosToBringOver(files)) > 0) useAdobeImport.getState().show(target.folder, null, 'folderAdded');
    }
  } catch (e) {
    console.error('Could not check for Lightroom edits:', e);
  }
}

interface Found {
  /** Photos to bring over, with their converted edits. */
  toImport: { path: string; adjustments: Record<string, any> }[];
  /** Photos already edited here: left alone. */
  alreadyEdited: number;
  /** Photos whose masks or retouching won't come across. */
  withLocal: number;
}

type Step = 'intro' | 'scanning' | 'review' | 'importing' | 'done';

const nameOf = (path: string) => path.split(/[\\/]/).pop() || path;

function Column({ icon, title, items }: { icon: ReactNode; title: string; items: string[] }) {
  return (
    <div className="flex-1 min-w-0 rounded-lg bg-bg-primary/60 p-3.5">
      <div className="flex items-center gap-2 mb-2">
        {icon}
        <Text weight={TextWeights.semibold}>{title}</Text>
      </div>
      <ul className="space-y-1">
        {items.map((item) => (
          <li key={item}>
            <Text variant={TextVariants.small} color={TextColors.secondary}>
              {item}
            </Text>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * Bring over Lightroom edits: a one-time move from Adobe. It explains what
 * converts, finds photos with Lightroom or Camera Raw edits in a folder,
 * and sets up the same edits here as a starting point. Photos already edited
 * here are left alone, Adobe's files are never changed, and Undo takes the
 * imported edits off again.
 */
export default function AdobeImportModal() {
  const { t } = useTranslation();
  const { open, folder: startFolder, paths: givenPaths, reason, hide } = useAdobeImport();
  const [folder, setFolder] = useState<string | null>(null);
  const [subfolders, setSubfolders] = useState(true);
  const [step, setStep] = useState<Step>('intro');
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  const [found, setFound] = useState<Found | null>(null);
  const [imported, setImported] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [wasOpen, setWasOpen] = useState(false);

  // Start fresh each time it opens.
  if (open && !wasOpen) {
    setWasOpen(true);
    setFolder(startFolder);
    setStep('intro');
    setFound(null);
    setImported([]);
    setError(null);
  } else if (!open && wasOpen) {
    setWasOpen(false);
  }
  if (!open) return null;

  const busy = step === 'scanning' || step === 'importing';

  const chooseFolder = async () => {
    const picked = await openDialog({ directory: true, multiple: false, defaultPath: folder ?? undefined });
    if (typeof picked === 'string') setFolder(picked);
  };

  const scan = async () => {
    if (!folder && !givenPaths) return;
    setStep('scanning');
    setError(null);
    try {
      let files: ImageFile[];
      if (givenPaths) {
        // Just the photos given: listed from their folders, for whether each is edited here.
        const folders = [...new Set(givenPaths.map((p) => p.replace(/[\\/][^\\/]*$/, '')))];
        const wanted = new Set(givenPaths);
        files = (await Promise.all(folders.map((f) => invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: f }))))
          .flat()
          .filter((f) => wanted.has(f.path));
      } else {
        files = await invoke(subfolders ? Invokes.ListImagesRecursive : Invokes.ListImagesInDir, { path: folder });
      }
      const photos = files.filter((f) => isPhoto(f.path));
      const settings: Record<string, AdobeDevelop> = {};
      setProgress({ done: 0, total: photos.length });
      for (let i = 0; i < photos.length; i += 100) {
        const batch = photos.slice(i, i + 100).map((f) => f.path);
        Object.assign(settings, await invoke(Invokes.ReadAdobeDevelop, { paths: batch }));
        setProgress({ done: Math.min(i + 100, photos.length), total: photos.length });
      }
      const result: Found = { toImport: [], alreadyEdited: 0, withLocal: 0 };
      for (const photo of photos) {
        const d = settings[photo.path];
        if (!d || !hasAdobeEdits(d)) continue;
        if (photo.is_edited) {
          result.alreadyEdited++;
          continue;
        }
        if (d.localCorrections + d.retouchSpots > 0) result.withLocal++;
        result.toImport.push({ path: photo.path, adjustments: adobeToAdjustments(d) });
      }
      setFound(result);
      setStep('review');
    } catch (e) {
      setError(String(e));
      setStep('intro');
    }
  };

  const bringOver = async () => {
    if (!found) return;
    setStep('importing');
    setProgress({ done: 0, total: found.toImport.length });
    const done: string[] = [];
    // A few at a time: each save also redraws the photo's thumbnail.
    const queue = [...found.toImport];
    const worker = async () => {
      for (let item = queue.shift(); item; item = queue.shift()) {
        try {
          await invoke(Invokes.SaveMetadataAndUpdateThumbnail, { path: item.path, adjustments: item.adjustments });
          done.push(item.path);
        } catch (e) {
          console.error('Could not bring over edits for', item.path, e);
        }
        setProgress((p) => ({ ...p, done: p.done + 1 }));
      }
    };
    await Promise.all([worker(), worker(), worker(), worker()]);
    setImported(done);
    setStep('done');
    window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
  };

  const undo = async () => {
    await invoke(Invokes.ResetAdjustmentsForPaths, { paths: imported }).catch((e) => setError(String(e)));
    window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
    hide();
  };

  return (
    <div
      className="fixed inset-0 z-[110] flex items-center justify-center bg-black/40 backdrop-blur-xs p-6"
      role="dialog"
      aria-modal="true"
      aria-labelledby="adobe-import-title"
      onClick={() => !busy && hide()}
    >
      <div
        className="relative w-full max-w-3xl max-h-full overflow-y-auto rounded-xl bg-surface p-7 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {!busy && (
          <button
            className="absolute right-4 top-4 p-1.5 rounded-md text-text-secondary hover:bg-card-active"
            onClick={hide}
            aria-label={t('adobeImport.close')}
          >
            <X size={18} />
          </button>
        )}

        <div className="flex items-center gap-2.5 mb-1">
          <Sparkles size={20} className="text-accent" />
          <Text variant={TextVariants.title} id="adobe-import-title">
            {t('adobeImport.title')}
          </Text>
        </div>

        {(step === 'intro' || step === 'scanning') && (
          <>
            {reason && (
              <div className="rounded-lg bg-accent/15 border border-accent/30 px-3.5 py-2.5 mt-3 mb-3">
                <Text weight={TextWeights.semibold}>
                  {reason === 'imported'
                    ? t('adobeImport.foundOnImport')
                    : t('adobeImport.foundInFolder', { name: folder ? nameOf(folder) : '' })}
                </Text>
              </div>
            )}
            <Text color={TextColors.secondary} className="mb-5">
              {t('adobeImport.lead')}
            </Text>
            <div className="flex gap-3 mb-4">
              <Column
                icon={<Check size={16} className="text-accent" />}
                title={t('adobeImport.exactTitle')}
                items={t('adobeImport.exact', { returnObjects: true }) as string[]}
              />
              <Column
                icon={<CircleDashed size={16} className="text-accent" />}
                title={t('adobeImport.closeTitle')}
                items={t('adobeImport.close_items', { returnObjects: true }) as string[]}
              />
              <Column
                icon={<X size={16} className="text-text-secondary" />}
                title={t('adobeImport.notTitle')}
                items={t('adobeImport.not', { returnObjects: true }) as string[]}
              />
            </div>
            <div className="flex gap-2 items-start rounded-lg border border-border-color p-3 mb-5">
              <Info size={15} className="mt-0.5 shrink-0 text-text-secondary" />
              <Text variant={TextVariants.small} color={TextColors.secondary}>
                {t('adobeImport.note')}
              </Text>
            </div>

            <div className="flex items-center gap-3 flex-wrap">
              {givenPaths ? (
                <Text color={TextColors.secondary}>{t('adobeImport.justImported', { count: givenPaths.length })}</Text>
              ) : (
                <>
                  <Button className="h-10 px-4 bg-bg-primary text-text-primary" onClick={chooseFolder} disabled={busy}>
                    <Folder size={15} className="mr-2" />
                    {folder ? nameOf(folder) : t('adobeImport.chooseFolder')}
                  </Button>
                  <Switch
                    id="adobe-import-subfolders"
                    checked={subfolders}
                    onChange={setSubfolders}
                    label={t('adobeImport.subfolders')}
                  />
                </>
              )}
              {reason && !busy && (
                <Button className="h-10 px-4 ml-auto bg-bg-primary text-text-primary" onClick={hide}>
                  {t('adobeImport.notNow')}
                </Button>
              )}
              <Button
                className={`h-10 px-5 ${reason && !busy ? '' : 'ml-auto'}`}
                onClick={scan}
                disabled={(!folder && !givenPaths) || busy}
              >
                {step === 'scanning'
                  ? t('adobeImport.scanning', { done: progress.done, total: progress.total })
                  : t('adobeImport.find')}
              </Button>
            </div>
            {error && (
              <Text variant={TextVariants.small} className="mt-3 text-red-400">
                {error}
              </Text>
            )}
          </>
        )}

        {step === 'review' && found && (
          <>
            {found.toImport.length === 0 ? (
              <Text color={TextColors.secondary} className="my-5">
                {found.alreadyEdited > 0
                  ? t('adobeImport.noneNew', { count: found.alreadyEdited })
                  : t('adobeImport.noneFound')}
              </Text>
            ) : (
              <div className="my-5 space-y-2">
                <Text variant={TextVariants.headline}>{t('adobeImport.found', { count: found.toImport.length })}</Text>
                {found.alreadyEdited > 0 && (
                  <Text color={TextColors.secondary}>
                    {t('adobeImport.alreadyEdited', { count: found.alreadyEdited })}
                  </Text>
                )}
                {found.withLocal > 0 && (
                  <Text color={TextColors.secondary}>{t('adobeImport.withLocal', { count: found.withLocal })}</Text>
                )}
              </div>
            )}
            <div className="flex justify-end gap-3">
              <Button className="h-10 px-4 bg-bg-primary text-text-primary" onClick={() => setStep('intro')}>
                {t('adobeImport.back')}
              </Button>
              {found.toImport.length > 0 && (
                <Button className="h-10 px-5" onClick={bringOver}>
                  {t('adobeImport.bringOver', { count: found.toImport.length })}
                </Button>
              )}
            </div>
          </>
        )}

        {step === 'importing' && (
          <div className="my-6">
            <Text color={TextColors.secondary} className="mb-2">
              {t('adobeImport.importing', { done: progress.done, total: progress.total })}
            </Text>
            <div className="h-2 rounded-full bg-bg-primary overflow-hidden">
              <div
                className="h-full bg-accent transition-[width] duration-300"
                style={{ width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%` }}
              />
            </div>
          </div>
        )}

        {step === 'done' && (
          <>
            <Text variant={TextVariants.headline} className="mt-5">
              {t('adobeImport.done', { count: imported.length })}
            </Text>
            <Text color={TextColors.secondary} className="mt-1 mb-6">
              {t('adobeImport.doneNote')}
            </Text>
            <div className="flex justify-end gap-3">
              <Button className="h-10 px-4 bg-bg-primary text-text-primary" onClick={undo}>
                <Undo2 size={15} className="mr-2" />
                {t('adobeImport.undo')}
              </Button>
              <Button className="h-10 px-5" onClick={hide}>
                {t('adobeImport.finish')}
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
