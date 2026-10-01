import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview, type DragDropEvent } from '@tauri-apps/api/webview';
import type { Event as TauriEvent } from '@tauri-apps/api/event';
import { toast } from 'react-toastify';
import { Invokes } from '../components/ui/AppProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { withVersions } from '../hooks/useStacks';
import { LIBRARY_REFRESH_EVENT } from './stacks';

/**
 * Dragging photos onto folders and albums, and files in from Finder.
 *
 * Built on pointer events rather than HTML drag and drop, which Tauri's
 * own file-drop handling interferes with: a drag starts once the pointer
 * moves a few pixels from a thumbnail, rows in the folder tree mark
 * themselves with `data-drop-folder` / `data-drop-album`, and the row under
 * the pointer is the target. Dropping on a folder moves the photos there
 * (⌥ copies); dropping on an album adds them to it. Files dragged in from
 * Finder are copied into the folder they land on (or the open folder), or
 * added to an album where they are.
 */
export type DropTarget = { kind: 'folder'; path: string; name: string } | { kind: 'album'; id: string; name: string };

interface FileDragState {
  source: 'library' | 'finder' | null;
  /** Everything that moves, including stacks' hidden versions. */
  paths: string[];
  /** How many photos the person picked up, for the ghost's count. */
  count: number;
  x: number;
  y: number;
  copy: boolean;
  target: DropTarget | null;
}

const IDLE: FileDragState = { source: null, paths: [], count: 0, x: 0, y: 0, copy: false, target: null };

export const useFileDragStore = create<FileDragState>(() => IDLE);

const nameOf = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() || path;
const parentOf = (path: string) => path.split('?vc=')[0].replace(/[\\/][^\\/]*$/, '');

/** The folder or album row under a point, if any. */
function targetAt(x: number, y: number): DropTarget | null {
  const el = document.elementFromPoint(x, y)?.closest('[data-drop-folder],[data-drop-album]') as HTMLElement | null;
  if (!el) return null;
  if (el.dataset.dropFolder) {
    return { kind: 'folder', path: el.dataset.dropFolder, name: nameOf(el.dataset.dropFolder) };
  }
  if (el.dataset.dropAlbum) {
    return { kind: 'album', id: el.dataset.dropAlbum, name: el.dataset.dropName || 'album' };
  }
  return null;
}

/** A library drag onto the folder the photos are already in is no move. */
function usableTarget(target: DropTarget | null, paths: string[]): DropTarget | null {
  if (target?.kind === 'folder' && paths.length > 0 && paths.every((p) => parentOf(p) === target.path)) {
    return null;
  }
  return target;
}

function refreshAfterDrop() {
  window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
  invoke(Invokes.GetAlbums)
    .then((tree: any) => useLibraryStore.getState().setLibrary({ albumTree: tree }))
    .catch(() => {});
}

const photos = (n: number) => `${n} photo${n === 1 ? '' : 's'}`;

async function dropLibrary(state: FileDragState) {
  const { target, paths, count, copy } = state;
  if (!target) return;
  try {
    if (target.kind === 'album') {
      await invoke(Invokes.AddToAlbum, { albumId: target.id, paths });
      toast.success(`Added ${photos(count)} to ${target.name}`);
    } else if (copy) {
      await invoke(Invokes.CopyFiles, { sourcePaths: paths, destinationFolder: target.path });
      toast.success(`Copied ${photos(count)} to ${target.name} — ⌘Z to undo`);
    } else {
      await invoke(Invokes.MoveFiles, { sourcePaths: paths, destinationFolder: target.path });
      useLibraryStore.getState().setLibrary({ multiSelectedPaths: [] });
      toast.success(`Moved ${photos(count)} to ${target.name} — ⌘Z to undo`);
    }
  } catch (err) {
    toast.error(`Could not ${target.kind === 'album' ? 'add to album' : copy ? 'copy' : 'move'}: ${err}`);
  }
  refreshAfterDrop();
}

/**
 * Called on pointer-down on a thumbnail. Becomes a drag once the pointer
 * moves; a plain click is left alone.
 */
export function beginFileDrag(e: React.PointerEvent, path: string) {
  if (e.button !== 0 || e.pointerType === 'touch') return;
  const startX = e.clientX;
  const startY = e.clientY;
  let dragging = false;
  let previousUserSelect = '';

  const stop = () => {
    window.removeEventListener('pointermove', onMove);
    window.removeEventListener('pointerup', onUp);
    window.removeEventListener('keydown', onKey, true);
    if (dragging) document.body.style.userSelect = previousUserSelect;
  };
  const onMove = (ev: PointerEvent) => {
    if (!dragging) {
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < 6) return;
      dragging = true;
      previousUserSelect = document.body.style.userSelect;
      document.body.style.userSelect = 'none';
      const { multiSelectedPaths } = useLibraryStore.getState();
      const picked = multiSelectedPaths.includes(path) ? multiSelectedPaths : [path];
      useFileDragStore.setState({ ...IDLE, source: 'library', paths: withVersions(picked), count: picked.length });
    }
    const { paths } = useFileDragStore.getState();
    useFileDragStore.setState({
      x: ev.clientX,
      y: ev.clientY,
      copy: ev.altKey,
      target: usableTarget(targetAt(ev.clientX, ev.clientY), paths),
    });
  };
  const onUp = () => {
    stop();
    if (!dragging) return;
    const state = useFileDragStore.getState();
    useFileDragStore.setState(IDLE);
    void dropLibrary(state);
  };
  const onKey = (ev: KeyboardEvent) => {
    if (ev.key === 'Escape' && dragging) {
      ev.stopPropagation();
      stop();
      useFileDragStore.setState(IDLE);
    } else if (ev.key === 'Alt' && dragging) {
      useFileDragStore.setState({ copy: true });
    }
  };
  window.addEventListener('pointermove', onMove);
  window.addEventListener('pointerup', onUp);
  window.addEventListener('keydown', onKey, true);
}

/** Files from Finder: keep the ones the library can show. */
function mediaPaths(paths: string[]): string[] {
  const types = useSettingsStore.getState().supportedTypes as { raw?: string[]; nonRaw?: string[] } | null;
  const known = new Set([...(types?.raw ?? []), ...(types?.nonRaw ?? []), 'mov', 'mp4', 'm4v']);
  return paths.filter((p) => {
    const ext = p.split('.').pop()?.toLowerCase() ?? '';
    return known.size === 0 || known.has(ext);
  });
}

async function dropFromFinder(paths: string[], target: DropTarget | null) {
  const files = mediaPaths(paths);
  if (files.length === 0) {
    toast.info('Only photos and videos can be dropped here');
    return;
  }
  const { currentFolderPath } = useLibraryStore.getState();
  const openFolder =
    currentFolderPath && !currentFolderPath.startsWith('Album: ')
      ? { kind: 'folder' as const, path: currentFolderPath, name: nameOf(currentFolderPath) }
      : null;
  const where = target ?? openFolder;
  if (!where) {
    toast.info('Drop onto a folder or album');
    return;
  }
  try {
    if (where.kind === 'album') {
      // Albums point at files where they are; nothing is copied.
      await invoke(Invokes.AddToAlbum, { albumId: where.id, paths: files });
      toast.success(`Added ${photos(files.length)} to ${where.name}`);
    } else {
      await invoke(Invokes.CopyFiles, { sourcePaths: files, destinationFolder: where.path });
      toast.success(`Copied ${photos(files.length)} into ${where.name} — ⌘Z to undo`);
    }
  } catch (err) {
    toast.error(`Could not add the files: ${err}`);
  }
  refreshAfterDrop();
}

/** Listen for files dragged in from Finder. Returns the unlisten call. */
export function listenForFinderDrops(): () => void {
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  let pending: string[] = [];
  const toCss = (p: { x: number; y: number }) => ({
    x: p.x / (window.devicePixelRatio || 1),
    y: p.y / (window.devicePixelRatio || 1),
  });
  getCurrentWebview()
    .onDragDropEvent((event: TauriEvent<DragDropEvent>) => {
      const payload = event.payload;
      if (payload.type === 'enter' || payload.type === 'over') {
        if (payload.type === 'enter') pending = mediaPaths(payload.paths);
        const { x, y } = toCss(payload.position);
        useFileDragStore.setState({
          source: 'finder',
          paths: pending,
          count: pending.length,
          x,
          y,
          copy: true,
          target: targetAt(x, y),
        });
      } else if (payload.type === 'drop') {
        const { x, y } = toCss(payload.position);
        const target = targetAt(x, y);
        useFileDragStore.setState(IDLE);
        void dropFromFinder(payload.paths, target);
      } else {
        useFileDragStore.setState(IDLE);
      }
    })
    .then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    })
    .catch((err) => console.warn('File drops from Finder are unavailable:', err));
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

/** What a drop would do, for the ghost's caption. */
export function dropCaption(state: FileDragState): string {
  const { target, source, copy, count } = state;
  const what = photos(count);
  if (!target) {
    if (source === 'finder') {
      const folder = useLibraryStore.getState().currentFolderPath;
      return folder && !folder.startsWith('Album: ') ? `Copy ${what} into ${nameOf(folder)}` : what;
    }
    return what;
  }
  if (target.kind === 'album') return `Add ${what} to ${target.name}`;
  return `${copy ? 'Copy' : 'Move'} ${what} to ${target.name}`;
}
