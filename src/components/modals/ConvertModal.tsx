import { useEffect, useMemo, useState } from 'react';
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import { toast, type Id } from 'react-toastify';
import { ChevronRight, FolderOpen } from 'lucide-react';
import clsx from 'clsx';
import Dropdown from '../ui/Dropdown';
import Slider from '../ui/Slider';
import Switch from '../ui/Switch';
import Button from '../ui/Button';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { LIBRARY_REFRESH_EVENT } from '../../utils/stacks';

/**
 * Convert photos to another file type (HEIC → JPEG, say) without editing
 * them: a short dialog from the right-click menu of a folder or a
 * selection, a progress note while it runs, a summary at the end. The work
 * is done by `convert.rs`; ⌘Z undoes a whole conversion.
 */
interface ConvertState {
  open: boolean;
  paths: string[];
  /** The folder it was started on, when it was a folder. */
  root: string | null;
  openConvert: (paths: string[], root: string | null) => void;
  close: () => void;
}

export const useConvertStore = create<ConvertState>((set) => ({
  open: false,
  paths: [],
  root: null,
  openConvert: (paths, root) => set({ open: paths.length > 0, paths: paths.map((p) => p.split('?vc=')[0]), root }),
  close: () => set({ open: false }),
}));

type Target = 'jpeg' | 'png' | 'tiff' | 'heic' | 'webp' | 'avif';
type Where = 'beside' | 'subfolder' | 'folder';
interface Scan {
  files: string[];
  counts: Record<string, number>;
  edited: number;
}

const extOf = (path: string) => {
  const e = (path.split('.').pop() || '').toUpperCase();
  return e === 'JPEG' ? 'JPG' : e === 'TIF' ? 'TIFF' : e === 'HEIF' ? 'HEIC' : e;
};

/** Run a conversion with a progress note that becomes the summary. */
async function runConversion(request: any, total: number, formatName: string) {
  let toastId: Id | null = null;
  const progress = (current: number) => `${'Converting'} ${current} of ${total} to ${formatName}…`;
  const cancel = (
    <button type="button" className="ml-2 underline" onClick={() => invoke('cancel_conversion').catch(() => {})}>
      Cancel
    </button>
  );
  toastId = toast.info(
    <span>
      {progress(0)}
      {cancel}
    </span>,
    { autoClose: false, closeOnClick: false, progress: 0 },
  );
  const unlisten = await listen<{ current: number; total: number }>('convert-progress', (event) => {
    if (toastId === null) return;
    toast.update(toastId, {
      render: (
        <span>
          {progress(event.payload.current)}
          {cancel}
        </span>
      ),
      progress: event.payload.current / Math.max(1, event.payload.total),
    });
  });
  try {
    const summary = await invoke<{ converted: number; skipped: number; failed: string[]; cancelled: boolean }>(
      'convert_files',
      { request },
    );
    const parts = [`Converted ${summary.converted}`];
    if (summary.skipped) parts.push(`skipped ${summary.skipped} already converted`);
    if (summary.failed.length) parts.push(`${summary.failed.length} failed`);
    const text = `${summary.cancelled ? 'Stopped. ' : ''}${parts.join(', ')} — ⌘Z to undo${
      summary.failed.length ? `\n${summary.failed.slice(0, 3).join('\n')}` : ''
    }`;
    toast.update(toastId, {
      render: text,
      type: summary.failed.length ? 'warning' : 'success',
      autoClose: 6000,
      closeOnClick: true,
      progress: undefined,
    });
  } catch (err) {
    toast.update(toastId, {
      render: `Could not convert: ${err}`,
      type: 'error',
      autoClose: 6000,
      progress: undefined,
    });
  } finally {
    unlisten();
    window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT));
  }
}

export default function ConvertModal() {
  const { t } = useTranslation();
  const { open, paths, root, close } = useConvertStore();
  const [scan, setScan] = useState<Scan | null>(null);
  const [includeSubfolders, setIncludeSubfolders] = useState(false);
  const [types, setTypes] = useState<Set<string>>(new Set());
  const [format, setFormat] = useState<Target>('jpeg');
  const [quality, setQuality] = useState(90);
  const [where, setWhere] = useState<Where>('beside');
  const [subfolder, setSubfolder] = useState('');
  const [folder, setFolder] = useState<string | null>(null);
  const [more, setMore] = useState(false);
  const [limit, setLimit] = useState(false);
  const [maxEdge, setMaxEdge] = useState(3000);
  const [stripLocation, setStripLocation] = useState(false);
  const [trashOriginals, setTrashOriginals] = useState(false);

  // A new session starts from the defaults.
  useEffect(() => {
    if (!open) return;
    setIncludeSubfolders(false);
    setMore(false);
    setTrashOriginals(false);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setScan(null);
    invoke<Scan>('scan_convertible', { paths, includeSubfolders })
      .then((result) => {
        if (cancelled) return;
        setScan(result);
        // Every type found, except the one being converted to.
        setTypes(new Set(Object.keys(result.counts)));
      })
      .catch((e) => {
        if (!cancelled) toast.error(`Could not read the folder: ${e}`);
      });
    return () => {
      cancelled = true;
    };
  }, [open, paths, includeSubfolders]);

  const targetExt = { jpeg: 'JPG', png: 'PNG', tiff: 'TIFF', heic: 'HEIC', webp: 'WEBP', avif: 'AVIF' }[format];
  const formatName = { jpeg: 'JPEG', png: 'PNG', tiff: 'TIFF', heic: 'HEIC', webp: 'WebP', avif: 'AVIF' }[format];
  const files = useMemo(
    () => (scan?.files ?? []).filter((f) => types.has(extOf(f)) && extOf(f) !== targetExt),
    [scan, types, targetExt],
  );
  const hasQuality = format !== 'png' && format !== 'tiff';
  // Location can be removed from JPEG and PNG written by macOS, and WebP and
  // AVIF carry none here; TIFF and HEIC keep theirs.
  const canStripLocation = format !== 'tiff' && format !== 'heic';
  const isFolder = !!root;

  if (!open) return null;

  const start = () => {
    if (files.length === 0) return;
    const request = {
      files,
      format,
      quality,
      maxEdge: limit ? maxEdge : null,
      destination:
        where === 'beside'
          ? { kind: 'beside' }
          : where === 'subfolder'
            ? { kind: 'subfolder', name: subfolder || formatName.toUpperCase() }
            : { kind: 'folder', path: folder },
      root,
      stripLocation: canStripLocation && stripLocation,
      trashOriginals,
    };
    close();
    void runConversion(request, files.length, formatName);
  };

  const whereText =
    where === 'beside'
      ? t('convert.whereBeside', { defaultValue: 'next to the originals' })
      : where === 'subfolder'
        ? t('convert.whereSubfolder', {
            defaultValue: 'in a “{{name}}” folder',
            name: subfolder || formatName.toUpperCase(),
          })
        : folder
          ? t('convert.whereFolder', { defaultValue: 'in {{name}}', name: folder.split(/[\\/]/).pop() })
          : t('convert.whereChoose', { defaultValue: 'in a folder you choose' });

  return (
    <div
      aria-modal="true"
      role="dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 backdrop-blur-xs"
      onClick={close}
    >
      <div
        className="w-full max-w-md rounded-lg bg-surface p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Escape') close();
        }}
      >
        <Text variant={TextVariants.title} className="mb-4">
          {scan
            ? t('convert.title', { defaultValue: 'Convert {{count}} photos', count: files.length })
            : t('convert.looking', { defaultValue: 'Looking for photos…' })}
        </Text>

        <div className="flex flex-col gap-4 text-sm text-text-primary">
          <div className="flex items-start gap-3">
            <span className="w-12 shrink-0 pt-1 text-text-secondary">
              {t('convert.from', { defaultValue: 'From' })}
            </span>
            <div className="flex flex-wrap gap-2">
              {Object.entries(scan?.counts ?? {}).map(([ext, count]) => (
                <label
                  key={ext}
                  className={clsx(
                    'flex cursor-pointer items-center gap-1.5 rounded-md bg-bg-primary px-2 py-1',
                    ext === targetExt && 'opacity-40',
                  )}
                >
                  <input
                    type="checkbox"
                    className="accent-accent"
                    disabled={ext === targetExt}
                    checked={types.has(ext) && ext !== targetExt}
                    onChange={(e) =>
                      setTypes((prev) => {
                        const next = new Set(prev);
                        if (e.target.checked) next.add(ext);
                        else next.delete(ext);
                        return next;
                      })
                    }
                  />
                  {ext} ({count})
                </label>
              ))}
              {scan && Object.keys(scan.counts).length === 0 && (
                <span className="text-text-secondary">
                  {t('convert.nothing', { defaultValue: 'No photos found here.' })}
                </span>
              )}
            </div>
          </div>

          <div className="flex items-center gap-3">
            <span className="w-12 shrink-0 text-text-secondary">{t('convert.to', { defaultValue: 'To' })}</span>
            <div className="flex-1">
              <Dropdown
                options={[
                  { value: 'jpeg', label: 'JPEG' },
                  { value: 'png', label: 'PNG' },
                  { value: 'tiff', label: 'TIFF' },
                  { value: 'heic', label: 'HEIC' },
                  { value: 'webp', label: 'WebP' },
                  { value: 'avif', label: 'AVIF' },
                ]}
                value={format}
                onChange={(v: Target) => setFormat(v)}
              />
            </div>
          </div>
          {hasQuality && (
            <Slider
              label={t('convert.quality', { defaultValue: 'Quality' })}
              min={50}
              max={100}
              step={1}
              defaultValue={90}
              value={quality}
              fillOrigin="min"
              onChange={(e: any) => setQuality(Number(e.target.value))}
            />
          )}

          <div className="flex items-center gap-3">
            <span className="w-12 shrink-0 text-text-secondary">{t('convert.save', { defaultValue: 'Save' })}</span>
            <div className="flex-1">
              <Dropdown
                options={[
                  { value: 'beside', label: t('convert.optBeside', { defaultValue: 'Next to the originals' }) },
                  { value: 'subfolder', label: t('convert.optSubfolder', { defaultValue: 'In a subfolder' }) },
                  { value: 'folder', label: t('convert.optFolder', { defaultValue: 'In a folder you choose' }) },
                ]}
                value={where}
                onChange={(v: Where) => setWhere(v)}
              />
            </div>
          </div>
          {where === 'subfolder' && (
            <input
              className="ml-15 rounded-md border border-surface bg-bg-primary p-2 text-sm"
              placeholder={formatName.toUpperCase()}
              value={subfolder}
              onChange={(e) => setSubfolder(e.target.value)}
            />
          )}
          {where === 'folder' && (
            <button
              type="button"
              onClick={async () => {
                const chosen = await openDialog({ directory: true, multiple: false });
                if (typeof chosen === 'string') setFolder(chosen);
              }}
              className="flex items-center gap-2 rounded-md bg-bg-primary px-3 py-2 text-left text-sm hover:bg-card-active"
            >
              <FolderOpen size={14} />
              <span className="truncate">
                {folder ?? t('convert.chooseFolder', { defaultValue: 'Choose a folder…' })}
              </span>
            </button>
          )}

          <button
            type="button"
            onClick={() => setMore((m) => !m)}
            className="flex items-center gap-1 self-start text-xs text-text-secondary hover:text-text-primary"
          >
            <ChevronRight size={13} className={clsx('transition-transform', more && 'rotate-90')} />
            {t('convert.moreOptions', { defaultValue: 'More options' })}
          </button>
          {more && (
            <div className="flex flex-col gap-3 rounded-md bg-bg-primary/50 p-3">
              {isFolder && (
                <Switch
                  label={t('convert.includeSubfolders', { defaultValue: 'Include subfolders' })}
                  checked={includeSubfolders}
                  onChange={setIncludeSubfolders}
                />
              )}
              <Switch
                label={t('convert.limitSize', { defaultValue: 'Limit size' })}
                checked={limit}
                onChange={setLimit}
              />
              {limit && (
                <label className="flex items-center gap-2 pl-1 text-xs text-text-secondary">
                  {t('convert.longEdge', { defaultValue: 'Long edge' })}
                  <input
                    type="number"
                    min={200}
                    max={20000}
                    value={maxEdge}
                    onChange={(e) => setMaxEdge(Math.max(200, Number(e.target.value) || 3000))}
                    className="w-24 rounded-md border border-surface bg-bg-primary p-1 text-text-primary"
                  />
                  px
                </label>
              )}
              <Switch
                label={t('convert.stripLocation', { defaultValue: 'Remove location' })}
                checked={canStripLocation && stripLocation}
                disabled={!canStripLocation}
                tooltip={
                  canStripLocation
                    ? undefined
                    : t('convert.stripLocationNot', { defaultValue: 'TIFF and HEIC files keep their location' })
                }
                onChange={setStripLocation}
              />
              <Switch
                label={t('convert.trashOriginals', { defaultValue: 'Move originals to the Trash' })}
                checked={trashOriginals}
                onChange={setTrashOriginals}
              />
            </div>
          )}

          {scan && scan.edited > 0 && (
            <p className="text-xs text-text-secondary">
              {t('convert.editedNote', {
                defaultValue:
                  '{{count}} of these have edits. Converting keeps them as shot; use Export to include edits.',
                count: scan.edited,
              })}
            </p>
          )}
          {files.length > 0 && (
            <p className="text-xs text-text-secondary">
              {t('convert.summary', {
                defaultValue: '{{count}} photos → {{format}}, {{where}}.',
                count: files.length,
                format: formatName,
                where: whereText,
              })}
            </p>
          )}
        </div>

        <div className="mt-6 flex justify-end gap-3">
          <Button variant="ghost" onClick={close}>
            {t('modals.confirm.cancel', { defaultValue: 'Cancel' })}
          </Button>
          <Button onClick={start} disabled={files.length === 0 || (where === 'folder' && !folder)}>
            {t('convert.convert', { defaultValue: 'Convert' })}
          </Button>
        </div>
      </div>
    </div>
  );
}
