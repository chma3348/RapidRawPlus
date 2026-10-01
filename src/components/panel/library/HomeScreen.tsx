import { ReactNode, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, ChevronRight, EyeOff, Folder, Play, Settings } from 'lucide-react';

import Button from '../../ui/Button';
import Text from '../../ui/Text';
import { AppSettings, ImageFile, Invokes } from '../../ui/AppProperties';
import { FolderTree } from '../FolderTree';
import { useProcessStore } from '../../../store/useProcessStore';
import { TextColors, TextVariants, TextWeights } from '../../../types/typography';
import { isVideoPath } from '../../../utils/media';

/** How many photos a shelf shows before "See all". */
const SHELF_LENGTH = 40;

const realPath = (path: string) => path.split('?vc=')[0];
const baseName = (path: string) => realPath(path).split(/[\\/]/).pop() || path;
const parentOf = (path: string) => realPath(path).replace(/[\\/][^\\/]*$/, '');
const isWithin = (path: string, folder: string) =>
  path === folder || path.startsWith(folder + '/') || path.startsWith(folder + '\\');

export interface HomeTarget {
  folder?: string;
  image?: string;
}

interface HomeScreenProps {
  appSettings: AppSettings;
  brand: ReactNode;
  footer: ReactNode;
  onAddFolder(): void;
  onOpen(target: HomeTarget): void;
  onOpenSettings(): void;
  onSettingsChange(settings: AppSettings): void;
  requestThumbnails(paths: string[]): void;
}

interface ShelfInfo {
  path: string;
  name: string;
  parentName?: string;
}

/** A folder that is both a library root and inside another gets one shelf. */
const dedupe = (shelves: ShelfInfo[]) => {
  const seen = new Set<string>();
  return shelves.filter((s) => !seen.has(s.path) && !!seen.add(s.path));
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

function ContinueBanner({
  folder,
  image,
  showPicture,
  onOpen,
  requestThumbnails,
}: {
  folder: string;
  image?: string;
  /** Off when the folder is hidden from Home: the banner then shows no photo. */
  showPicture: boolean;
  onOpen(target: HomeTarget): void;
  requestThumbnails(paths: string[]): void;
}) {
  const { t } = useTranslation();
  // Without a remembered photo, the folder's newest one stands in as the picture.
  const [cover, setCover] = useState<string | undefined>(image);
  useEffect(() => {
    if (!showPicture) {
      setCover(undefined);
      return;
    }
    if (image) {
      setCover(image);
      return;
    }
    invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: folder })
      .then((files) => setCover(shelfPhotos(files).find((f) => !isVideoPath(f.path))?.path))
      .catch(() => setCover(undefined));
  }, [folder, image, showPicture]);
  const url = useThumbnail(cover, requestThumbnails);
  const folderName = baseName(folder);

  return (
    <div className="relative mx-8 h-56 rounded-xl overflow-hidden bg-surface isolate">
      {url && (
        <img
          src={url}
          alt=""
          aria-hidden="true"
          className="absolute inset-0 w-full h-full object-cover blur-2xl scale-110 opacity-60 -z-10"
        />
      )}
      <div className="absolute inset-0 -z-10 bg-gradient-to-r from-bg-secondary via-bg-secondary/80 to-bg-secondary/20" />
      <div className="h-full flex items-center justify-between gap-8 p-8">
        <div className="min-w-0">
          <Text
            variant={TextVariants.small}
            color={TextColors.accent}
            weight={TextWeights.semibold}
            className="uppercase tracking-wider mb-2"
          >
            {t('library.home.continueTitle')}
          </Text>
          <Text variant={TextVariants.headline} className="truncate">
            {image ? baseName(image) : folderName}
          </Text>
          {image && (
            <Text color={TextColors.secondary} className="truncate">
              {t('library.home.inFolder', { name: folderName })}
            </Text>
          )}
          <div className="flex gap-3 mt-6">
            {image ? (
              <>
                <Button className="h-10 px-5" onClick={() => onOpen({ folder, image })}>
                  <Play size={16} className="mr-2" />
                  {t('library.home.continueEditing')}
                </Button>
                <Button className="h-10 px-5 bg-surface text-text-primary" onClick={() => onOpen({ folder })}>
                  <Folder size={16} className="mr-2" />
                  {t('library.home.openFolder', { name: folderName })}
                </Button>
              </>
            ) : (
              <Button className="h-10 px-5" onClick={() => onOpen({ folder })}>
                <Folder size={16} className="mr-2" />
                {t('library.home.openFolder', { name: folderName })}
              </Button>
            )}
          </div>
        </div>
        {url && (
          <button
            className="hidden md:block h-full shrink-0 rounded-lg overflow-hidden shadow-xl"
            onClick={() => onOpen(image ? { folder, image } : { folder })}
          >
            <img src={url} alt={image ? baseName(image) : folderName} className="h-full w-auto object-cover" />
          </button>
        )}
      </div>
    </div>
  );
}

function ShelfPhoto({
  path,
  onOpen,
  requestThumbnails,
}: {
  path: string;
  onOpen(): void;
  requestThumbnails(paths: string[]): void;
}) {
  const url = useThumbnail(path, requestThumbnails);
  const video = isVideoPath(path);
  return (
    <button
      className="relative h-40 shrink-0 rounded-md overflow-hidden bg-surface snap-start group/photo focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
      onClick={onOpen}
      data-tooltip={baseName(path)}
    >
      {url ? (
        <img
          src={url}
          alt={baseName(path)}
          draggable={false}
          className="h-full w-auto max-w-[24rem] object-cover transition-transform duration-300 group-hover/photo:scale-[1.03]"
        />
      ) : (
        <div className="h-full w-56 animate-pulse" />
      )}
      {video && (
        <span className="absolute bottom-2 left-2 rounded-full bg-black/60 p-1.5">
          <Play size={12} className="text-white" fill="white" />
        </span>
      )}
    </button>
  );
}

function Shelf({
  shelf,
  onHide,
  onOpen,
  requestThumbnails,
}: {
  shelf: ShelfInfo;
  onHide(): void;
  onOpen(target: HomeTarget): void;
  requestThumbnails(paths: string[]): void;
}) {
  const { t } = useTranslation();
  const sectionRef = useRef<HTMLElement>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [photos, setPhotos] = useState<ImageFile[] | null>(null);
  const [canScroll, setCanScroll] = useState({ left: false, right: false });

  // A folder is only read once its shelf comes near the screen.
  useEffect(() => {
    const el = sectionRef.current;
    if (!el || photos) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        observer.disconnect();
        invoke<ImageFile[]>(Invokes.ListImagesInDir, { path: shelf.path })
          .then((files) => setPhotos(shelfPhotos(files)))
          .catch(() => setPhotos([]));
      },
      { rootMargin: '400px 0px' },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [shelf.path, photos]);

  const updateArrows = () => {
    const el = scrollerRef.current;
    if (!el) return;
    setCanScroll({ left: el.scrollLeft > 4, right: el.scrollLeft + el.clientWidth < el.scrollWidth - 4 });
  };
  useEffect(updateArrows, [photos]);

  const scrollBy = (direction: number) => {
    const el = scrollerRef.current;
    if (el) el.scrollBy({ left: direction * el.clientWidth * 0.8, behavior: 'smooth' });
  };

  if (photos && photos.length === 0) return null;
  const shown = photos?.slice(0, SHELF_LENGTH);

  return (
    <section ref={sectionRef} className="group/shelf">
      <div className="flex items-end justify-between gap-4 px-8 mb-3">
        <div className="min-w-0">
          <Text variant={TextVariants.title} className="truncate">
            {shelf.name}
          </Text>
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {photos ? t('library.home.photoCount', { count: photos.length }) : ' '}
            {shelf.parentName && ` · ${t('library.home.inFolder', { name: shelf.parentName })}`}
          </Text>
        </div>
        <div className="flex items-center gap-4 shrink-0">
          <button
            className="flex items-center gap-1 text-sm text-text-secondary hover:text-text-primary opacity-0 group-hover/shelf:opacity-100 transition-opacity"
            onClick={onHide}
          >
            <EyeOff size={14} />
            {t('library.home.hide')}
          </button>
          <button
            className="flex items-center gap-1 text-sm text-text-secondary hover:text-accent transition-colors"
            onClick={() => onOpen({ folder: shelf.path })}
          >
            {t('library.home.seeAll')}
            <ChevronRight size={16} />
          </button>
        </div>
      </div>
      <div className="relative">
        <div
          ref={scrollerRef}
          onScroll={updateArrows}
          className="flex gap-3 overflow-x-auto px-8 pb-1 snap-x scroll-px-8 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        >
          {shown
            ? shown.map((f) => (
                <ShelfPhoto
                  key={f.path}
                  path={f.path}
                  onOpen={() => onOpen({ folder: shelf.path, image: f.path })}
                  requestThumbnails={requestThumbnails}
                />
              ))
            : Array.from({ length: 6 }, (_, i) => (
                <div key={i} className="h-40 w-56 shrink-0 rounded-md bg-surface animate-pulse" />
              ))}
          {photos && photos.length > SHELF_LENGTH && (
            <button
              className="h-40 w-40 shrink-0 rounded-md bg-surface hover:bg-card-active transition-colors flex flex-col items-center justify-center gap-1 snap-start"
              onClick={() => onOpen({ folder: shelf.path })}
            >
              <Text weight={TextWeights.semibold}>{t('library.home.seeAll')}</Text>
              <Text variant={TextVariants.small} color={TextColors.secondary}>
                {t('library.home.photoCount', { count: photos.length })}
              </Text>
            </button>
          )}
        </div>
        {canScroll.left && (
          <button
            className="absolute left-2 top-1/2 -translate-y-1/2 rounded-full bg-bg-primary/80 p-2 shadow-lg opacity-0 group-hover/shelf:opacity-100 transition-opacity"
            onClick={() => scrollBy(-1)}
            aria-label={t('library.home.scrollBack')}
          >
            <ChevronLeft size={20} />
          </button>
        )}
        {canScroll.right && (
          <button
            className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full bg-bg-primary/80 p-2 shadow-lg opacity-0 group-hover/shelf:opacity-100 transition-opacity"
            onClick={() => scrollBy(1)}
            aria-label={t('library.home.scrollForward')}
          >
            <ChevronRight size={20} />
          </button>
        )}
      </div>
    </section>
  );
}

/**
 * The opening screen once there are folders: a banner back to the last photo
 * edited, then a scrolling shelf for each library folder and the folders
 * directly inside it.
 */
export default function HomeScreen({
  appSettings,
  brand,
  footer,
  onAddFolder,
  onOpen,
  onOpenSettings,
  onSettingsChange,
  requestThumbnails,
}: HomeScreenProps) {
  const { t } = useTranslation();
  const [showHidden, setShowHidden] = useState(false);
  const hidden: string[] = useMemo(() => appSettings.homeHiddenFolders || [], [appSettings.homeHiddenFolders]);
  const setHidden = (folders: string[]) => onSettingsChange({ ...appSettings, homeHiddenFolders: folders });
  const roots: string[] = useMemo(
    () =>
      appSettings.rootFolders?.length
        ? appSettings.rootFolders
        : appSettings.lastRootPath
          ? [appSettings.lastRootPath]
          : [],
    [appSettings.rootFolders, appSettings.lastRootPath],
  );
  const [shelves, setShelves] = useState<ShelfInfo[] | null>(null);

  useEffect(() => {
    if (roots.length === 0) return;
    invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
      paths: roots,
      expandedFolders: roots,
      showImageCounts: false,
    })
      .then((trees) =>
        setShelves(
          dedupe(
            trees.flatMap((tree) => [
              { path: tree.path, name: tree.name },
              ...[...(tree.children || [])]
                .filter((c) => c.isDir)
                .sort((a, b) => (b.modified || 0) - (a.modified || 0))
                .map((c) => ({ path: c.path, name: c.name, parentName: tree.name })),
            ]),
          ),
        ),
      )
      .catch((err) => {
        console.error('Failed to read folders for the home screen:', err);
        setShelves(roots.map((path) => ({ path, name: baseName(path) })));
      });
  }, [roots]);

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
            <Button className="h-10 px-4 bg-surface text-text-primary" onClick={onAddFolder}>
              <Folder size={16} className="mr-2" />
              {t('library.splash.addFolder')}
            </Button>
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
            showPicture={!hidden.some((h) => isWithin(lastFolder, h))}
            onOpen={onOpen}
            requestThumbnails={requestThumbnails}
          />
        )}

        <div className="flex flex-col gap-10 py-10">
          {(shelves || [])
            .filter((shelf) => !hidden.some((h) => isWithin(shelf.path, h)))
            .map((shelf) => (
              <Shelf
                key={shelf.path}
                shelf={shelf}
                onHide={() => setHidden([...hidden, shelf.path])}
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
