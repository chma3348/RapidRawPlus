import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';

export type Flag = 'pick' | 'reject' | null;

/** Pick, reject or clear photos, in the library at once and on disk. */
export function setFlagForPaths(paths: string[], flag: Flag) {
  if (paths.length === 0) return;
  const set = new Set(paths);
  useLibraryStore.getState().setLibrary((s) => ({
    imageList: s.imageList.map((f) => (set.has(f.path) ? { ...f, flag } : f)),
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
