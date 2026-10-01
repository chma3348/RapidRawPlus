import { create } from 'zustand';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { AppSettings } from '../components/ui/AppProperties';
import { movableRejects, moveRejectsToSubfolders } from './flags';

/**
 * What happens to rejected photos when you finish culling: ask, move them into
 * the folder's rejects subfolder, or keep them hidden where they are. There is
 * one choice for all folders, and any folder can have its own.
 */
export type RejectsHandling = 'ask' | 'move' | 'keep';

export const parentOf = (path: string) => path.split('?vc=')[0].replace(/[\\/][^\\/]*$/, '');

/** The choice for one folder: its own if it has one, otherwise the one for all folders. */
export const rejectsHandlingFor = (settings: AppSettings | null | undefined, folder: string): RejectsHandling =>
  settings?.rejectsFolderChoices?.[folder] ?? settings?.rejectsAfterCull ?? 'ask';

interface RejectsPromptState {
  /** Rejects waiting on a decision, or null when nothing is being asked. */
  paths: string[] | null;
  ask: (paths: string[]) => void;
  close: () => void;
}

export const useRejectsPrompt = create<RejectsPromptState>((set) => ({
  paths: null,
  ask: (paths) => set({ paths }),
  close: () => set({ paths: null }),
}));

/**
 * Called when a cull is finished. Rejects in folders set to move go to their
 * rejects subfolder straight away, those set to keep stay hidden, and the rest
 * are asked about.
 */
export function afterCulling(culledPaths: string[]) {
  const culled = new Set(culledPaths);
  const rejects = movableRejects(useLibraryStore.getState().imageList).filter((p) => culled.has(p));
  if (rejects.length === 0) return;
  const settings = useSettingsStore.getState().appSettings;
  const toMove = rejects.filter((p) => rejectsHandlingFor(settings, parentOf(p)) === 'move');
  const toAsk = rejects.filter((p) => rejectsHandlingFor(settings, parentOf(p)) === 'ask');
  if (toMove.length > 0) moveRejectsToSubfolders(toMove);
  if (toAsk.length > 0) useRejectsPrompt.getState().ask(toAsk);
}

/**
 * Remember a choice for these folders, or for all folders. Choosing for all
 * folders clears these folders' own choices so the new one applies to them too.
 */
export function rememberRejectsHandling(choice: RejectsHandling, scope: 'folders' | 'all', folders: string[] = []) {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings) return;
  const own = { ...(appSettings.rejectsFolderChoices || {}) };
  if (scope === 'all') {
    folders.forEach((f) => delete own[f]);
    handleSettingsChange({ ...appSettings, rejectsAfterCull: choice, rejectsFolderChoices: own });
  } else {
    // A folder set to the same as all folders needs no choice of its own.
    const general = appSettings.rejectsAfterCull ?? 'ask';
    folders.forEach((f) => {
      if (choice === general) delete own[f];
      else own[f] = choice;
    });
    handleSettingsChange({ ...appSettings, rejectsFolderChoices: own });
  }
}
