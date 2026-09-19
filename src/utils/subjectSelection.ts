import type { Coord } from './adjustments';

export type SubjectPoint = Coord & { label: 0 | 1 | 2 | 3 };

/** Boxes establish a new subject; subsequent points refine it. Legacy masks
 * retain their original click/box as the first prompt when edited. */
export function subjectPrompts(parameters: any, start: Coord, end: Coord, exclude: boolean): SubjectPoint[] {
  const isPoint = Math.abs(start.x - end.x) < 1e-6 && Math.abs(start.y - end.y) < 1e-6;
  if (!isPoint && !exclude) {
    return [
      { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y), label: 2 },
      { x: Math.max(start.x, end.x), y: Math.max(start.y, end.y), label: 3 },
    ];
  }
  let previous: SubjectPoint[] = parameters.subjectPoints ?? [];
  if (!parameters.subjectPoints && parameters.maskDataBase64 && Number.isFinite(parameters.startX)) {
    previous = subjectPrompts(
      {},
      { x: parameters.startX, y: parameters.startY },
      { x: parameters.endX, y: parameters.endY },
      false,
    );
  }
  if (exclude && !previous.some((p) => p.label === 1 || p.label === 2)) {
    throw new Error('Include part of the subject before excluding the background.');
  }
  if (previous.length >= 256)
    throw new Error('This selection has reached 256 points. Start a new selection to continue.');
  return [...previous, { ...start, label: exclude ? 0 : 1 }];
}
