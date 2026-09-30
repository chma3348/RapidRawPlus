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

/** How many files the stack headed by `path` holds (0 when it has none). */
export function useStackSize(path: string): number {
  return useLibraryStore((state) => getStacks(state.imageList).members.get(path)?.length ?? 0);
}
