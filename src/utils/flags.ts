import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes, ImageFile } from '../components/ui/AppProperties';
import { LIBRARY_REFRESH_EVENT } from './stacks';

export type Flag = 'pick' | 'reject' | null;

/** Pick, reject or clear photos, in the library at once and on disk. */
export function setFlagForPaths(paths: string[], flag: Flag) {
  if (paths.length === 0) return;
  const set = new Set(paths);
  useLibraryStore.getState().setLibrary((s) => ({
    imageList: s.imageList.map((f) => (set.has(f.path) ? { ...f, flag } : f)),
    // A photo rejected here stays in view until you leave the folder, rather than vanishing under the cursor.
    justRejected: flag === 'reject' ? [...s.justRejected, ...paths] : s.justRejected,
  }));
  invoke('set_flag_for_paths', { paths, flag }).catch((e) => toast.error(`Could not flag: ${e}`));
}

/** The photos a key press acts on: the open photo, or the library selection. */
export function flagTargets(): string[] {
  const { selectedImage } = useEditorStore.getState();
  if (selectedImage) return [selectedImage.path];
  const { multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
  if (multiSelectedPaths.length > 0) return multiSelectedPaths;
  return libraryActivePath ? [libraryActivePath] : [];
}

const parentOf = (path: string) => path.split('?vc=')[0].replace(/[\\/][^\\/]*$/, '');
const nameOf = (path: string) => path.split(/[\\/]/).pop() || path;

/** A folder of rejects: "Tahiti rejects", "Rejects", "Rejected". */
export const isRejectsFolder = (path: string | null | undefined) =>
  !!path && /(^|\s)rejects$|^rejected$/i.test(nameOf(path));

/** Where a folder's rejects go: a subfolder named after it, "Tahiti/Tahiti rejects". */
export const rejectsFolderFor = (folder: string) => {
  const sep = folder.includes('\\') && !folder.includes('/') ? '\\' : '/';
  return `${folder}${sep}${nameOf(folder)} rejects`;
};

/**
 * The rejected photos that can be moved into a rejects folder. A virtual
 * copy is part of its photo's files, so it moves with the photo rather than
 * on its own, and a photo is left where it is while any copy of it is still
 * a keeper.
 */
export function movableRejects(imageList: ImageFile[]): string[] {
  const keptCopies = new Set(
    imageList.filter((f) => f.path.includes('?vc=') && f.flag !== 'reject').map((f) => f.path.split('?vc=')[0]),
  );
  return imageList
    .filter((f) => f.flag === 'reject' && !f.path.includes('?vc=') && !keptCopies.has(f.path))
    .filter((f) => !isRejectsFolder(parentOf(f.path)))
    .map((f) => f.path);
}

/**
 * Move rejected photos, with their sidecars and copies, into a rejects
 * subfolder of the folder each one is in, creating it if needed. Each folder's
 * move is one step that ⌘Z undoes.
 */
export async function moveRejectsToSubfolders(paths: string[]) {
  const byFolder = new Map<string, string[]>();
  for (const p of paths) {
    const folder = parentOf(p);
    byFolder.set(folder, [...(byFolder.get(folder) || []), p]);
  }
  let moved = 0;
  const into: string[] = [];
  try {
    for (const [folder, files] of byFolder) {
      const target = rejectsFolderFor(folder);
      await invoke(Invokes.CreateFolder, { path: target }).catch((e) => {
        if (!String(e).includes('already exists')) throw e;
      });
      await invoke(Invokes.MoveFiles, { sourcePaths: files, destinationFolder: target });
      moved += files.length;
      into.push(nameOf(target));
    }
    useLibraryStore.getState().setLibrary({ multiSelectedPaths: [], libraryActivePath: null });
    toast.success(
      `Moved ${moved} rejected photo${moved === 1 ? '' : 's'} to ${into.length === 1 ? `"${into[0]}"` : `${into.length} rejects folders`} — ⌘Z to undo`,
    );
  } catch (e) {
    toast.error(`Could not move rejects${moved ? ` (${moved} moved)` : ''}: ${e}`);
  }
  window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
}
