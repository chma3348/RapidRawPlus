import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { Columns2, Flag, FlagOff, Image as ImageIcon, Star, X, ArrowRightLeft } from 'lucide-react';
import { toast } from 'react-toastify';
import { ImageFile, Invokes } from '../../ui/AppProperties';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { useProcessStore } from '../../../store/useProcessStore';
import { isVideoPath } from '../../../utils/media';
import { COLOR_LABELS } from '../../../utils/adjustments';
import { setFlagForPaths } from '../../../utils/flags';

/**
 * Culling: going through a shoot quickly, one big photo at a time,
 * deciding with single keys and moving on — the way Lightroom's loupe and
 * darktable's culling layout work.
 *
 *   P pick · X reject · U clear flag · 1–5 stars (0 clears) ·
 *   6 red, 7 yellow, 8 green, 9 blue label · ←/→ previous/next ·
 *   C compare side by side · A auto-advance · Enter edit · Esc done
 *
 * With auto-advance on (the default), every decision moves to the next
 * photo. Compare shows two photos side by side; decisions apply to the
 * one outlined, and Tab or a click switches between them.
 */
interface CullState {
  open: boolean;
  /** The photos to go through, in library order. */
  paths: string[];
  startPath: string | null;
  openCull: (paths: string[], startPath: string | null) => void;
  closeCull: () => void;
}

export const useCullStore = create<CullState>((set) => ({
  open: false,
  paths: [],
  startPath: null,
  openCull: (paths, startPath) => set({ open: paths.length > 0, paths, startPath }),
  closeCull: () => set({ open: false }),
}));

/** Open culling on the selection (when several are selected) or the whole view. */
export function startCulling(sortedList: ImageFile[]) {
  const { multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
  const selected = new Set(multiSelectedPaths);
  const paths =
    multiSelectedPaths.length > 1
      ? sortedList.filter((f) => selected.has(f.path)).map((f) => f.path)
      : sortedList.map((f) => f.path);
  useCullStore.getState().openCull(paths, libraryActivePath ?? paths[0] ?? null);
}

const LABEL_KEYS: Record<string, string> = { '6': 'red', '7': 'yellow', '8': 'green', '9': 'blue' };

/** Sharp previews of the photos around the current one, with edits applied. */
function usePreviews(paths: string[], index: number) {
  const [urls, setUrls] = useState<Record<string, string>>({});
  const urlsRef = useRef<Record<string, string>>({});
  const pending = useRef(new Set<string>());
  const queue = useRef<string[]>([]);
  const running = useRef(false);

  const pump = useCallback(async () => {
    if (running.current) return;
    running.current = true;
    while (queue.current.length > 0) {
      const path = queue.current.shift()!;
      if (urlsRef.current[path] || isVideoPath(path)) continue;
      try {
        const metadata: any = await invoke(Invokes.LoadMetadata, { path });
        const adjustments =
          metadata?.adjustments && typeof metadata.adjustments === 'object' ? metadata.adjustments : {};
        const bytes: Uint8Array = await invoke(Invokes.GeneratePreviewForPath, {
          path: path.split('?vc=')[0],
          jsAdjustments: adjustments,
        });
        const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'image/png' }));
        urlsRef.current = { ...urlsRef.current, [path]: url };
        setUrls(urlsRef.current);
      } catch {
        // The library thumbnail stays on screen.
      } finally {
        pending.current.delete(path);
      }
    }
    running.current = false;
  }, []);

  useEffect(() => {
    // The current photo first, then the next two and the previous one.
    const wanted = [index, index + 1, index + 2, index - 1]
      .filter((i) => i >= 0 && i < paths.length)
      .map((i) => paths[i])
      .filter((p) => !urlsRef.current[p] && !pending.current.has(p));
    wanted.forEach((p) => pending.current.add(p));
    queue.current = [...wanted, ...queue.current.filter((p) => !wanted.includes(p))];
    void pump();

    // Keep a small window of previews; let the rest go.
    const keep = new Set(paths.slice(Math.max(0, index - 3), index + 6));
    const next: Record<string, string> = {};
    for (const [p, url] of Object.entries(urlsRef.current)) {
      if (keep.has(p)) next[p] = url;
      else URL.revokeObjectURL(url);
    }
    if (Object.keys(next).length !== Object.keys(urlsRef.current).length) {
      urlsRef.current = next;
      setUrls(next);
    }
  }, [paths, index, pump]);

  useEffect(
    () => () => {
      Object.values(urlsRef.current).forEach((u) => URL.revokeObjectURL(u));
    },
    [],
  );

  return urls;
}

export default function CullView({ onEdit }: { onEdit: (path: string) => void }) {
  const { t } = useTranslation();
  const { open, paths, startPath, closeCull } = useCullStore();
  const imageList = useLibraryStore((s) => s.imageList);
  const imageRatings = useLibraryStore((s) => s.imageRatings);
  const thumbnails = useProcessStore((s) => s.thumbnails);
  const [index, setIndex] = useState(0);
  const [autoAdvance, setAutoAdvance] = useState(true);
  const [compare, setCompare] = useState(false);
  const [focusRight, setFocusRight] = useState(false);

  useEffect(() => {
    if (!open) return;
    const start = startPath ? paths.indexOf(startPath) : 0;
    setIndex(Math.max(0, start));
    setCompare(false);
    setFocusRight(false);
  }, [open, paths, startPath]);

  const byPath = useMemo(() => new Map(imageList.map((f) => [f.path, f])), [imageList]);
  const previews = usePreviews(open ? paths : [], index);
  const target = Math.min(paths.length - 1, index + (compare && focusRight ? 1 : 0));
  const targetPath = paths[target];

  const go = useCallback((to: number) => setIndex(Math.max(0, Math.min(paths.length - 1, to))), [paths.length]);
  const advance = useCallback(() => {
    if (autoAdvance) go(index + 1);
  }, [autoAdvance, go, index]);

  const setFlag = useCallback(
    (flag: 'pick' | 'reject' | null) => {
      if (!targetPath) return;
      setFlagForPaths([targetPath], flag);
      advance();
    },
    [targetPath, advance],
  );

  const setRating = useCallback(
    (rating: number) => {
      if (!targetPath) return;
      useLibraryStore.getState().setLibrary((s) => ({ imageRatings: { ...s.imageRatings, [targetPath]: rating } }));
      invoke(Invokes.SetRatingForPaths, { paths: [targetPath], rating }).catch((e) =>
        toast.error(`Could not rate: ${e}`),
      );
      advance();
    },
    [targetPath, advance],
  );

  const setLabel = useCallback(
    (color: string) => {
      if (!targetPath) return;
      const current = byPath
        .get(targetPath)
        ?.tags?.find((tag) => tag.startsWith('color:'))
        ?.slice(6);
      const finalColor = current === color ? null : color;
      useLibraryStore.getState().setLibrary((s) => ({
        imageList: s.imageList.map((f) => {
          if (f.path !== targetPath) return f;
          const others = (f.tags || []).filter((tag) => !tag.startsWith('color:'));
          return { ...f, tags: finalColor ? [...others, `color:${finalColor}`] : others };
        }),
      }));
      invoke(Invokes.SetColorLabelForPaths, { paths: [targetPath], color: finalColor }).catch((e) =>
        toast.error(`Could not label: ${e}`),
      );
      advance();
    },
    [targetPath, byPath, advance],
  );

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const key = e.key.toLowerCase();
      let handled = true;
      if (key === 'escape') closeCull();
      else if (key === 'arrowright') go(index + 1);
      else if (key === 'arrowleft') go(index - 1);
      else if (key === 'p') setFlag('pick');
      else if (key === 'x') setFlag('reject');
      else if (key === 'u') setFlag(null);
      else if (/^[0-5]$/.test(key)) setRating(Number(key));
      else if (LABEL_KEYS[key]) setLabel(LABEL_KEYS[key]);
      else if (key === 'c') setCompare((v) => !v);
      else if (key === 'a') setAutoAdvance((v) => !v);
      else if (key === 'tab' && compare) setFocusRight((v) => !v);
      else if (key === 'enter' && targetPath) {
        closeCull();
        onEdit(targetPath);
      } else handled = false;
      if (handled) {
        e.preventDefault();
        e.stopImmediatePropagation();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [open, index, go, setFlag, setRating, setLabel, closeCull, compare, targetPath, onEdit]);

  if (!open || paths.length === 0) return null;

  const photo = (i: number, focused: boolean, onFocus?: () => void) => {
    const path = paths[i];
    if (!path) return <div className="flex-1" />;
    const file = byPath.get(path);
    const rejected = file?.flag === 'reject';
    const src = previews[path] || thumbnails[path];
    return (
      <div
        className={clsx(
          'relative flex min-h-0 min-w-0 flex-1 items-center justify-center rounded-lg p-1',
          compare && focused && 'ring-2 ring-accent',
        )}
        onClick={onFocus}
      >
        {src ? (
          <img
            src={src}
            alt=""
            draggable={false}
            className={clsx('max-h-full max-w-full object-contain transition-opacity', rejected && 'opacity-35')}
          />
        ) : (
          <ImageIcon size={48} className="text-text-secondary" />
        )}
        <PhotoBadges file={file} rating={imageRatings[path] || 0} />
      </div>
    );
  };

  const name = (paths[target] || '').split(/[\\/]/).pop()?.replace('?vc=', ' · copy ');
  const strip = paths.slice(Math.max(0, index - 8), index + 9);

  return (
    <div className="fixed inset-0 z-[60] flex flex-col bg-bg-primary text-text-primary">
      <div className="flex h-12 shrink-0 items-center gap-4 border-b border-surface px-4 text-sm">
        <span className="font-semibold">{t('cull.title', { defaultValue: 'Cull' })}</span>
        <span className="truncate text-text-secondary">{name}</span>
        <span className="text-text-secondary tabular-nums">
          {index + 1} / {paths.length}
        </span>
        <div className="flex-1" />
        <button
          type="button"
          onClick={() => setAutoAdvance((v) => !v)}
          aria-pressed={autoAdvance}
          className={clsx(
            'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs',
            autoAdvance ? 'bg-card-active' : 'text-text-secondary hover:bg-surface',
          )}
          data-tooltip={t('cull.autoAdvanceTip', { defaultValue: 'Move to the next photo after each decision (A)' })}
        >
          <ArrowRightLeft size={13} />
          {t('cull.autoAdvance', { defaultValue: 'Auto-advance' })}
        </button>
        <button
          type="button"
          onClick={() => setCompare((v) => !v)}
          aria-pressed={compare}
          className={clsx(
            'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs',
            compare ? 'bg-card-active' : 'text-text-secondary hover:bg-surface',
          )}
          data-tooltip={t('cull.compareTip', { defaultValue: 'Compare this photo with the next (C)' })}
        >
          <Columns2 size={13} />
          {t('cull.compare', { defaultValue: 'Compare' })}
        </button>
        <button
          type="button"
          onClick={closeCull}
          className="rounded-md bg-surface px-3 py-1 text-xs hover:bg-card-active"
        >
          {t('cull.done', { defaultValue: 'Done' })}
        </button>
      </div>

      <div className="flex min-h-0 flex-1 gap-3 p-4">
        {photo(index, !focusRight, () => setFocusRight(false))}
        {compare && photo(index + 1, focusRight, () => setFocusRight(true))}
      </div>

      <div className="shrink-0 border-t border-surface px-4 py-2">
        <div className="flex justify-center gap-1.5 overflow-hidden">
          {strip.map((p) => {
            const i = paths.indexOf(p);
            const f = byPath.get(p);
            return (
              <button
                key={p}
                type="button"
                onClick={() => go(i)}
                className={clsx(
                  'relative h-14 w-20 shrink-0 overflow-hidden rounded bg-surface',
                  i === index ? 'ring-2 ring-accent' : 'opacity-70 hover:opacity-100',
                )}
              >
                {thumbnails[p] && (
                  <img
                    src={thumbnails[p]}
                    alt=""
                    draggable={false}
                    className={clsx('h-full w-full object-cover', f?.flag === 'reject' && 'opacity-35')}
                  />
                )}
                {f?.flag === 'pick' && <Flag size={11} className="absolute left-1 top-1 fill-white text-white" />}
                {f?.flag === 'reject' && <X size={12} className="absolute left-1 top-1 text-white" />}
              </button>
            );
          })}
        </div>
        <p className="mt-2 text-center text-[11px] text-text-secondary">
          {t('cull.hints', {
            defaultValue:
              'P pick · X reject · U clear · 1–5 stars · 6–9 labels · ←/→ move · C compare · A auto-advance · Enter edit · Esc done',
          })}
        </p>
      </div>
    </div>
  );
}

/** Flag, stars and label for the photo on screen. */
function PhotoBadges({ file, rating }: { file?: ImageFile; rating: number }) {
  const color = file?.tags?.find((tag) => tag.startsWith('color:'))?.slice(6);
  const label = COLOR_LABELS.find((c) => c.name === color);
  return (
    <div className="pointer-events-none absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-3 rounded-full bg-black/55 px-3 py-1.5 text-white backdrop-blur">
      {file?.flag === 'pick' ? (
        <Flag size={14} className="fill-white" />
      ) : file?.flag === 'reject' ? (
        <X size={15} />
      ) : (
        <FlagOff size={14} className="opacity-40" />
      )}
      <div className="flex items-center gap-0.5">
        {[1, 2, 3, 4, 5].map((n) => (
          <Star key={n} size={13} className={n <= rating ? 'fill-white' : 'opacity-35'} />
        ))}
      </div>
      {label && <span className="h-3 w-3 rounded-full" style={{ backgroundColor: label.color }} />}
    </div>
  );
}
