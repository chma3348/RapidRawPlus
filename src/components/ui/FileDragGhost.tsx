import { Copy, FolderInput, Images } from 'lucide-react';
import { useProcessStore } from '../../store/useProcessStore';
import { dropCaption, useFileDragStore } from '../../utils/fileDrag';

/** What follows the pointer while photos are dragged: the first photo,
 *  how many, and what letting go will do. */
export default function FileDragGhost() {
  const state = useFileDragStore();
  const thumb = useProcessStore((s) => (state.paths[0] ? s.thumbnails[state.paths[0]] : undefined));
  if (!state.source) return null;
  const Icon = state.target?.kind === 'album' ? Images : state.copy ? Copy : FolderInput;
  return (
    <div
      className="pointer-events-none fixed z-[1000] flex items-center gap-2 rounded-lg border border-border-color bg-surface/95 px-2 py-1.5 text-xs text-text-primary shadow-xl backdrop-blur"
      style={{ left: state.x + 14, top: state.y + 14 }}
    >
      {state.source === 'library' && (
        <div className="relative h-9 w-9 shrink-0 overflow-hidden rounded bg-bg-primary">
          {thumb && <img src={thumb} alt="" className="h-full w-full object-cover" draggable={false} />}
          {state.count > 1 && (
            <span className="absolute bottom-0 right-0 rounded-tl bg-accent px-1 text-[10px] font-bold text-white">
              {state.count}
            </span>
          )}
        </div>
      )}
      <Icon size={14} className={state.target ? 'text-accent' : 'text-text-secondary'} />
      <span className={state.target ? '' : 'text-text-secondary'}>{dropCaption(state)}</span>
    </div>
  );
}
