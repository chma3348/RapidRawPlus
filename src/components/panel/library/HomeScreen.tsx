import { ReactNode, RefObject, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import {
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Download,
  EyeOff,
  Folder,
  FolderOpen,
  FolderPlus,
  Play,
  Settings,
  Star,
  Upload,
} from 'lucide-react';

import Button from '../../ui/Button';
import Text from '../../ui/Text';
import { AppSettings, ImageFile, Invokes } from '../../ui/AppProperties';
import { FolderTree } from '../FolderTree';
import { useProcessStore } from '../../../store/useProcessStore';
import { TextColors, TextVariants, TextWeights } from '../../../types/typography';
import { isVideoPath } from '../../../utils/media';

/** How many photos a shelf shows before "See all". */
const SHELF_LENGTH = 40;

/** Drift speeds in pixels a second, shelf by shelf, so neighbours never move in step. */
const DRIFT_SPEEDS = [16, 23, 19, 27, 14, 21, 25, 17];

/** The banner's strip of neighbouring photos drifts gently, slower than the shelves. */
const BANNER_DRIFT_SPEED = 9;

/** Page margin and how far each folder level is indented, in pixels. */
const PAGE_PAD = 32;
const INDENT = 26;

const realPath = (path: string) => path.split('?vc=')[0];
const baseName = (path: string) => realPath(path).split(/[\\/]/).pop() || path;
const parentOf = (path: string) => realPath(path).replace(/[\\/][^\\/]*$/, '');
const isWithin = (path: string, folder: string) =>
  path === folder || path.startsWith(folder + '/') || path.startsWith(folder + '\\');

export interface HomeTarget {
  folder?: string;
  image?: string;
  /** After opening the folder: cull what's left in it, or select its picks for export. */
  action?: 'cull' | 'exportPicks';
}

interface HomeScreenProps {
  appSettings: AppSettings;
  brand: ReactNode;
  footer: ReactNode;
  onAddFolder(): void;
  /** Pick photos to copy into `folder` (the library's Import). */
  onImportInto(folder: string): void;
  onOpen(target: HomeTarget): void;
  onOpenSettings(): void;
  onSettingsChange(settings: AppSettings): void;
  requestThumbnails(paths: string[]): void;
}

/** One folder on the page, in tree order. */
interface FolderRow {
  path: string;
  name: string;
  /** 0 for a library folder, 1 for a folder inside it, and so on. */
  depth: number;
  hasChildren: boolean;
  /** Where it sits on disk, shown under a library folder's name. */
  location?: string;
  /** For each level above it: whether the guide line carries on past this row. */
  linesContinue: boolean[];
  /** Its children follow it on the page, so a guide line drops from its icon. */
  startsLine: boolean;
}

const byName = (a: FolderTree, b: FolderTree) =>
  a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });

/**
 * Lay the library out as a tree: each library folder, then the folders inside
 * it, indented. A library folder that also sits inside another one is shown
 * only in its place in that tree. Hidden folders drop out with everything in
 * them, and a collapsed folder keeps its heading but not its contents.
 */
const folderRows = (trees: FolderTree[], hidden: string[], collapsed: string[]): FolderRow[] => {
  const tops = trees.filter((t) => !trees.some((o) => o.path !== t.path && isWithin(t.path, o.path)));
  const rows: Omit<FolderRow, 'linesContinue' | 'startsLine'>[] = [];
  const walk = (node: FolderTree, depth: number) => {
    if (hidden.some((h) => isWithin(node.path, h))) return;
    const children = (node.children || []).filter((c) => c.isDir).sort(byName);
    rows.push({
      path: node.path,
      name: node.name,
      depth,
      hasChildren: children.length > 0,
      location: depth === 0 ? parentOf(node.path) : undefined,
    });
    if (!collapsed.includes(node.path)) children.forEach((c) => walk(c, depth + 1));
  };
  tops.forEach((t) => walk(t, 0));
  // A guide line at level k runs on past a row while a later row still sits deeper than k.
  return rows.map((row, i) => ({
    ...row,
    startsLine: rows[i + 1]?.depth === row.depth + 1,
    linesContinue: Array.from({ length: row.depth }, (_, k) => {
      for (let j = i + 1; j < rows.length; j++) {
        if (rows[j].depth <= k) return false;
        if (rows[j].depth === k + 1) return true;
      }
      return false;
    }),
  }));
};

/** Newest photos first; copies are left to the library. */
const shelfPhotos = (files: ImageFile[]) =>
  files.filter((f) => !f.is_virtual_copy && !f.path.includes('?vc=')).sort((a, b) => b.modified - a.modified);

const useThumbnail = (path: string | undefined, requestThumbnails: (paths: string[]) => void) => {
  const url = useProcessStore((state) => (path ? state.thumbnails[path] : undefined));
  useEffect(() => {
    if (path && !url) requestThumbnails([path]);
  }, [path, url, requestThumbnails]);
  return url;
};

/** Read a folder once its element comes near the screen. */
const useFolderPhotos = (path: string, ref: RefObject<HTMLElement | null>) => {
  const [photos, setPhotos] = useState<ImageFile[] | null>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el || photos) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        observer.disconnect();
        invoke<ImageFile[]>(Invokes.ListImagesInDir, { path })
          .then((files) => setPhotos(shelfPhotos(files)))
          .catch(() => setPhotos([]));
      },
      { rootMargin: '400px 0px' },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [path, photos, ref]);
  return photos;
};

/**
 * Drift a strip of photos along by itself. The photos are laid out twice and
 * the track is moved with a transform, which lands on fractions of a pixel,
 * so slow speeds glide rather than stepping a whole pixel at a time; its
 * position wraps by exactly one set's width, so the loop has no seam. It
 * eases to a stop under the pointer and back up to speed afterwards, rests
 * while off screen and for people who have asked their system for reduced
 * motion, and a sideways trackpad swipe or an arrow moves it by hand.
 */
const useDrift = (
  viewportRef: RefObject<HTMLDivElement | null>,
  trackRef: RefObject<HTMLDivElement | null>,
  firstRef: RefObject<HTMLDivElement | null>,
  secondRef: RefObject<HTMLDivElement | null>,
  /** Pixels a second; negative drifts the other way. */
  speed: number,
  enabled: boolean,
) => {
  const paused = useRef(false);
  const offset = useRef(0);
  /** Distance still to travel from an arrow press, used up over a fraction of a second. */
  const glide = useRef(0);

  useEffect(() => {
    const viewport = viewportRef.current;
    const track = trackRef.current;
    if (!viewport || !track) return;
    if (!enabled) {
      offset.current = 0;
      track.style.transform = '';
      return;
    }
    const still = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    const period = () => (secondRef.current?.offsetLeft ?? 0) - (firstRef.current?.offsetLeft ?? 0);
    const place = () => {
      const p = period();
      if (p <= 0) return;
      offset.current = ((offset.current % p) + p) % p;
      track.style.transform = `translate3d(${-offset.current}px, 0, 0)`;
    };

    let visible = true;
    const observer = new IntersectionObserver((entries) => {
      visible = entries.some((e) => e.isIntersecting);
    });
    observer.observe(viewport);

    // A sideways swipe moves the strip; an up-and-down one still scrolls the page.
    const onWheel = (e: WheelEvent) => {
      const dx = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.shiftKey ? e.deltaY : 0;
      if (!dx) return;
      e.preventDefault();
      offset.current += dx;
      place();
    };
    viewport.addEventListener('wheel', onWheel, { passive: false });

    let velocity = still ? 0 : speed;
    let last = performance.now();
    let frame = requestAnimationFrame(function step(now) {
      const dt = Math.min(now - last, 100) / 1000;
      last = now;
      if (visible) {
        const target = paused.current || still ? 0 : speed;
        velocity += (target - velocity) * (1 - Math.exp(-dt / 0.35));
        const glideStep = glide.current * (1 - Math.exp(-dt / 0.12));
        glide.current -= glideStep;
        if (Math.abs(glide.current) < 0.1) glide.current = 0;
        if (Math.abs(velocity) > 0.01 || glideStep) {
          offset.current += velocity * dt + glideStep;
          place();
        }
      }
      frame = requestAnimationFrame(step);
    });
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      viewport.removeEventListener('wheel', onWheel);
    };
  }, [viewportRef, trackRef, firstRef, secondRef, speed, enabled]);

  return {
    pause: () => (paused.current = true),
    resume: () => (paused.current = false),
    /** Glide most of a strip's width one way or the other. */
    step: (direction: number) => {
      glide.current += direction * (viewportRef.current?.clientWidth ?? 0) * 0.8;
    },
  };
};

function StripPhoto({
  path,
  height,
  copy,
  highlighted,
  onOpen,
  requestThumbnails,
}: {
  path: string;
  height: number;
  /** The repeat that lets a drifting strip loop: hidden from keyboard and screen readers. */
  copy?: boolean;
  highlighted?: boolean;
  onOpen(): void;
  requestThumbnails(paths: string[]): void;
}) {
  const url = useThumbnail(path, requestThumbnails);
  const video = isVideoPath(path);
  return (
    <button
      className={`relative shrink-0 rounded-md overflow-hidden bg-surface group/photo focus:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
        highlighted ? 'ring-2 ring-inset ring-accent' : ''
      }`}
      style={{ height }}
      onClick={onOpen}
      tabIndex={copy ? -1 : undefined}
      aria-hidden={copy || undefined}
      data-tooltip={baseName(path)}
    >
      {url ? (
        <img
          src={url}
          alt={baseName(path)}
          draggable={false}
          className="h-full w-auto object-cover transition-transform duration-300 group-hover/photo:scale-[1.03]"
          style={{ maxWidth: height * 2.4 }}
        />
      ) : (
        <div className="h-full animate-pulse" style={{ width: height * 1.4 }} />
      )}
      {video && (
        <span className="absolute bottom-1.5 left-1.5 rounded-full bg-black/60 p-1">
          <Play size={10} className="text-white" fill="white" />
        </span>
      )}
    </button>
  );
}

const fade = (left: boolean) => {
  const mask = `linear-gradient(to right, ${left ? 'transparent 0, black 28px' : 'black 0'}, black calc(100% - 28px), transparent 100%)`;
  return { maskImage: mask, WebkitMaskImage: mask };
};

/** A horizontal strip of photos that drifts along on its own and loops. */
function DriftStrip({
  photos,
  height,
  speed,
  arrows = true,
  fadeLeft = true,
  highlight,
  onOpenPhoto,
  requestThumbnails,
}: {
  /** null while the folder is still being read. */
  photos: string[] | null;
  height: number;
  /** Pixels a second; negative drifts the other way. */
  speed: number;
  arrows?: boolean;
  /** Soften the left end; off where the strip sits against a See all tile. */
  fadeLeft?: boolean;
  highlight?: string;
  onOpenPhoto(path: string): void;
  requestThumbnails(paths: string[]): void;
}) {
  const { t } = useTranslation();
  const viewportRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  const firstRef = useRef<HTMLDivElement>(null);
  const secondRef = useRef<HTMLDivElement>(null);
  // Loop only once the photos are wider than the strip.
  const [loops, setLoops] = useState(false);
  const drift = useDrift(viewportRef, trackRef, firstRef, secondRef, speed, loops);

  useEffect(() => {
    const first = firstRef.current;
    const viewport = viewportRef.current;
    if (!first || !viewport) return;
    const observer = new ResizeObserver(() => setLoops(first.offsetWidth > viewport.clientWidth));
    observer.observe(first);
    observer.observe(viewport);
    return () => observer.disconnect();
  }, [photos]);

  const set = (copy: boolean) =>
    photos!.map((p) => (
      <StripPhoto
        key={p}
        path={p}
        height={height}
        copy={copy}
        highlighted={p === highlight}
        onOpen={() => onOpenPhoto(p)}
        requestThumbnails={requestThumbnails}
      />
    ));

  const arrow =
    'absolute top-1/2 -translate-y-1/2 rounded-full bg-bg-primary/80 p-2 shadow-lg opacity-0 group-hover/strip:opacity-100 transition-opacity';
  return (
    <div className="relative group/strip min-w-0 flex-1" onPointerEnter={drift.pause} onPointerLeave={drift.resume}>
      <div ref={viewportRef} className="overflow-hidden" style={loops ? fade(fadeLeft) : undefined}>
        <div ref={trackRef} className="flex gap-2.5 w-max will-change-transform">
          <div ref={firstRef} className="flex gap-2.5 shrink-0">
            {photos
              ? set(false)
              : Array.from({ length: 6 }, (_, i) => (
                  <div
                    key={i}
                    className="shrink-0 rounded-md bg-surface animate-pulse"
                    style={{ height, width: height * 1.4 }}
                  />
                ))}
          </div>
          {loops && photos && (
            <div ref={secondRef} className="flex gap-2.5 shrink-0" aria-hidden="true">
              {set(true)}
            </div>
          )}
        </div>
      </div>
      {arrows && loops && (
        <>
          <button
            className={`${arrow} left-2`}
            onClick={() => drift.step(-1)}
            aria-label={t('library.home.scrollBack')}
          >
            <ChevronLeft size={20} />
          </button>
          <button
            className={`${arrow} right-2`}
            onClick={() => drift.step(1)}
            aria-label={t('library.home.scrollForward')}
          >
            <ChevronRight size={20} />
          </button>
        </>
      )}
    </div>
  );
}

/** The library folder holding `folder`, and the folder names from it down. */
const crumbsFor = (folder: string, roots: string[]) => {
  const root = roots.filter((r) => isWithin(folder, r)).sort((a, b) => a.length - b.length)[0];
  if (!root) return [{ name: baseName(folder), path: folder }];
  const crumbs = [{ name: baseName(root), path: root }];
  let path = root;
  for (const part of folder.slice(root.length).split(/[\\/]/).filter(Boolean)) {
    path = `${path}/${part}`;
    crumbs.push({ name: part, path });
  }
  return crumbs;
};

const formatDate = (exifDate?: string) => {
  const m = exifDate?.match(/^(\d{4}):(\d{2}):(\d{2})/);
  if (!m) return undefined;
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
  });
};

/** One label and value in the banner's details; left out when there's no value. */
function Spec({ label, children }: { label: string; children: ReactNode }) {
  if (!children) return null;
  return (
    <>
      <dt className="text-xs text-text-secondary/80 pt-px">{label}</dt>
      <dd className="text-sm text-text-primary break-words">{children}</dd>
    </>
  );
}

function Progress({ label, done, total }: { label: string; done: number; total: number }) {
  return (
    <div>
      <Text variant={TextVariants.small} color={TextColors.secondary} className="mb-1.5">
        {label}
      </Text>
      <div className="h-1.5 rounded-full bg-bg-primary/70 overflow-hidden">
        <div
          className="h-full rounded-full bg-accent transition-[width] duration-700"
          style={{ width: `${total ? (done / total) * 100 : 0}%` }}
        />
      </div>
    </div>
  );
}

/**
 * Back to where you were: the last-edited photo, its details and a large
 * Continue editing button, beside how far through its folder you are with
 * the next steps, over a filmstrip of the folder drifting along the bottom.
 */
function ContinueBanner({
  folder,
  image,
  roots,
  showPicture,
  onOpen,
  requestThumbnails,
}: {
  folder: string;
  image?: string;
  roots: string[];
  /** Off when the folder is hidden from Home: the banner then shows no photos. */
  showPicture: boolean;
  onOpen(target: HomeTarget): void;
  requestThumbnails(paths: string[]): void;
}) {
  const { t } = useTranslation();
  const [files, setFiles] = useState<ImageFile[] | null>(null);
  const [exif, setExif] = useState<Record<string, string> | null>(null);

  useEffect(() => {
    setFiles(null);
    invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: folder })
      .then((list) => setFiles(shelfPhotos(list)))
      .catch(() => setFiles([]));
  }, [folder]);

  // Without a remembered photo, the folder's newest one stands in.
  const cover = image || files?.find((f) => !isVideoPath(f.path))?.path;
  const info = files?.find((f) => f.path === cover);

  useEffect(() => {
    setExif(null);
    if (!cover || !showPicture) return;
    invoke<Record<string, Record<string, string>>>(Invokes.ReadExifForPaths, { paths: [cover] })
      .then((map) => setExif(map[cover] || null))
      .catch(() => setExif(null));
  }, [cover, showPicture]);

  const url = useThumbnail(showPicture ? cover : undefined, requestThumbnails);
  const crumbs = crumbsFor(folder, roots);

  // The folder's photos, starting a little before this one so it drifts into view.
  const neighbours = useMemo(() => {
    if (!files || !showPicture) return null;
    const paths = files.slice(0, SHELF_LENGTH).map((f) => f.path);
    const at = cover ? paths.indexOf(cover) : -1;
    if (at < 0) return paths;
    const start = Math.max(0, at - 2);
    return [...paths.slice(start), ...paths.slice(0, start)];
  }, [files, cover, showPicture]);

  const total = files?.length ?? 0;
  const edited = files?.filter((f) => f.is_edited).length ?? 0;
  const picked = files?.filter((f) => f.flag === 'pick').length ?? 0;
  const rejected = files?.filter((f) => f.flag === 'reject').length ?? 0;

  const fNumber =
    exif?.FNumber && (String(exif.FNumber).toLowerCase().startsWith('f') ? exif.FNumber : `f/${exif.FNumber}`);
  const exposure = [
    exif?.FocalLengthIn35mmFilm || exif?.FocalLength,
    fNumber,
    exif?.ExposureTime,
    (exif?.PhotographicSensitivity || exif?.ISO) && `ISO ${exif?.PhotographicSensitivity || exif?.ISO}`,
  ]
    .filter(Boolean)
    .join('  ');
  const status = [
    info?.flag === 'pick' ? t('library.home.picked') : info?.flag === 'reject' ? t('library.home.rejected') : null,
    info?.is_edited ? t('library.home.edited') : null,
  ]
    .filter(Boolean)
    .join(' · ');
  const rating = info?.rating && info.rating > 0 && (
    <span className="inline-flex gap-0.5 align-[-1px]">
      {Array.from({ length: info.rating }, (_, i) => (
        <Star key={i} size={12} className="text-accent" fill="currentColor" />
      ))}
    </span>
  );
  const showDetails = !!image && showPicture;

  return (
    <div className="relative mx-8 rounded-xl overflow-hidden bg-surface isolate">
      {url && (
        <img
          src={url}
          alt=""
          aria-hidden="true"
          className="absolute inset-0 w-full h-full object-cover blur-2xl scale-110 opacity-50 -z-10"
        />
      )}
      <div className="absolute inset-0 -z-10 bg-gradient-to-r from-bg-secondary/70 via-bg-secondary/85 to-bg-secondary/95" />

      <div
        className="grid p-6 pb-5"
        style={{
          gridTemplateColumns: url ? 'auto minmax(0, 1.4fr) minmax(0, 1fr)' : 'minmax(0, 1.4fr) minmax(0, 1fr)',
        }}
      >
        {url && (
          <button
            className="hidden md:block h-60 shrink-0 rounded-lg overflow-hidden shadow-xl mr-7"
            onClick={() => onOpen(image ? { folder, image } : { folder })}
          >
            <img
              src={url}
              alt={cover ? baseName(cover) : ''}
              className="h-full w-auto max-w-[min(20rem,24vw)] object-cover transition-transform duration-500 hover:scale-[1.02]"
            />
          </button>
        )}

        <div className="flex flex-col min-w-0 pr-7">
          <Text
            variant={TextVariants.small}
            color={TextColors.accent}
            weight={TextWeights.semibold}
            className="uppercase tracking-wider"
          >
            {t('library.home.continueTitle')}
          </Text>
          <Text variant={TextVariants.headline} className="truncate mt-1">
            {image ? baseName(image) : baseName(folder)}
          </Text>
          <div className="flex items-center gap-1 text-sm text-text-secondary min-w-0">
            <FolderOpen size={14} className="shrink-0 mr-0.5" />
            {crumbs.map((c, i) => (
              <span key={c.path} className="flex items-center gap-1 min-w-0">
                {i > 0 && <ChevronRight size={12} className="shrink-0 opacity-60" />}
                <button
                  className="truncate hover:text-accent transition-colors"
                  onClick={() => onOpen({ folder: c.path })}
                >
                  {c.name}
                </button>
              </span>
            ))}
          </div>

          {showDetails && (
            <div className="grid grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)] gap-x-6 mt-4">
              <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 content-start">
                <Spec label={t('library.home.camera')}>{exif?.Model}</Spec>
                <Spec label={t('library.home.lens')}>{exif?.LensModel}</Spec>
                <Spec label={t('library.home.exposure')}>{exposure}</Spec>
              </dl>
              <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 content-start">
                <Spec label={t('library.home.rating')}>{rating}</Spec>
                <Spec label={t('library.home.status')}>{status}</Spec>
                <Spec label={t('library.home.taken')}>{formatDate(exif?.DateTimeOriginal)}</Spec>
              </dl>
            </div>
          )}

          <div className="flex items-center gap-3 mt-auto pt-5">
            {image ? (
              <>
                <Button
                  className="h-12 px-7 text-base rounded-lg shadow-lg shadow-accent/20 whitespace-nowrap shrink-0"
                  onClick={() => onOpen({ folder, image })}
                >
                  <Play size={18} className="mr-2.5" fill="currentColor" />
                  {t('library.home.continueEditing')}
                </Button>
                {/* On narrower windows this shrinks to its icon, leaving room for Continue editing. */}
                <Button
                  className="h-12 px-4 xl:px-5 bg-surface text-text-primary rounded-lg whitespace-nowrap min-w-0"
                  onClick={() => onOpen({ folder })}
                  data-tooltip={t('library.home.openFolder', { name: baseName(folder) })}
                >
                  <Folder size={16} className="xl:mr-2 shrink-0" />
                  <span className="hidden xl:inline truncate">
                    {t('library.home.openFolder', { name: baseName(folder) })}
                  </span>
                </Button>
              </>
            ) : (
              <Button className="h-12 px-7 text-base rounded-lg" onClick={() => onOpen({ folder })}>
                <Folder size={18} className="mr-2.5" />
                {t('library.home.openFolder', { name: baseName(folder) })}
              </Button>
            )}
          </div>
        </div>

        <div className="flex flex-col min-w-0 border-l border-text-secondary/20 pl-7">
          <Text weight={TextWeights.semibold}>{t('library.home.thisFolder')}</Text>
          <Text variant={TextVariants.small} color={TextColors.secondary} className="mb-4">
            {files ? t('library.home.photoCount', { count: total }) : ' '}
          </Text>
          <div className="flex flex-col gap-4">
            <Progress label={t('library.home.editedOf', { done: edited, total })} done={edited} total={total} />
            <Progress
              label={t('library.home.culledOf', { done: picked + rejected, total, picked, rejected })}
              done={picked + rejected}
              total={total}
            />
          </div>
          <div className="flex flex-wrap items-center gap-2 mt-auto pt-5">
            {files && picked + rejected < total ? (
              <Button
                className="h-12 px-4 bg-surface text-text-primary rounded-lg whitespace-nowrap"
                onClick={() => onOpen({ folder, action: 'cull' })}
              >
                <Check size={16} className="mr-2" />
                {t('library.home.cullRest')}
              </Button>
            ) : (
              files &&
              total > 0 && (
                <Text
                  variant={TextVariants.small}
                  color={TextColors.secondary}
                  className="flex items-center gap-1.5 h-12"
                >
                  <Check size={14} /> {t('library.home.allCulled')}
                </Text>
              )
            )}
            {picked > 0 && (
              <Button
                className="h-12 px-4 bg-surface text-text-primary rounded-lg whitespace-nowrap"
                onClick={() => onOpen({ folder, action: 'exportPicks' })}
              >
                <Upload size={16} className="mr-2" />
                {t('library.home.exportPicks')}
              </Button>
            )}
          </div>
        </div>
      </div>

      {neighbours && neighbours.length > 1 && (
        <div className="border-t border-text-secondary/15 py-3">
          <DriftStrip
            photos={neighbours}
            height={56}
            speed={BANNER_DRIFT_SPEED}
            arrows={false}
            highlight={image}
            onOpenPhoto={(p) => onOpen({ folder, image: p })}
            requestThumbnails={requestThumbnails}
          />
        </div>
      )}
    </div>
  );
}

/** Where a level's guide line runs: under the icon of the folder at that level. */
const lineX = (level: number) => PAGE_PAD + level * INDENT + 8;

/**
 * The tree's guide lines to the left of a row: a line down from a parent's
 * icon, lines passing through for the levels above, and an elbow into this
 * folder's own icon. `headerY` is the middle of the row's heading.
 */
function GuideLines({ row, headerY }: { row: FolderRow; headerY: number }) {
  const line = 'absolute border-l border-text-secondary/35 pointer-events-none';
  return (
    <>
      {row.startsLine && (
        <span aria-hidden="true" className={line} style={{ left: lineX(row.depth), top: headerY + 12, bottom: 0 }} />
      )}
      {row.linesContinue.map((continues, k) => {
        const own = k === row.depth - 1;
        // Levels above the parent only pass through; the parent's own line stops here at its last child.
        if (!continues && !own) return null;
        return (
          <span
            key={k}
            aria-hidden="true"
            className={line}
            style={{ left: lineX(k), top: 0, height: continues ? '100%' : headerY }}
          />
        );
      })}
      {row.depth > 0 && (
        <span
          aria-hidden="true"
          className="absolute border-t border-text-secondary/35 pointer-events-none"
          style={{ left: lineX(row.depth - 1), top: headerY, width: INDENT - 12 }}
        />
      )}
    </>
  );
}

function FolderSection({
  row,
  index,
  collapsed,
  onToggle,
  onHide,
  onOpen,
  requestThumbnails,
}: {
  row: FolderRow;
  /** Its place on the page: sets how fast it drifts, and every other row drifts the other way. */
  index: number;
  collapsed: boolean;
  onToggle(): void;
  onHide(): void;
  onOpen(target: HomeTarget): void;
  requestThumbnails(paths: string[]): void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLElement>(null);
  const photos = useFolderPhotos(row.path, ref);
  const left = PAGE_PAD + row.depth * INDENT;
  const top = row.depth === 0;
  const height = top ? 150 : 120;
  const speed = DRIFT_SPEEDS[index % DRIFT_SPEEDS.length] * (index % 2 ? -1 : 1);

  const shown = photos?.slice(0, SHELF_LENGTH).map((f) => f.path) ?? null;
  const empty = photos !== null && photos.length === 0;

  return (
    <section ref={ref} className={`relative ${top ? 'pt-8' : 'pt-1'} ${empty || collapsed ? 'pb-3' : 'pb-5'}`}>
      <GuideLines row={row} headerY={top ? 46 : 16} />
      <div className="flex items-center gap-2 min-w-0 mb-2.5" style={{ paddingLeft: left, paddingRight: PAGE_PAD }}>
        {row.hasChildren ? (
          <button
            className="text-text-secondary hover:text-text-primary transition-colors shrink-0"
            onClick={onToggle}
            aria-label={collapsed ? t('library.home.expand') : t('library.home.collapse')}
            aria-expanded={!collapsed}
          >
            {collapsed ? <Folder size={top ? 18 : 15} /> : <FolderOpen size={top ? 18 : 15} />}
          </button>
        ) : (
          <Folder size={top ? 18 : 15} className="text-text-secondary shrink-0" />
        )}
        {/* Hide from Home only shows while the pointer is on the folder's name. */}
        <div className="group/name flex items-center gap-2 min-w-0">
          <Text
            variant={top ? TextVariants.title : TextVariants.body}
            weight={top ? undefined : TextWeights.semibold}
            className="truncate"
          >
            {row.name}
          </Text>
          <Text variant={TextVariants.small} color={TextColors.secondary} className="whitespace-nowrap">
            {photos === null
              ? ''
              : empty
                ? row.hasChildren
                  ? ''
                  : t('library.home.noPhotos')
                : t('library.home.photoCount', { count: photos.length })}
          </Text>
          {row.location && (
            <Text variant={TextVariants.small} color={TextColors.secondary} className="truncate opacity-70">
              {row.location}
            </Text>
          )}
          {row.hasChildren && (
            <button
              className="text-text-secondary hover:text-text-primary transition-colors shrink-0"
              onClick={onToggle}
              aria-hidden="true"
              tabIndex={-1}
            >
              <ChevronDown size={14} className={`transition-transform ${collapsed ? '-rotate-90' : ''}`} />
            </button>
          )}
          <button
            className="ml-1 flex items-center gap-1 text-xs text-text-secondary hover:text-text-primary whitespace-nowrap opacity-0 group-hover/name:opacity-100 focus-visible:opacity-100 transition-opacity"
            onClick={onHide}
          >
            <EyeOff size={13} />
            {t('library.home.hide')}
          </button>
        </div>
      </div>
      {!empty && !(top && collapsed) && (
        <div className="flex gap-2.5" style={{ paddingLeft: left }}>
          {/* See all stays put at the start of the shelf while the photos drift past it. */}
          <button
            className="shrink-0 rounded-md bg-surface hover:bg-card-active border border-border-color/60 transition-colors flex flex-col items-center justify-center gap-1.5 px-2 text-center group/all"
            style={{ height, width: Math.round(height * 0.8) }}
            onClick={() => onOpen({ folder: row.path })}
          >
            <FolderOpen
              size={top ? 22 : 18}
              className="text-text-secondary group-hover/all:text-accent transition-colors"
            />
            <Text weight={TextWeights.semibold}>{t('library.home.seeAll')}</Text>
            {photos && (
              <Text variant={TextVariants.small} color={TextColors.secondary}>
                {t('library.home.photoCount', { count: photos.length })}
              </Text>
            )}
          </button>
          <DriftStrip
            photos={shown}
            height={height}
            speed={speed}
            fadeLeft
            onOpenPhoto={(p) => onOpen({ folder: row.path, image: p })}
            requestThumbnails={requestThumbnails}
          />
        </div>
      )}
    </section>
  );
}

/** Import: copy photos in from a card or another folder, or add a folder that's already in place. */
function ImportMenu({
  defaultFolder,
  onAddFolder,
  onImportInto,
}: {
  defaultFolder?: string;
  onAddFolder(): void;
  onImportInto(folder: string): void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener('pointerdown', close);
    return () => window.removeEventListener('pointerdown', close);
  }, [open]);

  const importPhotos = async () => {
    setOpen(false);
    const folder = await openDialog({
      directory: true,
      multiple: false,
      defaultPath: defaultFolder,
      title: t('library.home.importWhere'),
    });
    if (typeof folder === 'string') onImportInto(folder);
  };

  const item = 'w-full flex items-start gap-3 rounded-md px-3 py-2.5 text-left hover:bg-card-active transition-colors';
  return (
    <div ref={ref} className="relative">
      <Button className="h-10 px-4" onClick={() => setOpen(!open)}>
        <Download size={16} className="mr-2" />
        {t('library.home.import')}
        <ChevronDown size={14} className="ml-1.5" />
      </Button>
      {open && (
        <div className="absolute right-0 top-12 z-20 w-72 rounded-lg bg-surface p-1.5 shadow-xl border border-border-color">
          <button className={item} onClick={importPhotos}>
            <Download size={16} className="mt-0.5 shrink-0" />
            <span>
              <Text weight={TextWeights.semibold}>{t('library.home.importPhotos')}</Text>
              <Text variant={TextVariants.small} color={TextColors.secondary}>
                {t('library.home.importPhotosDesc')}
              </Text>
            </span>
          </button>
          <button
            className={item}
            onClick={() => {
              setOpen(false);
              onAddFolder();
            }}
          >
            <FolderPlus size={16} className="mt-0.5 shrink-0" />
            <span>
              <Text weight={TextWeights.semibold}>{t('library.home.addFolder')}</Text>
              <Text variant={TextVariants.small} color={TextColors.secondary}>
                {t('library.home.addFolderDesc')}
              </Text>
            </span>
          </button>
        </div>
      )}
    </div>
  );
}

/**
 * The opening screen once there are folders: a banner back to the last photo
 * edited, then the library laid out as a tree, each folder with a drifting
 * shelf of its photos.
 */
export default function HomeScreen({
  appSettings,
  brand,
  footer,
  onAddFolder,
  onImportInto,
  onOpen,
  onOpenSettings,
  onSettingsChange,
  requestThumbnails,
}: HomeScreenProps) {
  const { t } = useTranslation();
  const [showHidden, setShowHidden] = useState(false);
  const hidden: string[] = useMemo(() => appSettings.homeHiddenFolders || [], [appSettings.homeHiddenFolders]);
  const collapsed: string[] = useMemo(() => appSettings.homeCollapsedFolders || [], [appSettings.homeCollapsedFolders]);
  const setHidden = (folders: string[]) => onSettingsChange({ ...appSettings, homeHiddenFolders: folders });
  const toggleCollapsed = (folder: string) =>
    onSettingsChange({
      ...appSettings,
      homeCollapsedFolders: collapsed.includes(folder) ? collapsed.filter((f) => f !== folder) : [...collapsed, folder],
    });
  const roots: string[] = useMemo(
    () =>
      appSettings.rootFolders?.length
        ? appSettings.rootFolders
        : appSettings.lastRootPath
          ? [appSettings.lastRootPath]
          : [],
    [appSettings.rootFolders, appSettings.lastRootPath],
  );
  const [trees, setTrees] = useState<FolderTree[] | null>(null);

  useEffect(() => {
    if (roots.length === 0) return;
    // With the library folders open, the tree comes back two levels deep below them.
    invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
      paths: roots,
      expandedFolders: roots,
      showImageCounts: false,
    })
      .then(setTrees)
      .catch((err) => {
        console.error('Failed to read folders for the home screen:', err);
        setTrees(roots.map((path) => ({ path, name: baseName(path), isDir: true, children: [] })));
      });
  }, [roots]);

  const rows = useMemo(() => (trees ? folderRows(trees, hidden, collapsed) : []), [trees, hidden, collapsed]);

  const folderState = appSettings.lastFolderState;
  const lastImage: string | undefined = folderState?.lastEditedImage || undefined;
  const lastFolder: string | undefined =
    (lastImage && parentOf(lastImage)) ||
    (folderState?.currentFolderPath && !folderState.currentFolderPath.startsWith('Album: ')
      ? folderState.currentFolderPath
      : undefined) ||
    roots[0];

  return (
    <div className="flex-1 flex h-full p-2 bg-transparent min-w-0">
      <div className="w-full h-full bg-bg-secondary rounded-lg border border-border-color/25 overflow-y-auto custom-scrollbar">
        <header className="flex items-center justify-between gap-4 px-8 pt-8 pb-6">
          {brand}
          <div className="flex items-center gap-2">
            <ImportMenu defaultFolder={roots[0]} onAddFolder={onAddFolder} onImportInto={onImportInto} />
            <Button
              className="h-10 px-3 bg-surface text-text-primary"
              onClick={onOpenSettings}
              data-tooltip={t('settings.general.title')}
              variant="ghost"
            >
              <Settings size={18} />
            </Button>
          </div>
        </header>

        {lastFolder && (
          <ContinueBanner
            folder={lastFolder}
            image={lastImage}
            roots={roots}
            showPicture={!hidden.some((h) => isWithin(lastFolder, h))}
            onOpen={onOpen}
            requestThumbnails={requestThumbnails}
          />
        )}

        <div className="pt-4 pb-10">
          {rows.map((row, i) => (
            <FolderSection
              key={row.path}
              row={row}
              index={i}
              collapsed={collapsed.includes(row.path)}
              onToggle={() => toggleCollapsed(row.path)}
              onHide={() => setHidden([...hidden, row.path])}
              onOpen={onOpen}
              requestThumbnails={requestThumbnails}
            />
          ))}
        </div>

        {hidden.length > 0 && (
          <div className="px-8 pb-6">
            <button
              className="text-sm text-text-secondary hover:text-text-primary transition-colors"
              onClick={() => setShowHidden(!showHidden)}
            >
              {t('library.home.hiddenCount', { count: hidden.length })}
            </button>
            {showHidden && (
              <div className="mt-3 flex flex-col gap-2 max-w-md">
                {hidden.map((folder) => (
                  <div key={folder} className="flex items-center justify-between gap-4 rounded-md bg-surface px-3 py-2">
                    <Text className="truncate">{baseName(folder)}</Text>
                    <button
                      className="text-sm text-accent hover:underline shrink-0"
                      onClick={() => setHidden(hidden.filter((h) => h !== folder))}
                    >
                      {t('library.home.unhide')}
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        <div className="px-8 pb-8">{footer}</div>
      </div>
    </div>
  );
}
