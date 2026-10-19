import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import { Cloud, Pause, Play, X } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';

import { useLibraryStore } from '../../../store/useLibraryStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { AppSettings, cloudServiceName, ImageFile, Invokes } from '../../ui/AppProperties';

/**
 * Photos that are only in a cloud service (Nextcloud…), not on this Mac.
 *
 * The first time a folder (or Home) shows such photos, and nothing has been
 * chosen yet, a window explains what is happening — they cannot be shown
 * until something is downloaded — and offers the one choice: small
 * previews, thumbnails only, or nothing kept. Nothing downloads until it is
 * answered. After that a quiet line above the photos says how this
 * folder's are coming along, with the time left and Pause; "Change…"
 * reopens the choice.
 */

export type CloudMode = 'previews' | 'thumbnails' | 'none';

interface BuilderProgress {
  done: number;
  total: number;
  paused: boolean;
  /** Seconds per photo lately, all workers together; null at first. */
  secondsPerPhoto?: number | null;
}

interface FolderSummary {
  photos: number;
  bytes: number;
  withPreviews: number;
}

/** The cloud fields of the settings (not in the AppSettings type). */
interface CloudSettings {
  cloudPhotosMode?: string | null;
  cloudFolderModes?: Record<string, string> | null;
  cloudPhotosCardSeen?: boolean | null;
}

const cloudSettings = (settings: AppSettings | null): CloudSettings => (settings ?? {}) as unknown as CloudSettings;

/** Per photo, roughly: a preview, a thumbnail. */
const PREVIEW_BYTES = 1_000_000;
const THUMBNAIL_BYTES = 80_000;

const size = (bytes: number) =>
  bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;

/** The mode for a folder: its own choice, an enclosing folder's, or the default. */
export const modeFor = (folder: string | null, settings: AppSettings | null): CloudMode => {
  const cloud = cloudSettings(settings);
  const folders: Record<string, string> = cloud.cloudFolderModes ?? {};
  const match = Object.keys(folders)
    .filter((f) => folder && (folder === f || folder.startsWith(`${f}/`)))
    .sort((a, b) => b.length - a.length)[0];
  return ((match && folders[match]) || cloud.cloudPhotosMode || 'previews') as CloudMode;
};

/** Previews made this session (the listing's has_preview is read once). */
const madeThisSession = new Set<string>();
/** "Not now" in the first-time window: not asked again until the app restarts. */
let postponed = false;

const useMade = () => {
  const [, setCount] = useState(0);
  useEffect(() => {
    const unlisten = listen<string>('cloud-preview-made', (e) => {
      madeThisSession.add(e.payload);
      setCount((c) => c + 1);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);
  return madeThisSession;
};

/**
 * The choice of what to keep for cloud photos. `firstTime` is the window
 * shown before anything has been chosen: it says why nothing shows, and
 * offers "Not now".
 */
export function CloudPhotosChoice({
  folder,
  photoCount,
  summary,
  firstTime = false,
  service: named,
  onClose,
}: {
  folder: string | null;
  photoCount: number;
  summary?: FolderSummary | null;
  firstTime?: boolean;
  /** The cloud service's name, where the caller knows it better than the folder does. */
  service?: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const current = modeFor(folder, appSettings);
  const [mode, setMode] = useState<CloudMode>(current);
  const [everywhere, setEverywhere] = useState(firstTime || !folder);
  const service = named ?? cloudServiceName(folder ?? '');
  const count = summary?.photos ?? photoCount;
  const toMake = summary ? summary.photos - summary.withPreviews : photoCount;

  const options: { value: CloudMode; title: string; desc: string; cost: string | null }[] = [
    {
      value: 'previews',
      title: t('cloud.previews', { defaultValue: 'Make small previews' }),
      desc: t('cloud.previewsDescLong', {
        defaultValue:
          'Downloads each photo once, makes a small preview of it (about 1 MB), then removes the download. The preview is saved next to the photo in {{service}}, so you can browse and edit here without keeping the full photos on this Mac.',
        service,
      }),
      cost:
        folder && toMake > 0
          ? t('cloud.previewsCost', {
              defaultValue: 'This folder: downloads {{download}} once, keeps about {{kept}}.',
              download: summary ? size(summary.bytes) : t('cloud.eachPhoto', { defaultValue: 'each photo' }),
              kept: size(toMake * PREVIEW_BYTES),
            })
          : null,
    },
    {
      value: 'thumbnails',
      title: t('cloud.thumbnails', { defaultValue: 'Thumbnails only' }),
      desc: t('cloud.thumbnailsDescLong', {
        defaultValue:
          'Downloads each photo once to make a thumbnail for browsing, then removes it. Opening a photo downloads it again.',
      }),
      cost:
        folder && count > 0
          ? t('cloud.thumbnailsCost', {
              defaultValue: 'This folder: downloads {{download}} once, keeps about {{kept}}.',
              download: summary ? size(summary.bytes) : t('cloud.eachPhoto', { defaultValue: 'each photo' }),
              kept: size(count * THUMBNAIL_BYTES),
            })
          : null,
    },
    {
      value: 'none',
      title: t('cloud.none', { defaultValue: "Don't download anything" }),
      desc: t('cloud.noneDescLong', {
        defaultValue: 'Photos show as cloud icons. Opening one downloads it, which takes a moment.',
      }),
      cost: null,
    },
  ];

  const save = async () => {
    const settings = useSettingsStore.getState().appSettings;
    const cloud = cloudSettings(settings);
    const folders = { ...(cloud.cloudFolderModes ?? {}) };
    let changes: CloudSettings;
    if (everywhere || !folder) {
      if (folder) delete folders[folder];
      changes = { cloudPhotosMode: mode, cloudFolderModes: folders, cloudPhotosCardSeen: true };
    } else {
      if (mode === (cloud.cloudPhotosMode ?? 'previews')) delete folders[folder];
      else folders[folder] = mode;
      changes = { cloudFolderModes: folders, cloudPhotosCardSeen: true };
    }
    await useSettingsStore.getState().handleSettingsChange({ ...settings, ...changes } as AppSettings);
    onClose();
  };

  const later = () => {
    postponed = true;
    onClose();
  };

  const start =
    mode === 'previews'
      ? t('cloud.startPreviews', { defaultValue: 'Make previews' })
      : mode === 'thumbnails'
        ? t('cloud.startThumbnails', { defaultValue: 'Make thumbnails' })
        : t('common.done', { defaultValue: 'Done' });

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={firstTime ? undefined : onClose}
    >
      <div
        className="w-[520px] max-w-[92vw] rounded-xl bg-surface p-6 text-sm text-text-primary shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-2 flex items-start justify-between gap-4">
          <div className="flex items-center gap-2.5">
            <Cloud size={20} className="shrink-0 text-accent" />
            <span className="text-base font-medium">
              {firstTime
                ? count > 0
                  ? t('cloud.firstTitle', {
                      defaultValue: '{{count}} photos here are only in {{service}}',
                      count,
                      service,
                    })
                  : t('cloud.firstTitleSome', { defaultValue: 'Some photos here are only in {{service}}', service })
                : t('cloud.choiceTitle', { defaultValue: 'Photos stored in {{service}}', service })}
            </span>
          </div>
          {!firstTime && (
            <button onClick={onClose} className="rounded p-1 text-text-secondary hover:bg-card-active">
              <X size={16} />
            </button>
          )}
        </div>
        <p className="mb-4 text-text-secondary">
          {firstTime
            ? t('cloud.firstBody', {
                defaultValue:
                  "They haven't been downloaded to this Mac, so there is nothing to show yet. How should Darkroom Index handle photos stored in {{service}}?",
                service,
              })
            : t('cloud.choiceBody', {
                defaultValue: 'What Darkroom Index keeps on this Mac for photos that are only in {{service}}.',
                service,
              })}
        </p>
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
                <span className="mt-0.5 block text-text-secondary">{o.desc}</span>
                {o.cost && <span className="mt-1 block text-xs text-text-secondary">{o.cost}</span>}
              </span>
            </label>
          ))}
        </div>
        {folder && (
          <label className="mt-3 flex cursor-pointer items-center gap-2 text-text-secondary">
            <input type="checkbox" checked={everywhere} onChange={(e) => setEverywhere(e.target.checked)} />
            {t('cloud.everywhereService', { defaultValue: 'Use this for all my {{service}} folders', service })}
          </label>
        )}
        <p className="mt-3 text-xs text-text-secondary">
          {t('cloud.backgroundNote', {
            defaultValue:
              'This runs in the background, a few photos at a time, starting with the ones on screen. You can pause it at any time, and change this later with Change… above the photos or in Settings.',
          })}
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <button onClick={firstTime ? later : onClose} className="rounded-md px-3 py-1.5 hover:bg-card-active">
            {firstTime
              ? t('cloud.notNow', { defaultValue: 'Not now' })
              : t('common.cancel', { defaultValue: 'Cancel' })}
          </button>
          <button onClick={save} className="rounded-md bg-accent px-4 py-1.5 font-medium text-button-text">
            {start}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * In the library, for the folder open there. Home passes the photos of every
 * folder it shows (`photos`) and no folder: Change… there sets the choice for
 * all cloud folders.
 */
export default function CloudNotice({ photos, home = false }: { photos?: ImageFile[]; home?: boolean } = {}) {
  const { t } = useTranslation();
  const { libraryList, currentFolderPath } = useLibraryStore(
    useShallow((s) => ({ libraryList: s.imageList, currentFolderPath: s.currentFolderPath })),
  );
  const imageList = photos ?? libraryList;
  const appSettings = useSettingsStore((s) => s.appSettings);
  const [progress, setProgress] = useState<BuilderProgress>({ done: 0, total: 0, paused: false });
  const [choosing, setChoosing] = useState(false);
  const [summary, setSummary] = useState<FolderSummary | null>(null);
  const [, setPostponedTick] = useState(0);
  const made = useMade();

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
  const folder = !home && currentFolderPath && !currentFolderPath.startsWith('Album: ') ? currentFolderPath : null;

  // What this folder's cloud photos come to, for the choice.
  useEffect(() => {
    setSummary(null);
    if (!folder || cloudPhotos.length === 0) return;
    invoke<FolderSummary>(Invokes.CloudFolderSummary, { path: folder })
      .then(setSummary)
      .catch(() => {});
  }, [folder, cloudPhotos.length]);

  const building = progress.total > progress.done;
  if (cloudPhotos.length === 0 && !building) return null;

  const service = cloudServiceName(cloudPhotos[0]?.path ?? folder ?? '');
  const mode = modeFor(folder, appSettings);
  const chosen = !!cloudSettings(appSettings).cloudPhotosCardSeen;
  const link = 'inline-flex items-center gap-1.5 hover:text-text-primary transition-colors';

  // This folder's previews: made before (listed) or since (events).
  const waiting = cloudPhotos.filter((f) => !f.has_preview && !made.has(f.path)).length;
  const ready = cloudPhotos.length - waiting;
  const secondsLeft =
    progress.secondsPerPhoto && !progress.paused
      ? (folder ? waiting : progress.total - progress.done) * progress.secondsPerPhoto
      : null;
  const minutesLeft = secondsLeft === null ? null : Math.round(secondsLeft / 60);
  // "about 1 h 20 min left", "about 5 min left", "under a minute left".
  const timeLeft =
    minutesLeft === null
      ? null
      : minutesLeft < 1
        ? t('cloud.underAMinute', { defaultValue: 'under a minute left' })
        : minutesLeft < 60
          ? t('cloud.minutesLeft', { defaultValue: 'about {{minutes}} min left', minutes: minutesLeft })
          : minutesLeft % 60
            ? t('cloud.hoursMinutesLeft', {
                defaultValue: 'about {{hours}} h {{minutes}} min left',
                hours: Math.floor(minutesLeft / 60),
                minutes: minutesLeft % 60,
              })
            : t('cloud.hoursLeft', { defaultValue: 'about {{hours}} h left', hours: minutesLeft / 60 });

  const pauseButton = building && (
    <button
      className={link}
      onClick={() =>
        invoke<BuilderProgress>(Invokes.SetCloudPreviewsPaused, { paused: !progress.paused }).then(setProgress)
      }
    >
      {progress.paused ? <Play size={12} /> : <Pause size={12} />}
      {progress.paused ? t('cloud.resume', { defaultValue: 'Resume' }) : t('cloud.pause', { defaultValue: 'Pause' })}
    </button>
  );

  // What is happening, in one line.
  let status: string;
  if (!chosen) {
    status = t('cloud.notSetUp', { defaultValue: 'Not downloading anything yet' });
  } else if (mode === 'none') {
    status = t('cloud.noneStatus', { defaultValue: 'Downloaded when opened' });
  } else if (mode === 'thumbnails') {
    status = building
      ? t('cloud.makingThumbnails', {
          defaultValue: 'Making thumbnails, {{left}} to go',
          left: progress.total - progress.done,
        })
      : t('cloud.thumbnailsStatus', { defaultValue: 'Thumbnails only' });
  } else if (folder && waiting > 0) {
    status = progress.paused
      ? t('cloud.pausedFolder', {
          defaultValue: 'Previews paused, {{ready}} of {{total}} ready',
          ready,
          total: cloudPhotos.length,
        })
      : t('cloud.makingFolder', {
          defaultValue: 'Making previews for this folder: {{ready}} of {{total}}',
          ready,
          total: cloudPhotos.length,
        });
  } else if (folder) {
    status = building
      ? t('cloud.readyHereMore', {
          defaultValue: 'Previews ready · making {{left}} more for other folders',
          left: progress.total - progress.done,
        })
      : t('cloud.readyHere', { defaultValue: 'Previews ready' });
  } else {
    status = building
      ? t('cloud.makingHome', { defaultValue: 'Making previews, {{left}} to go', left: progress.total - progress.done })
      : t('cloud.previewsStatus', { defaultValue: 'Small previews' });
  }

  const askNow = !chosen && !postponed && cloudPhotos.length > 0;

  return (
    <>
      <div
        className={`shrink-0 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-text-secondary ${
          home ? 'px-8 pb-4' : 'px-4 py-1.5 border-b border-surface'
        }`}
      >
        {cloudPhotos.length > 0 && (
          <span className="inline-flex items-center gap-1.5">
            <Cloud size={13} />
            {t('cloud.line', {
              count: cloudPhotos.length,
              defaultValue: '{{count}} in {{service}}',
              service,
            })}
          </span>
        )}
        <span>
          {status}
          {timeLeft && (mode === 'previews' || mode === 'thumbnails') && (folder ? waiting > 0 : building)
            ? ` · ${timeLeft}`
            : ''}
        </span>
        {pauseButton}
        {cloudPhotos.length > 0 && (
          <button className={link} onClick={() => setChoosing(true)}>
            {chosen ? t('cloud.change', { defaultValue: 'Change…' }) : t('cloud.setUp', { defaultValue: 'Set up…' })}
          </button>
        )}
      </div>
      {(choosing || askNow) && (
        <CloudPhotosChoice
          folder={folder}
          photoCount={cloudPhotos.length}
          summary={summary}
          firstTime={!chosen}
          service={service}
          onClose={() => {
            setChoosing(false);
            setPostponedTick((n) => n + 1);
          }}
        />
      )}
    </>
  );
}
