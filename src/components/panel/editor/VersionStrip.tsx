import { useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Image as ImageIcon, Layers } from 'lucide-react';
import clsx from 'clsx';
import { useProcessStore } from '../../../store/useProcessStore';
import { useStacks } from '../../../hooks/useStacks';
import { stackRootOf, versionLabels } from '../../../utils/stacks';
import type { ImageFile } from '../../ui/AppProperties';

/**
 * The open photo's versions, under the image: the original, its restores,
 * upscales and other made files, and its virtual copies. Clicking one
 * opens it in the editor; each keeps its own edits. Shown only for photos
 * that have versions (see utils/stacks).
 */
export default function VersionStrip({
  selectedPath,
  onSelect,
  requestThumbnails,
}: {
  selectedPath: string | undefined;
  onSelect(path: string): void;
  requestThumbnails?: (paths: string[]) => void;
}) {
  const { t } = useTranslation();
  const stacks = useStacks();
  const root = stackRootOf(stacks, selectedPath);
  const group = root ? stacks.members.get(root) : undefined;
  const labels = useMemo(() => (group ? versionLabels(group) : new Map<string, string>()), [group]);

  useEffect(() => {
    if (group) requestThumbnails?.(group.map((f) => f.path));
  }, [group, requestThumbnails]);

  if (!group || group.length < 2) return null;

  return (
    <div className="flex shrink-0 items-center justify-center gap-2 border-b border-surface px-4 pb-1.5 pt-2">
      <span
        className="flex items-center gap-1 text-xs text-text-secondary"
        data-tooltip={t('editor.versions.tooltip', 'Files made from this photo. Click one to open it.')}
      >
        <Layers size={13} />
        {t('editor.versions.title', 'Versions')}
      </span>
      <div className="flex max-w-full items-center gap-2 overflow-x-auto">
        {group.map((file) => (
          <VersionTile
            key={file.path}
            file={file}
            label={translateLabel(t, labels.get(file.path) ?? '')}
            active={file.path === selectedPath}
            onSelect={onSelect}
          />
        ))}
      </div>
    </div>
  );
}

// "Restored 2" → the translated kind, keeping the number.
const translateLabel = (t: (key: string, fallback: string) => string, label: string) => {
  const m = label.match(/^(.*?)(?:\s+(\d+))?$/);
  const kind = m?.[1] ?? label;
  const n = m?.[2];
  const word = t(`editor.versions.kinds.${kind.replace(/\s+/g, '')}`, kind);
  return n ? `${word} ${n}` : word;
};

function VersionTile({
  file,
  label,
  active,
  onSelect,
}: {
  file: ImageFile;
  label: string;
  active: boolean;
  onSelect(path: string): void;
}) {
  const thumb = useProcessStore((s) => s.thumbnails[file.path]);
  const name = file.path.split(/[\\/]/).pop()?.replace('?vc=', ' · copy ') ?? file.path;
  return (
    <button
      type="button"
      onClick={() => onSelect(file.path)}
      data-tooltip={name}
      className={clsx(
        'group flex shrink-0 flex-col items-center gap-1 rounded-md p-1 transition-colors',
        active ? 'bg-card-active' : 'hover:bg-card-active/60',
      )}
    >
      <div
        className={clsx(
          'flex h-11 w-16 items-center justify-center overflow-hidden rounded bg-bg-primary ring-2',
          active ? 'ring-accent' : 'ring-transparent',
        )}
      >
        {thumb ? (
          <img src={thumb} alt="" className="h-full w-full object-cover" draggable={false} />
        ) : (
          <ImageIcon size={16} className="text-text-secondary" />
        )}
      </div>
      <span className={clsx('text-[11px] leading-none', active ? 'text-text-primary' : 'text-text-secondary')}>
        {label}
      </span>
    </button>
  );
}
