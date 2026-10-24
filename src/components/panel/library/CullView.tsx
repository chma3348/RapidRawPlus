import { PointerEvent as ReactPointerEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import {
  Columns2,
  Flag,
  FlagOff,
  Image as ImageIcon,
  Keyboard,
  SlidersHorizontal,
  Star,
  X,
  ArrowRightLeft,
} from 'lucide-react';
import { toast } from 'react-toastify';
import { ImageFile, Invokes } from '../../ui/AppProperties';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { useProcessStore } from '../../../store/useProcessStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { isVideoPath } from '../../../utils/media';
import { COLOR_LABELS } from '../../../utils/adjustments';
import { setFlagForPaths } from '../../../utils/flags';
import { afterCulling } from '../../../utils/rejectsAfterCull';

/**
 * Culling: going through a shoot quickly, one big photo at a time,
 * deciding with single keys and moving on — the way Lightroom's loupe and
 * darktable's culling layout work.
 *
 *   Z pick (P too) · X reject · U clear flag · 1–5 stars (0 clears) ·
 *   6 red, 7 yellow, 8 green, 9 blue label · ←/→ previous/next ·
 *   Space / click zoom to 100% and back · ⌘= ⌘− zoom · C compare side by
 *   side · A auto-advance · Enter edit · ? keys · Esc done
 *
 * Photos zoom and pan as in the editor, at full size when zoomed (for
 * focus), and in Compare both zoom and pan together.
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
  /** Close at the end of a cull (Done or Esc), then deal with what was rejected. */
  finishCull: () => void;
}

export const useCullStore = create<CullState>((set, get) => ({
  open: false,
  paths: [],
  startPath: null,
  openCull: (paths, startPath) => set({ open: paths.length > 0, paths, startPath }),
  closeCull: () => set({ open: false }),
  finishCull: () => {
    set({ open: false });
    afterCulling(get().paths);
  },
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

/** Previews loading at once: the backend makes several in parallel. */
const PARALLEL = 3;

/**
 * Previews of the photos around the current one, quick to make (see
 * cull_preview.rs): the camera's JPEG inside a RAW, or a JPEG at screen
 * size; developed with its edits when `showEdits` is on. The current photo
 * comes first, then the next six (so the next ones are always ready at a
 * decision a second), then the two before; three load at once.
 */
function usePreviews(paths: string[], index: number, showEdits: boolean) {
  const [urls, setUrls] = useState<Record<string, string>>({});
  const urlsRef = useRef<Record<string, string>>({});
  const pending = useRef(new Set<string>());
  const queue = useRef<string[]>([]);
  const running = useRef(0);
  const showEditsRef = useRef(showEdits);

  // Changing what is shown starts again.
  useEffect(() => {
    if (showEditsRef.current === showEdits) return;
    showEditsRef.current = showEdits;
    Object.values(urlsRef.current).forEach((u) => URL.revokeObjectURL(u));
    urlsRef.current = {};
    setUrls({});
  }, [showEdits]);

  const load = useCallback(async (path: string) => {
    const withEdits = showEditsRef.current;
    try {
      let adjustments: unknown = {};
      if (withEdits) {
        const metadata = await invoke<{ adjustments?: unknown } | null>(Invokes.LoadMetadata, { path });
        if (metadata?.adjustments && typeof metadata.adjustments === 'object') adjustments = metadata.adjustments;
      }
      const bytes: Uint8Array = await invoke(Invokes.GenerateCullPreview, {
        path,
        showEdits: withEdits,
        jsAdjustments: adjustments,
      });
      if (withEdits !== showEditsRef.current) return;
      const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'image/jpeg' }));
      urlsRef.current = { ...urlsRef.current, [path]: url };
      setUrls(urlsRef.current);
    } catch {
      // The library thumbnail stays on screen.
    }
  }, []);

  const pump = useCallback(() => {
    while (running.current < PARALLEL && queue.current.length > 0) {
      const path = queue.current.shift()!;
      if (urlsRef.current[path] || isVideoPath(path)) {
        pending.current.delete(path);
        continue;
      }
      running.current += 1;
      void load(path).finally(() => {
        running.current -= 1;
        pending.current.delete(path);
        pump();
      });
    }
  }, [load]);

  useEffect(() => {
    const around = [index, index + 1, index + 2, index + 3, index + 4, index + 5, index + 6, index - 1, index - 2]
      .filter((i) => i >= 0 && i < paths.length)
      .map((i) => paths[i]);
    // Only the photos around this one: moving on drops what was queued for
    // photos already passed.
    queue.current.filter((p) => !around.includes(p)).forEach((p) => pending.current.delete(p));
    const stillWanted = queue.current.filter((p) => around.includes(p));
    const wanted = around.filter((p) => !urlsRef.current[p] && !pending.current.has(p));
    wanted.forEach((p) => pending.current.add(p));
    queue.current = [...wanted, ...stillWanted];
    pump();

    // Keep a small window of previews; let the rest go.
    const keep = new Set(paths.slice(Math.max(0, index - 3), index + 8));
    const next: Record<string, string> = {};
    for (const [p, url] of Object.entries(urlsRef.current)) {
      if (keep.has(p)) next[p] = url;
      else URL.revokeObjectURL(url);
    }
    if (Object.keys(next).length !== Object.keys(urlsRef.current).length) {
      urlsRef.current = next;
      setUrls(next);
    }
  }, [paths, index, pump, showEdits]);

  useEffect(
    () => () => {
      Object.values(urlsRef.current).forEach((u) => URL.revokeObjectURL(u));
    },
    [],
  );

  return urls;
}

/**
 * How a cull photo is zoomed: `scale` against the fitted size (1 is the
 * whole photo), and the point of the photo at the middle of the view
 * (`cx`, `cy`, 0–1 across and down). Shared by both photos in Compare, so
 * they zoom and pan together, and kept from photo to photo, as Lightroom
 * keeps it: the same spot of each frame in a burst, for focus.
 */
interface Zoom {
  scale: number;
  cx: number;
  cy: number;
}
const FIT: Zoom = { scale: 1, cx: 0.5, cy: 0.5 };
const MAX_ZOOM_FROM_FIT = 40;

/** The photo itself, full size, for focus at 100% (the previews are 1920 px). */
function useFullSize(path: string | undefined, wanted: boolean, zoomed: boolean) {
  const [full, setFull] = useState<{ path: string; url: string; width: number; height: number } | null>(null);
  useEffect(() => {
    if (!path || !wanted || !zoomed || isVideoPath(path) || full?.path === path) return;
    let cancelled = false;
    let url: string | null = null;
    // Only when zoomed: rendering every photo rested on at full size held
    // up the previews of the next ones.
    const timer = window.setTimeout(async () => {
      try {
        const metadata = await invoke<{ adjustments?: unknown } | null>(Invokes.LoadMetadata, { path });
        const adjustments =
          metadata?.adjustments && typeof metadata.adjustments === 'object' ? metadata.adjustments : {};
        const bytes: Uint8Array = await invoke(Invokes.GeneratePreviewForPath, {
          path: path.split('?vc=')[0],
          jsAdjustments: adjustments,
          maxDimension: 0,
        });
        if (cancelled) return;
        url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'image/png' }));
        const img = new Image();
        img.src = url;
        await img.decode();
        if (cancelled) {
          URL.revokeObjectURL(url);
          return;
        }
        setFull((old) => {
          if (old) URL.revokeObjectURL(old.url);
          return { path, url: url!, width: img.naturalWidth, height: img.naturalHeight };
        });
      } catch {
        // The 1920-pixel preview stays.
      }
    }, 0);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [path, wanted, zoomed, full?.path]);
  useEffect(
    () => () => {
      if (full) URL.revokeObjectURL(full.url);
    },
    [full],
  );
  return full?.path === path ? full : null;
}

/**
 * One photo in Cull, zoomable like the editor: click (or Space) toggles
 * fit and 100% at that spot; the wheel or a pinch zooms, dragging or a
 * two-finger scroll pans.
 */
function CullPhoto({
  path,
  src,
  full,
  zoom,
  setZoom,
  rejected,
  focused,
  outlined,
  onFocus,
  command,
  children,
}: {
  path: string;
  src?: string;
  full: { url: string; width: number; height: number } | null;
  zoom: Zoom;
  setZoom: (z: Zoom | ((z: Zoom) => Zoom)) => void;
  rejected: boolean;
  focused: boolean;
  outlined: boolean;
  onFocus?: () => void;
  /** A zoom asked for by key (Space, ⌘= / ⌘−), acted on by the focused photo. */
  command?: { id: number; kind: 'toggle' | 'in' | 'out' } | null;
  children?: React.ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [natural, setNatural] = useState({ w: 0, h: 0 });
  const [animate, setAnimate] = useState(false);
  const drag = useRef<{ x: number; y: number; moved: boolean; zoom: Zoom } | null>(null);
  const trackpad = useSettingsStore((s) => s.appSettings?.canvasInputMode === 'trackpad');

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // The fitted size, and the scale at which one pixel of the photo is one
  // pixel of the screen.
  const aspect = full ? full.width / full.height : natural.w && natural.h ? natural.w / natural.h : 1.5;
  const fitW = Math.max(1, Math.min(size.w, size.h * aspect));
  const fitH = fitW / aspect;
  const dpr = window.devicePixelRatio || 1;
  const fullWidth = full?.width ?? 0;
  const oneToOne = fullWidth ? fullWidth / (fitW * dpr) : Math.max(2, natural.w / (fitW * dpr));

  // A zoom to 100% made before the full size arrived used a guess; when
  // it arrives, go to exactly 100% on the same spot.
  const wants100 = useRef(false);
  useEffect(() => {
    if (full && focused && wants100.current && zoom.scale > 1.01) {
      wants100.current = false;
      setZoom((z) => ({ ...z, scale: Math.min(MAX_ZOOM_FROM_FIT, oneToOne) }));
    }
  }, [full, focused, oneToOne, zoom.scale, setZoom]);

  const shownW = fitW * zoom.scale;
  const shownH = fitH * zoom.scale;
  const place = (centre: number, shown: number, room: number) =>
    shown <= room ? (room - shown) / 2 : Math.min(0, Math.max(room - shown, room / 2 - centre * shown));
  const left = place(zoom.cx, shownW, size.w);
  const top = place(zoom.cy, shownH, size.h);

  // The photo point under a screen point, 0–1.
  const pointAt = (clientX: number, clientY: number) => {
    const rect = box.current!.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (clientX - rect.left - left) / shownW)),
      y: Math.min(1, Math.max(0, (clientY - rect.top - top) / shownH)),
    };
  };

  // Zoom to `scale`, keeping the photo point under (clientX, clientY) there.
  const zoomAround = (scale: number, clientX: number, clientY: number) => {
    const rect = box.current!.getBoundingClientRect();
    const p = pointAt(clientX, clientY);
    const next = Math.min(MAX_ZOOM_FROM_FIT, Math.max(1, scale));
    if (next <= 1.0001) return setZoom(FIT);
    const offX = clientX - rect.left - size.w / 2;
    const offY = clientY - rect.top - size.h / 2;
    setZoom({ scale: next, cx: p.x - offX / (fitW * next), cy: p.y - offY / (fitH * next) });
  };

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      setAnimate(false);
      wants100.current = false;
      const pinch = e.ctrlKey;
      if (pinch || (!trackpad && !e.shiftKey)) {
        const delta = e.deltaY !== 0 ? e.deltaY : e.deltaX;
        zoomAround(zoom.scale * Math.exp(-delta * (pinch ? 0.01 : 0.002)), e.clientX, e.clientY);
      } else if (zoom.scale > 1) {
        setZoom((z) => ({ ...z, cx: z.cx + e.deltaX / (fitW * z.scale), cy: z.cy + e.deltaY / (fitH * z.scale) }));
      }
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  });

  const onPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    (e.target as Element).setPointerCapture?.(e.pointerId);
    drag.current = { x: e.clientX, y: e.clientY, moved: false, zoom };
  };
  const onPointerMove = (e: ReactPointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x;
    const dy = e.clientY - d.y;
    if (!d.moved && Math.hypot(dx, dy) < 4) return;
    d.moved = true;
    if (d.zoom.scale <= 1) return;
    setAnimate(false);
    setZoom({ ...d.zoom, cx: d.zoom.cx - dx / (fitW * d.zoom.scale), cy: d.zoom.cy - dy / (fitH * d.zoom.scale) });
  };
  const onPointerUp = (e: ReactPointerEvent) => {
    const d = drag.current;
    drag.current = null;
    if (!d || d.moved) return;
    onFocus?.();
    setAnimate(true);
    if (zoom.scale > 1.01) setZoom(FIT);
    else {
      wants100.current = !full;
      zoomAround(oneToOne, e.clientX, e.clientY);
    }
  };

  // Keys zoom about the middle of the view.
  const lastCommand = useRef(0);
  useEffect(() => {
    if (!command || !focused || command.id === lastCommand.current || !box.current) return;
    lastCommand.current = command.id;
    const rect = box.current.getBoundingClientRect();
    const [x, y] = [rect.left + size.w / 2, rect.top + size.h / 2];
    setAnimate(true);
    if (command.kind === 'toggle') {
      if (zoom.scale > 1.01) setZoom(FIT);
      else {
        wants100.current = !full;
        zoomAround(oneToOne, x, y);
      }
    } else {
      wants100.current = false;
      zoomAround(zoom.scale * (command.kind === 'in' ? 1.5 : 1 / 1.5), x, y);
    }
  });

  const shown = full?.url ?? src;
  return (
    <div
      ref={box}
      className={clsx(
        'relative min-h-0 min-w-0 flex-1 overflow-hidden rounded-lg',
        outlined && focused && 'ring-2 ring-accent',
        zoom.scale > 1 ? 'cursor-grab active:cursor-grabbing' : 'cursor-zoom-in',
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      data-path={path}
    >
      {shown ? (
        <img
          src={shown}
          alt=""
          draggable={false}
          onLoad={(e) => {
            if (!full) setNatural({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight });
          }}
          className={clsx('absolute max-w-none select-none', rejected && 'opacity-35')}
          style={{
            left,
            top,
            width: shownW,
            height: shownH,
            transition: animate
              ? 'left 160ms ease-out, top 160ms ease-out, width 160ms ease-out, height 160ms ease-out'
              : undefined,
          }}
        />
      ) : (
        <div className="flex h-full items-center justify-center">
          <ImageIcon size={48} className="text-text-secondary" />
        </div>
      )}
      {zoom.scale > 1.01 && (
        <div className="pointer-events-none absolute right-3 top-3 rounded-full bg-black/55 px-2.5 py-1 text-xs tabular-nums text-white">
          {Math.round((zoom.scale / oneToOne) * 100)}%{!full && ' · loading full size…'}
        </div>
      )}
      {children}
    </div>
  );
}

export default function CullView({ onEdit }: { onEdit: (path: string) => void }) {
  const { t } = useTranslation();
  const { open, paths, startPath, closeCull, finishCull } = useCullStore();
  const imageList = useLibraryStore((s) => s.imageList);
  const imageRatings = useLibraryStore((s) => s.imageRatings);
  const thumbnails = useProcessStore((s) => s.thumbnails);
  const [index, setIndex] = useState(0);
  const [autoAdvance, setAutoAdvance] = useState(true);
  const [compare, setCompare] = useState(false);
  const [focusRight, setFocusRight] = useState(false);
  const [zoom, setZoom] = useState<Zoom>(FIT);
  // The camera's previews by default (fast); developed with edits on ask.
  const [showEdits, setShowEdits] = useState(() => {
    try {
      return localStorage.getItem('darkroom.cullShowEdits') === '1';
    } catch {
      return false;
    }
  });
  const toggleShowEdits = useCallback(() => {
    setShowEdits((v) => {
      try {
        localStorage.setItem('darkroom.cullShowEdits', v ? '0' : '1');
      } catch {
        // Not remembered; harmless.
      }
      return !v;
    });
  }, []);

  useEffect(() => {
    if (!open) return;
    const start = startPath ? paths.indexOf(startPath) : 0;
    setIndex(Math.max(0, start));
    setCompare(false);
    setFocusRight(false);
    setZoom(FIT);
  }, [open, paths, startPath]);

  const byPath = useMemo(() => new Map(imageList.map((f) => [f.path, f])), [imageList]);
  const previews = usePreviews(open ? paths : [], index, showEdits);
  const target = Math.min(paths.length - 1, index + (compare && focusRight ? 1 : 0));
  const targetPath = paths[target];
  // Full size for the photos on screen, once there is a moment to look.
  const zoomed = zoom.scale > 1.01;
  const fullLeft = useFullSize(open ? paths[index] : undefined, open, zoomed);
  const fullRight = useFullSize(open && compare ? paths[index + 1] : undefined, open && compare, zoomed);

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

  // The shortcut card: shown the first time, and on ? after that.
  const [showKeys, setShowKeys] = useState(false);
  useEffect(() => {
    if (!open) return;
    try {
      setShowKeys(!localStorage.getItem('darkroom.cullKeysSeen'));
    } catch {
      setShowKeys(false);
    }
  }, [open]);
  const closeKeys = useCallback(() => {
    setShowKeys(false);
    try {
      localStorage.setItem('darkroom.cullKeysSeen', '1');
    } catch {
      // Shown again next time; harmless.
    }
  }, []);

  const [command, setCommand] = useState<{ id: number; kind: 'toggle' | 'in' | 'out' } | null>(null);
  const zoomBy = useCallback(
    (kind: 'toggle' | 'in' | 'out') => setCommand((c) => ({ id: (c?.id ?? 0) + 1, kind })),
    [],
  );

  useEffect(() => {
    if (!open) return;
    // Lightroom's keys (the Lightroom desktop app): Z pick (P too), X
    // reject, U clear, 0–5 stars; Space toggles zoom, ⌘= / ⌘− zoom.
    const onKey = (e: KeyboardEvent) => {
      const key = e.key.toLowerCase();
      let handled = true;
      if (e.metaKey || e.ctrlKey) {
        if (key === '=' || key === '+') zoomBy('in');
        else if (key === '-') zoomBy('out');
        else handled = false;
      } else if (e.altKey) {
        handled = false;
      } else if (key === 'escape') {
        if (showKeys) closeKeys();
        else finishCull();
      } else if (key === '?' || (key === '/' && e.shiftKey)) setShowKeys((v) => !v);
      else if (key === ' ') zoomBy('toggle');
      else if (key === 'arrowright') go(index + 1);
      else if (key === 'arrowleft') go(index - 1);
      else if (key === 'z' || key === 'p') setFlag('pick');
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
  }, [
    open,
    index,
    go,
    setFlag,
    setRating,
    setLabel,
    closeCull,
    finishCull,
    compare,
    targetPath,
    onEdit,
    showKeys,
    closeKeys,
    zoomBy,
  ]);

  if (!open || paths.length === 0) return null;

  const photo = (i: number, focused: boolean, full: ReturnType<typeof useFullSize>, onFocus?: () => void) => {
    const path = paths[i];
    if (!path) return <div className="flex-1" />;
    const file = byPath.get(path);
    return (
      <CullPhoto
        key={path}
        path={path}
        src={previews[path] || thumbnails[path]}
        full={full}
        zoom={zoom}
        setZoom={setZoom}
        rejected={file?.flag === 'reject'}
        focused={focused}
        outlined={compare}
        onFocus={onFocus}
        command={command}
      >
        <PhotoBadges file={file} rating={imageRatings[path] || 0} />
      </CullPhoto>
    );
  };

  const targetFile = targetPath ? byPath.get(targetPath) : undefined;
  const targetRating = targetPath ? imageRatings[targetPath] || 0 : 0;

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
          onClick={toggleShowEdits}
          aria-pressed={showEdits}
          className={clsx(
            'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs',
            showEdits ? 'bg-card-active' : 'text-text-secondary hover:bg-surface',
          )}
          data-tooltip={t('cull.showEditsTip', {
            defaultValue:
              'Off: the camera’s own preview, instant. On: each photo developed with its edits, slower. Zooming in always shows the developed photo.',
          })}
        >
          <SlidersHorizontal size={13} />
          {t('cull.showEdits', { defaultValue: 'Show edits' })}
        </button>
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
          onClick={finishCull}
          className="rounded-md bg-surface px-3 py-1 text-xs hover:bg-card-active"
        >
          {t('cull.done', { defaultValue: 'Done' })}
        </button>
      </div>

      <div className="flex min-h-0 flex-1 gap-3 p-4">
        {photo(index, !focusRight, fullLeft, () => setFocusRight(false))}
        {compare && photo(index + 1, focusRight, fullRight, () => setFocusRight(true))}
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
        <div className="mt-3 flex items-center justify-center gap-3">
          <DecisionButton
            active={targetFile?.flag === 'reject'}
            tone="reject"
            onClick={() => setFlag('reject')}
            label={t('cull.reject', { defaultValue: 'Reject' })}
            keyName="X"
            icon={<X size={20} />}
          />
          <button
            type="button"
            onClick={() => setFlag(null)}
            className="rounded-lg px-3 py-3 text-xs text-text-secondary hover:bg-surface"
            data-tooltip={t('cull.unflagTip', { defaultValue: 'Clear the flag (U)' })}
          >
            <FlagOff size={16} />
          </button>
          <DecisionButton
            active={targetFile?.flag === 'pick'}
            tone="pick"
            onClick={() => setFlag('pick')}
            label={t('cull.pick', { defaultValue: 'Pick' })}
            keyName="Z"
            icon={<Flag size={18} />}
          />
          <div
            className="ml-4 flex items-center gap-1"
            role="group"
            aria-label={t('cull.stars', { defaultValue: 'Stars' })}
          >
            {[1, 2, 3, 4, 5].map((n) => (
              <button
                key={n}
                type="button"
                onClick={() => setRating(targetRating === n ? 0 : n)}
                className="rounded p-1 hover:bg-surface"
                data-tooltip={`${n} (${n})`}
              >
                <Star size={20} className={n <= targetRating ? 'fill-accent text-accent' : 'text-text-secondary'} />
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={() => setShowKeys((v) => !v)}
            className="ml-4 flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-text-secondary hover:bg-surface"
          >
            <Keyboard size={14} />
            {t('cull.shortcuts', { defaultValue: 'Shortcuts' })} <Kbd>?</Kbd>
          </button>
        </div>
      </div>
      {showKeys && <CullKeys onClose={closeKeys} />}
    </div>
  );
}

/** A key, as printed on it. */
function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="inline-flex min-w-[1.5em] items-center justify-center rounded border border-border-color bg-bg-primary px-1.5 py-0.5 font-sans text-[11px] text-text-primary">
      {children}
    </kbd>
  );
}

/** Pick or Reject: big, with its key, filled when it is the photo's flag. */
function DecisionButton({
  active,
  tone,
  onClick,
  label,
  keyName,
  icon,
}: {
  active: boolean;
  tone: 'pick' | 'reject';
  onClick: () => void;
  label: string;
  keyName: string;
  icon: React.ReactNode;
}) {
  const pick = tone === 'pick';
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={clsx(
        'flex min-w-[9rem] items-center justify-center gap-2.5 rounded-xl border-2 px-5 py-3 text-base font-semibold transition-colors',
        pick
          ? active
            ? 'border-emerald-500 bg-emerald-500 text-white'
            : 'border-emerald-500/60 text-emerald-400 hover:bg-emerald-500/15'
          : active
            ? 'border-red-500 bg-red-500 text-white'
            : 'border-red-500/60 text-red-400 hover:bg-red-500/15',
      )}
    >
      {icon}
      {label}
      <Kbd>{keyName}</Kbd>
    </button>
  );
}

/** The cull keys, on a card over the photos: the first time, and on ?. */
function CullKeys({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const rows: [string, string][] = [
    ['Z', t('cull.keys.pick', { defaultValue: 'Pick (P works too)' })],
    ['X', t('cull.keys.reject', { defaultValue: 'Reject' })],
    ['U', t('cull.keys.clear', { defaultValue: 'Clear the flag' })],
    ['1 – 5', t('cull.keys.stars', { defaultValue: 'Stars (0 clears)' })],
    ['6 – 9', t('cull.keys.labels', { defaultValue: 'Red, yellow, green, blue label' })],
    ['← →', t('cull.keys.move', { defaultValue: 'Previous, next photo' })],
    [
      t('cull.keys.spaceKey', { defaultValue: 'Space' }),
      t('cull.keys.zoom', { defaultValue: 'Zoom to 100% and back (or click the photo)' }),
    ],
    ['⌘=  ⌘−', t('cull.keys.zoomStep', { defaultValue: 'Zoom in, out (or scroll, pinch); drag to move around' })],
    ['C', t('cull.keys.compare', { defaultValue: 'Compare with the next photo (zooms together)' })],
    ['Tab', t('cull.keys.switch', { defaultValue: 'Switch which photo you are deciding on, in Compare' })],
    ['A', t('cull.keys.auto', { defaultValue: 'Auto-advance on or off' })],
    ['Enter', t('cull.keys.edit', { defaultValue: 'Edit this photo' })],
    ['Esc', t('cull.keys.done', { defaultValue: 'Done' })],
    ['?', t('cull.keys.help', { defaultValue: 'Show or hide this card' })],
  ];
  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/40" onClick={onClose}>
      <div
        className="w-[440px] max-w-[92vw] rounded-xl bg-surface p-5 text-sm text-text-primary shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <span className="flex items-center gap-2 text-base font-medium">
            <Keyboard size={18} />
            {t('cull.keys.title', { defaultValue: 'Culling keys' })}
          </span>
          <button type="button" onClick={onClose} className="rounded p-1 text-text-secondary hover:bg-card-active">
            <X size={16} />
          </button>
        </div>
        <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2">
          {rows.map(([key, what]) => (
            <div key={key} className="contents">
              <dt className="text-right">
                <Kbd>{key}</Kbd>
              </dt>
              <dd className="text-text-secondary">{what}</dd>
            </div>
          ))}
        </dl>
        <div className="mt-4 flex justify-end">
          <button type="button" onClick={onClose} className="rounded-md bg-accent px-4 py-1.5 text-button-text">
            {t('cull.keys.gotIt', { defaultValue: 'Got it' })}
          </button>
        </div>
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
