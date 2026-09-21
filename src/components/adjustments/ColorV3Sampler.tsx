import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';

interface Inspection {
  image: string;
  selection: string;
  center: number[] | null;
}
export default function ColorV3Sampler({
  path,
  edits,
  index,
  onCenter,
}: {
  path: string;
  edits: unknown;
  index: number;
  onCenter: (center: number[]) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<Inspection | null>(null);
  const [displayPath, setDisplayPath] = useState('');
  const [loadedKey, setLoadedKey] = useState('');
  const key = JSON.stringify([path, edits, index, open]);
  const current = useRef(key);
  current.current = key;
  const sequence = useRef(0);
  useEffect(() => {
    const id = ++sequence.current;
    setError('');
    if (!open) {
      setBusy(false);
      setResult(null);
      return;
    }
    setBusy(true);
    const timer = setTimeout(() => {
      invoke<Inspection>('inspect_color_v3', { path, edits, index, point: null })
        .then((value) => {
          if (current.current === key && sequence.current === id) {
            setResult(value);
            setDisplayPath(path);
            setLoadedKey(key);
          }
        })
        .catch((e) => {
          if (current.current === key && sequence.current === id) {
            setError(String(e));
            setResult(null);
          }
        })
        .finally(() => {
          if (current.current === key && sequence.current === id) setBusy(false);
        });
    }, 250);
    return () => {
      clearTimeout(timer);
      sequence.current++;
    };
  }, [key]);
  const sample = async (point: number[]) => {
    const id = ++sequence.current;
    setBusy(true);
    setError('');
    try {
      const value = await invoke<Inspection>('inspect_color_v3', { path, edits, index, point });
      if (current.current !== key || sequence.current !== id) return;
      setResult(value);
      setDisplayPath(path);
      if (value.center) onCenter(value.center);
    } catch (e) {
      if (current.current === key && sequence.current === id) setError(String(e));
    } finally {
      if (current.current === key && sequence.current === id) setBusy(false);
    }
  };
  return (
    <div className="my-2 text-sm text-text-primary">
      <button
        type="button"
        aria-expanded={open}
        className="rounded-md px-2 py-1 bg-surface hover:bg-card-active focus-visible:outline-2 focus-visible:outline-accent"
        onClick={() => setOpen((v) => !v)}
      >
        {open
          ? t('colorV3.hideSampler', { defaultValue: 'Hide image picker' })
          : t('colorV3.showSampler', { defaultValue: 'Pick from image / view selection' })}
      </button>
      {open && (
        <>
          <p className="my-2">
            {t('colorV3.sampleHelp', {
              defaultValue:
                'Click this preview to retarget the selected range. It samples after light and tone, before color adjustments and masks. White below means strongest influence; black means none. Enter samples the center.',
            })}
          </p>
          {busy && <p role="status">{t('colorV3.sampling', { defaultValue: 'Updating selection…' })}</p>}
          {error && <p role="alert">{error}</p>}
          {result && displayPath === path && (
            <>
              <button
                type="button"
                disabled={busy || loadedKey !== key}
                aria-label={t('colorV3.sampleImage', {
                  defaultValue: 'Sample color from preview; Enter samples center',
                })}
                className="block w-full p-0 cursor-crosshair focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50"
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  void sample(
                    e.detail === 0 ? [0.5, 0.5] : [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height],
                  );
                }}
              >
                <img
                  className="block w-full h-auto"
                  src={result.image}
                  alt={t('colorV3.sampleSource', { defaultValue: 'Image before selective color' })}
                />
              </button>
              <figure className="mt-2">
                <img
                  className="block w-full h-auto"
                  src={result.selection}
                  alt={t('colorV3.selectionAlt', { defaultValue: 'Selected color range influence in grayscale' })}
                />
                <figcaption className="mt-1">
                  {t('colorV3.selectionCaption', { defaultValue: 'Range influence · 512px inspection preview' })}
                </figcaption>
              </figure>
            </>
          )}
        </>
      )}
    </div>
  );
}
