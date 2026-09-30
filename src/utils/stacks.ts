import type { ImageFile } from '../components/ui/AppProperties';

/**
 * Version stacks: a photo and the files made from it, shown as one.
 *
 * Restores, upscales, denoised and expanded copies, negative conversions
 * and virtual copies all stay separate files (or sidecars) on disk, but
 * the library shows one tile per photo, the original, and the editor
 * lists its versions underneath. The backend says which file each version
 * came from (`derived_from`, see versions.rs); a version made from a
 * version joins the same stack.
 *
 * Frames saved from a video are not stacked: they stay pictures of their
 * own, placed straight after their clip whatever the sort order.
 */
export interface Stacks {
  /** Version path → the stack's original. Originals are not listed. */
  rootOf: Map<string, string>;
  /** Original path → the whole stack in display order, original first.
   *  Only photos that have versions. */
  members: Map<string, ImageFile[]>;
  /** Saved frame path → its clip. */
  frameSource: Map<string, string>;
}

export const FRAME_KIND = 'Frame';

/** Dispatched on `window` by views that write a file into the open folder
 *  (a saved video frame) so the library lists it without a manual refresh. */
export const LIBRARY_REFRESH_EVENT = 'rapidraw:library-refresh';

const realPath = (path: string) => path.split('?vc=')[0];
const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

export function buildStacks(list: ImageFile[]): Stacks {
  const byPath = new Map<string, ImageFile>();
  for (const f of list) byPath.set(f.path, f);

  const frameSource = new Map<string, string>();
  const parentOf = (f: ImageFile): string | null => {
    if (f.is_virtual_copy || f.path.includes('?vc=')) {
      // A virtual copy belongs with its file, which may itself be a version.
      const real = realPath(f.path);
      return byPath.has(real) ? real : null;
    }
    if (f.derived_from && f.derived_kind !== FRAME_KIND && byPath.has(f.derived_from)) {
      return f.derived_from;
    }
    return null;
  };

  for (const f of list) {
    if (f.derived_kind === FRAME_KIND && f.derived_from && byPath.has(f.derived_from)) {
      frameSource.set(f.path, f.derived_from);
    }
  }

  const rootOf = new Map<string, string>();
  const members = new Map<string, ImageFile[]>();
  for (const f of list) {
    let root = f.path;
    const seen = new Set([root]);
    for (;;) {
      const next = parentOf(byPath.get(root)!);
      if (!next || seen.has(next)) break;
      seen.add(next);
      root = next;
    }
    if (root === f.path) continue;
    rootOf.set(f.path, root);
    if (!members.has(root)) members.set(root, [byPath.get(root)!]);
    members.get(root)!.push(f);
  }

  // Original first, then its versions and copies by name, each copy right
  // after the file it copies (plain comparison keeps `?vc=` after its base).
  for (const [root, group] of members) {
    const [original, ...rest] = group;
    rest.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
    members.set(root, [original, ...rest]);
  }

  return { rootOf, members, frameSource };
}

export const stackRootOf = (stacks: Stacks, path: string | null | undefined) =>
  path ? (stacks.rootOf.get(path) ?? path) : path;

/** Short labels for a stack's members: Original, Restored, Restored 2,
 *  Copy 1… */
export function versionLabels(group: ImageFile[]): Map<string, string> {
  const labels = new Map<string, string>();
  let copies = 0;
  group.forEach((f, i) => {
    if (i === 0) {
      labels.set(f.path, 'Original');
    } else if (f.path.includes('?vc=')) {
      copies += 1;
      labels.set(f.path, `Copy ${copies}`);
    } else {
      const stem = fileName(f.path).replace(/\.[^.]+$/, '');
      const clash = stem.match(/_(\d+)$/);
      const kind = f.derived_kind || 'Version';
      labels.set(f.path, clash && !stem.endsWith(`_${kind}`) ? `${kind} ${clash[1]}` : kind);
    }
  });
  return labels;
}
