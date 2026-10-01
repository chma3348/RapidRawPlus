import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { LIBRARY_REFRESH_EVENT } from './stacks';

/**
 * Undo the last file operation (move, rename, copy, delete, folder
 * change) — ⌘Z in the library. The backend journal knows exactly which
 * files went where; see journal.rs.
 */
export async function undoFileOperation() {
  try {
    const outcome = await invoke<{ label: string; problems: string[] }>('undo_file_operation');
    if (outcome.problems.length === 0) {
      toast.success(`Undone: ${outcome.label}`);
    } else {
      toast.warn(`Undone, with problems: ${outcome.label}\n${outcome.problems.join('\n')}`);
    }
    window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
  } catch (err) {
    toast.info(String(err) === 'Nothing to undo' ? 'Nothing to undo' : `Could not undo: ${err}`);
  }
}
