import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import { Cloud, Pause, Play, X } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';

import { useLibraryStore } from '../../../store/useLibraryStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { cloudServiceName, Invokes } from '../../ui/AppProperties';

/**
 * Photos in this folder that are only in a cloud service (Nextcloud…).
 *
 * The first time such a folder is shown, a small card says what happens
 * (a small preview of each is kept) with "Sounds good" and "Change…"; until
 * it is answered nothing is downloaded. After that, a quiet line: how many
 * photos here are in the cloud, what is kept for them, and while previews
 * are being made, the progress with Pause. "Change…" opens the one choice:
 * small previews, thumbnails only, or nothing kept.
 */

export type CloudMode = 'previews' | 'thumbnails' | 'none';

interface BuilderProgress {
  done: number;
  total: number;
  paused: boolean;
}

/** Per photo, roughly: a preview, a thumbnail. */
const PREVIEW_BYTES = 1_000_000;
const THUMBNAIL_BYTES = 80_000;

const size = (bytes: number) =>
  bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;

/** The mode for a folder: its own choice, an enclosing folder's, or the default. */
export const modeFor = (folder: string | null, settings: any): CloudMode => {
  const folders: Record<string, string> = settings?.cloudFolderModes ?? {};
  const match = Object.keys(folders)
    .filter((f) => folder && (folder === f || folder.startsWith(`${f}/`)))
    .sort((a, b) => b.length - a.length)[0];
  return ((match && folders[match]) || settings?.cloudPhotosMode || 'previews') as CloudMode;
};

export function CloudPhotosChoice({
  folder,
  photoCount,
  onClose,
}: {
  folder: string | null;
  photoCount: number;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const current = modeFor(folder, appSettings);
  const [mode, setMode] = useState<CloudMode>(current);
  const [everywhere, setEverywhere] = useState(!folder);

  const options: { value: CloudMode; title: string; desc: string; bytes: number }[] = [
    {
      value: 'previews',
      title: t('cloud.previews', { defaultValue: 'Small previews' }),
      desc: t('cloud.previewsDesc', { defaultValue: 'Browse and edit without downloading.' }),
      bytes: photoCount * PREVIEW_BYTES,
    },
    {
      value: 'thumbnails',
      title: t('cloud.thumbnails', { defaultValue: 'Thumbnails only' }),
      desc: t('cloud.thumbnailsDesc', {
        defaultValue: 'Browse without downloading; opening a photo downloads it.',
      }),
      bytes: photoCount * THUMBNAIL_BYTES,
    },
    {
      value: 'none',
      title: t('cloud.none', { defaultValue: 'Download when opened' }),
      desc: t('cloud.noneDesc', {
        defaultValue: 'Nothing stored on this Mac; photos take a moment to open.',
      }),
      bytes: 0,
    },
  ];

  const save = async () => {
    const s: any = useSettingsStore.getState().appSettings ?? {};
    const folders = { ...(s.cloudFolderModes ?? {}) };
    let next: any;
    if (everywhere || !folder) {
      if (folder) delete folders[folder];
      next = { ...s, cloudPhotosMode: mode, cloudFolderModes: folders, cloudPhotosCardSeen: true };
    } else {
      if (mode === (s.cloudPhotosMode ?? 'previews')) delete folders[folder];
      else folders[folder] = mode;
      next = { ...s, cloudFolderModes: folders, cloudPhotosCardSeen: true };
    }
    await useSettingsStore.getState().handleSettingsChange(next);
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40" onClick={onClose}>
      <div
        className="w-[420px] max-w-[90vw] rounded-xl bg-surface p-5 text-sm text-text-primary shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <span className="text-base font-medium">
            {t('cloud.choiceTitle', { defaultValue: 'Photos that aren’t on this Mac' })}
          </span>
          <button onClick={onClose} className="rounded p-1 text-text-secondary hover:bg-card-active">
            <X size={16} />
          </button>
        </div>
        <div className="space-y-2">
          {options.map((o) => (
            <label
              key={o.value}
              className={`flex cursor-pointer gap-3 rounded-lg border p-3 ${
                mode === o.value ? 'border-accent bg-accent/10' : 'border-border-color hover:bg-card-active'
              }`}
            >
              <input type="radio" checked={mode === o.value} onChange={() => setMode(o.value)} className="mt-1" />
              <span>
                <span className="font-medium">
                  {o.title}
                  {o.value === 'previews' && (
                    <span className="ml-2 text-xs text-text-secondary">
                      {t('cloud.recommended', { defaultValue: 'recommended' })}
                    </span>
                  )}
                </span>
                <span className="block text-text-secondary">{o.desc}</span>
                {folder && photoCount > 0 && (
                  <span className="block text-xs text-text-secondary">
                    {o.bytes > 0
                      ? t('cloud.estimate', { defaultValue: 'About {{size}} for this folder.', size: size(o.bytes) })
                      : t('cloud.estimateNone', { defaultValue: 'Nothing for this folder.' })}
                  </span>
                )}
              </span>
            </label>
          ))}
        </div>
        {folder && (
          <label className="mt-3 flex cursor-pointer items-center gap-2 text-text-secondary">
            <input type="checkbox" checked={everywhere} onChange={(e) => setEverywhere(e.target.checked)} />
            {t('cloud.everywhere', { defaultValue: 'Use this for all cloud folders' })}
          </label>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <button onClick={onClose} className="rounded-md px-3 py-1.5 hover:bg-card-active">
            {t('common.cancel', { defaultValue: 'Cancel' })}
          </button>
          <button onClick={save} className="rounded-md bg-accent px-3 py-1.5 text-button-text">
            {t('common.done', { defaultValue: 'Done' })}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function CloudNotice() {
  const { t } = useTranslation();
  const { imageList, currentFolderPath } = useLibraryStore(
    useShallow((s) => ({ imageList: s.imageList, currentFolderPath: s.currentFolderPath })),
  );
  const appSettings: any = useSettingsStore((s) => s.appSettings);
  const [progress, setProgress] = useState<BuilderProgress>({ done: 0, total: 0, paused: false });
  const [choosing, setChoosing] = useState(false);

  useEffect(() => {
    invoke<BuilderProgress>(Invokes.GetCloudPreviewsProgress)
      .then(setProgress)
      .catch(() => {});
    const unlisten = listen<BuilderProgress>('cloud-previews-progress', (e) => setProgress(e.payload));
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const cloudPhotos = useMemo(() => imageList.filter((f) => f.cloud_only && !f.is_virtual_copy), [imageList]);
  const folder = currentFolderPath && !currentFolderPath.startsWith('Album: ') ? currentFolderPath : null;
  const building = progress.total > progress.done;
  if (cloudPhotos.length === 0 && !building) return null;

  const service = cloudServiceName(cloudPhotos[0]?.path ?? folder ?? '');
  const mode = modeFor(folder, appSettings);
  const cardSeen = !!appSettings?.cloudPhotosCardSeen;
  const link = 'inline-flex items-center gap-1.5 hover:text-text-primary transition-colors';

  const answer = () =>
    useSettingsStore.getState().handleSettingsChange({ ...(appSettings ?? {}), cloudPhotosCardSeen: true });

  return (
    <>
      {!cardSeen && cloudPhotos.length > 0 ? (
        <div className="shrink-0 mx-4 mt-3 mb-1 flex items-start gap-3 rounded-lg border border-border-color bg-surface p-3 text-sm">
          <Cloud size={18} className="mt-0.5 shrink-0 text-text-secondary" />
          <div className="flex-1">
            <div className="font-medium">
              {t('cloud.cardTitle', {
                defaultValue: 'Some of these photos are only in {{service}}.',
                service,
              })}
            </div>
            <div className="text-text-secondary">
              {t('cloud.cardBody', {
                defaultValue:
                  'Darkroom Index keeps a small preview of each (about 1 MB) so you can browse and edit them without downloading them.',
              })}
            </div>
            <div className="mt-2 flex gap-2">
              <button onClick={answer} className="rounded-md bg-accent px-3 py-1 text-button-text">
                {t('cloud.soundsGood', { defaultValue: 'Sounds good' })}
              </button>
              <button onClick={() => setChoosing(true)} className="rounded-md px-3 py-1 hover:bg-card-active">
                {t('cloud.change', { defaultValue: 'Change…' })}
              </button>
            </div>
          </div>
        </div>
      ) : (
        <div className="shrink-0 flex items-center gap-4 px-4 py-1.5 text-xs text-text-secondary border-b border-surface">
          {cloudPhotos.length > 0 && (
            <span className="inline-flex items-center gap-1.5">
              <Cloud size={13} />
              {t('cloud.line', {
                count: cloudPhotos.length,
                defaultValue: '{{count}} in {{service}}',
                service,
              })}
              {' · '}
              {mode === 'previews'
                ? t('cloud.previews', { defaultValue: 'Small previews' })
                : mode === 'thumbnails'
                  ? t('cloud.thumbnails', { defaultValue: 'Thumbnails only' })
                  : t('cloud.none', { defaultValue: 'Download when opened' })}
            </span>
          )}
          {building && (
            <span className="inline-flex items-center gap-1.5">
              {progress.paused
                ? t('cloud.paused', {
                    defaultValue: 'Previews paused, {{done}} of {{total}}',
                    done: progress.done,
                    total: progress.total,
                  })
                : t('cloud.making', {
                    defaultValue: 'Making previews {{done}} of {{total}}',
                    done: progress.done,
                    total: progress.total,
                  })}
              <button
                className={link}
                onClick={() =>
                  invoke<BuilderProgress>(Invokes.SetCloudPreviewsPaused, { paused: !progress.paused }).then(
                    setProgress,
                  )
                }
              >
                {progress.paused ? <Play size={12} /> : <Pause size={12} />}
                {progress.paused
                  ? t('cloud.resume', { defaultValue: 'Resume' })
                  : t('cloud.pause', { defaultValue: 'Pause' })}
              </button>
            </span>
          )}
          {cloudPhotos.length > 0 && (
            <button className={link} onClick={() => setChoosing(true)}>
              {t('cloud.change', { defaultValue: 'Change…' })}
            </button>
          )}
        </div>
      )}
      {choosing && (
        <CloudPhotosChoice folder={folder} photoCount={cloudPhotos.length} onClose={() => setChoosing(false)} />
      )}
    </>
  );
}
