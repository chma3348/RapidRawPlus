import { useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { v4 as uuidv4 } from 'uuid';
import { toast } from 'react-toastify';
import { Cloud, FlipHorizontal2, Loader2, Trash2 } from 'lucide-react';
import Slider from '../../ui/Slider';
import { Invokes } from '../../ui/AppProperties';
import { useEditorStore } from '../../../store/useEditorStore';
import { useEditorActions } from '../../../hooks/useEditorActions';

/**
 * Sky Replace.
 *
 * Choose a plate, see it in the photograph at once, tune it, apply. The
 * result goes into the edit as a patch — the same kind of thing a heal or a
 * fill makes — so it can be hidden, faded or deleted from the Inpaint panel,
 * and every adjustment in the edit applies on top of it.
 *
 * The preview here is the photograph as shot with the new sky, before the
 * edit's adjustments; the canvas shows the finished result once applied.
 */

interface Plate {
  file: string;
  look: string;
  title: string;
  author: string;
  licence: string;
  source: string;
  thumbnail: string;
}

/** Mirrors `SkyReplaceOptions` in `sky_replace.rs`. */
interface SkyOptions {
  relight: number;
  haze: number;
  horizonOffset: number;
  scale: number;
  pan: number;
  flipHorizontal: boolean;
  matchGrain: boolean;
  edgeShift: number;
  edgeFeather: number;
  horizonFade: number;
  whiteBalanceMatch: number;
  skyTemperature: number;
  skyTint: number;
  skyExposure: number;
  skyContrast: number;
  skySaturation: number;
}

const BASE: SkyOptions = {
  relight: 0.55,
  haze: 0.1,
  horizonOffset: 0,
  scale: 1,
  pan: 0,
  flipHorizontal: false,
  matchGrain: true,
  edgeShift: 0,
  edgeFeather: 0,
  horizonFade: 0,
  whiteBalanceMatch: 0.4,
  skyTemperature: 0,
  skyTint: 0,
  skyExposure: 0,
  skyContrast: 0,
  skySaturation: 0,
};
/** The plate exactly as shot; the foreground left alone. */
const AS_SHOT: SkyOptions = { ...BASE, relight: 0, haze: 0, whiteBalanceMatch: 0 };
/** Match the sky to the photograph's light and soften the seam. */
const AUTO: SkyOptions = {
  ...BASE,
  relight: 0.55,
  haze: 0.1,
  whiteBalanceMatch: 0.5,
  horizonFade: 0.1,
  edgeFeather: 0.0015,
  edgeShift: -0.0008,
};

const LOOKS = ['blue-clear', 'blue-clouds', 'mixed', 'overcast', 'stormy', 'sunset', 'twilight'];
let plateCache: Plate[] | null = null;

export default function SkyPanel() {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const { setAdjustments } = useEditorActions();
  const existing = useMemo(
    () => (adjustments.aiPatches || []).find((p: any) => p.patchType === 'sky'),
    [adjustments.aiPatches],
  );

  const [plates, setPlates] = useState<Plate[]>(plateCache ?? []);
  const [look, setLook] = useState<string>('all');
  const [plate, setPlate] = useState<string | null>((existing as any)?.sky?.plate ?? null);
  const [options, setOptions] = useState<SkyOptions>((existing as any)?.sky?.options ?? AUTO);
  const [status, setStatus] = useState<'finding' | 'ready' | 'none' | 'error'>('finding');
  const [message, setMessage] = useState('');
  const [preview, setPreview] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const request = useRef(0);

  useEffect(() => {
    if (plateCache) return;
    invoke<Plate[]>(Invokes.ListSkyPlates)
      .then((p) => {
        plateCache = p;
        setPlates(p);
      })
      .catch((e) => setMessage(String(e)));
  }, []);

  // Find the sky once per photograph. The backend keeps it, so reopening
  // the panel on the same photograph is instant.
  useEffect(() => {
    if (!selectedImage?.path || selectedImage.isVideo) return;
    let cancelled = false;
    setStatus('finding');
    setPreview(null);
    invoke<{ coverage: number }>(Invokes.PrepareSkyReplacement, {
      path: selectedImage.path,
      orientationSteps: adjustments.orientationSteps ?? 0,
      flipHorizontal: adjustments.flipHorizontal ?? false,
      flipVertical: adjustments.flipVertical ?? false,
    })
      .then((r) => {
        if (cancelled) return;
        if (r.coverage < 0.005) {
          setStatus('none');
          setMessage(t('sky.noSky', { defaultValue: 'There is almost no sky in this photograph to replace.' }));
        } else {
          setStatus('ready');
        }
      })
      .catch((e) => {
        if (cancelled) return;
        setStatus('error');
        setMessage(String(e));
      });
    return () => {
      cancelled = true;
    };
    // Orientation changes which way is up for the sky model.
  }, [selectedImage?.path, adjustments.orientationSteps, adjustments.flipHorizontal, adjustments.flipVertical]);

  // Live preview, debounced; a late answer never overwrites a newer one.
  useEffect(() => {
    if (status !== 'ready' || !plate) return;
    const id = ++request.current;
    setPreviewing(true);
    const timer = window.setTimeout(() => {
      invoke<string>(Invokes.PreviewSkyReplacement, { plate, options })
        .then((url) => {
          if (id === request.current) setPreview(url);
        })
        .catch((e) => {
          if (id === request.current) setMessage(String(e));
        })
        .finally(() => {
          if (id === request.current) setPreviewing(false);
        });
    }, 120);
    return () => window.clearTimeout(timer);
  }, [plate, options, status]);

  const apply = async () => {
    if (!plate) return;
    setApplying(true);
    try {
      const patchData = await invoke(Invokes.ApplySkyReplacement, { plate, options });
      const chosen = plates.find((p) => p.file === plate);
      // A new id every time: the backend caches patch pixels by id, so
      // reusing one would keep showing the previous sky.
      const patch = {
        id: uuidv4(),
        name: `${t('sky.patchName', { defaultValue: 'Sky' })}: ${chosen?.look ?? plate}`,
        isLoading: false,
        invert: false,
        prompt: '',
        subMasks: [],
        visible: true,
        opacity: 100,
        feather: 0,
        patchType: 'sky',
        sky: { plate, options },
        patchData,
      };
      setAdjustments((prev: any) => ({
        ...prev,
        aiPatches: [...(prev.aiPatches || []).filter((p: any) => p.patchType !== 'sky'), patch],
      }));
      toast.success(
        t('sky.applied', {
          defaultValue: 'Sky applied. It is a patch: hide, fade or delete it from the Inpaint panel.',
        }),
      );
    } catch (e) {
      toast.error(`${t('sky.failed', { defaultValue: 'Could not replace the sky' })}: ${e}`);
    } finally {
      setApplying(false);
    }
  };

  const remove = () =>
    setAdjustments((prev: any) => ({
      ...prev,
      aiPatches: (prev.aiPatches || []).filter((p: any) => p.patchType !== 'sky'),
    }));

  const set = (patch: Partial<SkyOptions>) => setOptions((o) => ({ ...o, ...patch }));
  /** A slider over a 0..1-style option, shown on a friendlier scale. */
  const slider = (key: keyof SkyOptions, label: string, min: number, max: number, factor = 1, step = 1) => (
    <Slider
      key={key}
      label={label}
      min={min}
      max={max}
      step={step}
      value={Math.round(((options[key] as number) * factor) / step) * step}
      defaultValue={Math.round(((AUTO[key] as number) * factor) / step) * step}
      onChange={(e: any) => set({ [key]: Number(e.target.value) / factor } as Partial<SkyOptions>)}
    />
  );
  const shown = look === 'all' ? plates : plates.filter((p) => p.look === look);
  const chosen = plates.find((p) => p.file === plate);

  if (!selectedImage || selectedImage.isVideo) {
    return <p className="p-4 text-sm text-text-secondary">{t('sky.noPhoto', { defaultValue: 'Open a photograph.' })}</p>;
  }

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-4 text-sm text-text-primary">
      <div className="flex items-center gap-2">
        <Cloud size={18} />
        <h2 className="text-base font-medium">{t('sky.title', { defaultValue: 'Sky Replace' })}</h2>
      </div>

      {status === 'finding' && (
        <p className="flex items-center gap-2 text-text-secondary">
          <Loader2 size={14} className="animate-spin" />
          {t('sky.finding', { defaultValue: 'Finding the sky…' })}
        </p>
      )}
      {(status === 'none' || status === 'error' || message) && status !== 'finding' && (
        <p role="alert" className="text-text-secondary">
          {message}
        </p>
      )}

      <div className="relative overflow-hidden rounded-md bg-surface">
        {preview ? (
          <img src={preview} alt="" className={`w-full ${previewing ? 'opacity-70' : ''}`} />
        ) : (
          <div className="flex aspect-[3/2] items-center justify-center text-text-secondary">
            {status === 'ready'
              ? t('sky.pick', { defaultValue: 'Pick a sky below' })
              : t('sky.waiting', { defaultValue: 'Preparing…' })}
          </div>
        )}
        {previewing && preview && <Loader2 size={16} className="absolute right-2 top-2 animate-spin" />}
      </div>

      <div className="flex flex-wrap gap-1">
        {['all', ...LOOKS].map((l) => (
          <button
            key={l}
            type="button"
            onClick={() => setLook(l)}
            aria-pressed={look === l}
            className={`rounded-md px-2 py-1 text-xs ${look === l ? 'bg-accent text-button-text' : 'bg-surface hover:bg-card-active'}`}
          >
            {t(`sky.look.${l}`, { defaultValue: l === 'all' ? 'All' : l.replace('-', ' ') })}
          </button>
        ))}
      </div>

      <div className="grid max-h-64 grid-cols-3 gap-1 overflow-y-auto">
        {shown.map((p) => (
          <button
            key={p.file}
            type="button"
            onClick={() => setPlate(p.file)}
            aria-pressed={plate === p.file}
            title={p.title}
            disabled={status !== 'ready'}
            className={`overflow-hidden rounded ${plate === p.file ? 'ring-2 ring-accent' : ''} disabled:opacity-50`}
          >
            <img src={convertFileSrc(p.thumbnail)} alt={p.title} loading="lazy" className="aspect-[3/2] w-full object-cover" />
          </button>
        ))}
      </div>
      {chosen && (
        <p className="text-xs text-text-secondary">
          {chosen.author} · {chosen.licence}
        </p>
      )}

      <div className="flex gap-1">
        <button
          type="button"
          onClick={() => setOptions(AS_SHOT)}
          className="flex-1 rounded-md bg-surface px-2 py-1 hover:bg-card-active"
        >
          {t('sky.asShot', { defaultValue: 'As shot' })}
        </button>
        <button
          type="button"
          onClick={() => setOptions(AUTO)}
          className="flex-1 rounded-md bg-surface px-2 py-1 hover:bg-card-active"
        >
          {t('sky.autoMatch', { defaultValue: 'Auto match' })}
        </button>
      </div>

      <h3 className="mt-1 font-medium">{t('sky.position', { defaultValue: 'Position' })}</h3>
      {slider('horizonOffset', t('sky.horizon', { defaultValue: 'Horizon' }), -30, 30, 100)}
      {slider('scale', t('sky.scale', { defaultValue: 'Zoom' }), 100, 300, 100)}
      {slider('pan', t('sky.pan', { defaultValue: 'Slide' }), -50, 50, 100)}
      <button
        type="button"
        onClick={() => set({ flipHorizontal: !options.flipHorizontal })}
        aria-pressed={options.flipHorizontal}
        className="flex items-center gap-2 self-start rounded-md bg-surface px-2 py-1 hover:bg-card-active"
      >
        <FlipHorizontal2 size={14} />
        {t('sky.flip', { defaultValue: 'Flip' })}
      </button>

      <h3 className="mt-1 font-medium">{t('sky.blend', { defaultValue: 'Blend' })}</h3>
      {slider('relight', t('sky.relight', { defaultValue: 'Relight foreground' }), 0, 100, 100)}
      {slider('haze', t('sky.haze', { defaultValue: 'Haze' }), 0, 50, 100)}
      {slider('horizonFade', t('sky.fade', { defaultValue: 'Fade at horizon' }), 0, 30, 100)}
      {slider('edgeShift', t('sky.edgeShift', { defaultValue: 'Edge shift' }), -50, 50, 10000)}
      {slider('edgeFeather', t('sky.edgeFeather', { defaultValue: 'Edge feather' }), 0, 100, 10000)}

      <h3 className="mt-1 font-medium">{t('sky.colour', { defaultValue: 'Sky colour' })}</h3>
      {slider('whiteBalanceMatch', t('sky.match', { defaultValue: 'Match photo light' }), 0, 100, 100)}
      {slider('skyTemperature', t('sky.temperature', { defaultValue: 'Warmth' }), -100, 100)}
      {slider('skyTint', t('sky.tint', { defaultValue: 'Tint' }), -100, 100)}
      {slider('skyExposure', t('sky.exposure', { defaultValue: 'Brightness (stops)' }), -200, 200, 100)}
      {slider('skyContrast', t('sky.contrast', { defaultValue: 'Contrast' }), -100, 100)}
      {slider('skySaturation', t('sky.saturation', { defaultValue: 'Saturation' }), -100, 100)}

      <div className="sticky bottom-0 mt-2 flex gap-2 bg-bg-secondary py-2">
        <button
          type="button"
          onClick={apply}
          disabled={!plate || status !== 'ready' || applying}
          className="flex flex-1 items-center justify-center gap-2 rounded-md bg-accent px-3 py-2 text-button-text disabled:opacity-50"
        >
          {applying && <Loader2 size={14} className="animate-spin" />}
          {applying
            ? t('sky.applying', { defaultValue: 'Applying at full resolution…' })
            : existing
              ? t('sky.replace', { defaultValue: 'Replace sky' })
              : t('sky.apply', { defaultValue: 'Apply sky' })}
        </button>
        {existing && (
          <button
            type="button"
            onClick={remove}
            title={t('sky.remove', { defaultValue: 'Remove the sky' })}
            className="rounded-md bg-surface px-3 py-2 hover:bg-card-active"
          >
            <Trash2 size={14} />
          </button>
        )}
      </div>
    </div>
  );
}
