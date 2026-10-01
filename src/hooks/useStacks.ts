import { useLibraryStore } from '../store/useLibraryStore';
import { buildStacks, Stacks } from '../utils/stacks';
import type { ImageFile } from '../components/ui/AppProperties';

// Built once per listing and shared: every tile asks for its stack size,
// and the library sort needs the same grouping.
let cached: { list: ImageFile[]; stacks: Stacks } | null = null;

export function getStacks(list: ImageFile[]): Stacks {
  if (cached?.list !== list) cached = { list, stacks: buildStacks(list) };
  return cached.stacks;
}

/** The current folder's version stacks; see utils/stacks. */
export function useStacks(): Stacks {
  return getStacks(useLibraryStore((state) => state.imageList));
}

/** `paths` plus the hidden versions of any stacks among them, so moving
 *  or copying a photo's tile takes its restores and copies along. */
export function withVersions(paths: string[]): string[] {
  const stacks = getStacks(useLibraryStore.getState().imageList);
  return Array.from(new Set(paths.flatMap((p) => (stacks.members.get(p) ?? [{ path: p }]).map((f) => f.path))));
}

/** How many files the stack headed by `path` holds (0 when it has none). */
export function useStackSize(path: string): number {
  return useLibraryStore((state) => getStacks(state.imageList).members.get(path)?.length ?? 0);
}
