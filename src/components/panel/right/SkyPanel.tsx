import { useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { v4 as uuidv4 } from 'uuid';
import { toast } from 'react-toastify';
import { ChevronLeft, ChevronRight, Cloud, Eye, EyeOff, FlipHorizontal2, Loader2, Trash2 } from 'lucide-react';
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
 * While choosing, the chosen sky shows on the photograph in the editor,
 * through the whole edit, at about screen size; another thumbnail swaps it.
 * Nothing goes into the edit until Apply, which makes the full-size one.
 * (With "Show on photo" off, the small preview here shows the photograph as
 * shot with the new sky instead.)
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

/** Thumbnails per page: two across, big enough to judge a sky by. */
const PAGE = 8;
const sameOptions = (a: SkyOptions | undefined, b: SkyOptions) => !!a && JSON.stringify(a) === JSON.stringify(b);

const LOOKS = ['blue-clear', 'blue-clouds', 'mixed', 'overcast', 'stormy', 'sunset', 'twilight'];
let plateCache: Plate[] | null = null;

export default function SkyPanel() {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const { setAdjustments } = useEditorActions();
  // The detected sky, shown in red on the photograph until a sky is applied.
  const setSkyOverlay = (mask: string | null) => useEditorStore.getState().setEditor({ skyMaskOverlay: mask });
  const existing = useMemo(
    () => (adjustments.aiPatches || []).find((p: any) => p.patchType === 'sky'),
    [adjustments.aiPatches],
  );

  const [plates, setPlates] = useState<Plate[]>(plateCache ?? []);
  const [look, setLook] = useState<string>('all');
  const [plate, setPlate] = useState<string | null>((existing as any)?.sky?.plate ?? null);
  const [options, setOptions] = useState<SkyOptions>((existing as any)?.sky?.options ?? AUTO);
  const [status, setStatus] = useState<'idle' | 'finding' | 'ready' | 'none' | 'error'>('idle');
  const [coverage, setCoverage] = useState(0);
  const [message, setMessage] = useState('');
  const [preview, setPreview] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [page, setPage] = useState(0);
  // Show the chosen sky on the photograph in the editor.
  const [onPhoto, setOnPhoto] = useState(true);
  const [photoPatch, setPhotoPatch] = useState<any>(null);
  const [placing, setPlacing] = useState(false);
  const [skyMask, setSkyMask] = useState<string | null>(null);
  const request = useRef(0);
  const photoRequest = useRef(0);
  const ownsOverride = useRef(false);

  useEffect(() => {
    if (plateCache) return;
    invoke<Plate[]>(Invokes.ListSkyPlates)
      .then((p) => {
        plateCache = p;
        setPlates(p);
      })
      .catch((e) => setMessage(String(e)));
  }, []);

  // The sky is found when asked (the Detect sky button), not on opening the
  // panel: it takes a few seconds and a photograph may have none. A new
  // photograph or a new way up starts over.
  const detection = useRef(0);
  useEffect(() => {
    const id = ++detection.current;
    setStatus('idle');
    setMessage('');
    setPreview(null);
    setPhotoPatch(null);
    setSkyMask(null);
    if (!selectedImage?.path || selectedImage.isVideo) return;
    // Coming back to the panel: a sky already found for this photo is picked
    // up at once (nothing is looked for until Detect sky is pressed).
    invoke<{ coverage: number; mask: string } | null>(Invokes.PrepareSkyReplacement, {
      path: selectedImage.path,
      orientationSteps: adjustments.orientationSteps ?? 0,
      flipHorizontal: adjustments.flipHorizontal ?? false,
      flipVertical: adjustments.flipVertical ?? false,
      cachedOnly: true,
    })
      .then((r) => {
        if (id !== detection.current || !r || r.coverage < 0.005) return;
        setCoverage(r.coverage);
        setSkyMask(r.mask);
        setStatus('ready');
      })
      .catch(() => {});
  }, [selectedImage?.path, adjustments.orientationSteps, adjustments.flipHorizontal, adjustments.flipVertical]);

  const detect = () => {
    if (!selectedImage?.path || selectedImage.isVideo) return;
    const id = ++detection.current;
    setStatus('finding');
    setMessage('');
    setPreview(null);
    setPhotoPatch(null);
    setSkyMask(null);
    invoke<{ coverage: number; mask: string } | null>(Invokes.PrepareSkyReplacement, {
      path: selectedImage.path,
      orientationSteps: adjustments.orientationSteps ?? 0,
      flipHorizontal: adjustments.flipHorizontal ?? false,
      flipVertical: adjustments.flipVertical ?? false,
      cachedOnly: false,
    })
      .then((r) => {
        if (id !== detection.current || !r) return;
        console.warn(`[sky] detected: ${Math.round(r.coverage * 100)}% sky`);
        setCoverage(r.coverage);
        if (r.coverage < 0.005) {
          setStatus('none');
          setMessage(t('sky.noSky', { defaultValue: 'There is almost no sky in this photograph to replace.' }));
        } else {
          setStatus('ready');
          setSkyMask(r.mask);
        }
      })
      .catch((e) => {
        if (id !== detection.current) return;
        console.error(`[sky] detection failed: ${e}`);
        setStatus('error');
        setMessage(String(e));
      });
  };

  // The applied sky, unchanged: the photograph already shows it.
  const unchanged =
    !!existing && (existing as any).sky?.plate === plate && sameOptions((existing as any).sky?.options, options);

  // The chosen sky on the photograph, debounced; a late answer never
  // overwrites a newer one.
  useEffect(() => {
    const id = ++photoRequest.current;
    if (status !== 'ready' || !plate || !onPhoto || unchanged) {
      setPhotoPatch(null);
      setPlacing(false);
      return;
    }
    setPlacing(true);
    const timer = window.setTimeout(() => {
      // A new id each time: the editor sends a patch's pixels once per id.
      const patchId = uuidv4();
      invoke(Invokes.PreviewSkyOnPhoto, { plate, options, id: patchId })
        .then((patchData) => {
          if (id !== photoRequest.current) return;
          setPhotoPatch({
            id: patchId,
            name: 'Sky (preview)',
            isLoading: false,
            invert: false,
            prompt: '',
            subMasks: [],
            visible: true,
            opacity: (existing as any)?.opacity ?? 100,
            feather: 0,
            patchType: 'sky',
            sky: { plate, options },
            patchData,
          });
        })
        .catch((e) => {
          console.error(`[sky] preview on photo failed: ${e}`);
          if (id === photoRequest.current) setMessage(String(e));
        })
        .finally(() => {
          if (id === photoRequest.current) setPlacing(false);
        });
    }, 200);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plate, options, status, onPhoto, unchanged]);

  // Show it through the edit without changing the edit: the editor renders
  // this instead (as it does for a LUT being tried), and saves nothing.
  useEffect(() => {
    const { setEditor } = useEditorStore.getState();
    if (photoPatch) {
      ownsOverride.current = true;
      setEditor({
        previewOverride: {
          ...adjustments,
          aiPatches: [...(adjustments.aiPatches || []).filter((p: any) => p.patchType !== 'sky'), photoPatch],
        },
      });
    } else if (ownsOverride.current) {
      ownsOverride.current = false;
      setEditor({ previewOverride: null });
    }
  }, [photoPatch, adjustments]);

  // The detected sky in red, until a sky is chosen to look at on the photo.
  useEffect(() => {
    setSkyOverlay(status === 'ready' && !(plate && onPhoto) && !existing ? skyMask : null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status, plate, onPhoto, existing, skyMask]);

  // Leaving the panel leaves the photograph as the edit has it.
  useEffect(
    () => () => {
      setSkyOverlay(null);
      if (ownsOverride.current) {
        ownsOverride.current = false;
        useEditorStore.getState().setEditor({ previewOverride: null });
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  // The small preview here, when not shown on the photo; debounced, a late
  // answer never overwrites a newer one.
  useEffect(() => {
    if (status !== 'ready' || !plate || onPhoto) return;
    const id = ++request.current;
    setPreviewing(true);
    const timer = window.setTimeout(() => {
      invoke<string>(Invokes.PreviewSkyReplacement, { plate, options })
        .then((url) => {
          if (id === request.current) setPreview(url);
        })
        .catch((e) => {
          console.error(`[sky] preview failed: ${e}`);
          if (id === request.current) setMessage(String(e));
        })
        .finally(() => {
          if (id === request.current) setPreviewing(false);
        });
    }, 120);
    return () => window.clearTimeout(timer);
  }, [plate, options, status, onPhoto]);

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
      // The edit itself now has the sky: stop showing the stand-in first, so
      // the edit is rendered and saved as it is.
      photoRequest.current += 1;
      setPhotoPatch(null);
      if (ownsOverride.current) {
        ownsOverride.current = false;
        useEditorStore.getState().setEditor({ previewOverride: null });
      }
      setAdjustments((prev: any) => ({
        ...prev,
        aiPatches: [...(prev.aiPatches || []).filter((p: any) => p.patchType !== 'sky'), patch],
      }));
      toast.success(
        t('sky.applied', {
          defaultValue: 'Sky applied. Hide or delete it at the top of this panel; fade it in the Inpaint panel.',
        }),
      );
    } catch (e) {
      console.error(`[sky] apply failed: ${e}`);
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

  const toggleVisible = () =>
    setAdjustments((prev: any) => ({
      ...prev,
      aiPatches: (prev.aiPatches || []).map((p: any) => (p.patchType === 'sky' ? { ...p, visible: !p.visible } : p)),
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
  const pages = Math.max(1, Math.ceil(shown.length / PAGE));
  const onPage = shown.slice(page * PAGE, page * PAGE + PAGE);
  const chosen = plates.find((p) => p.file === plate);
  /** The next or previous sky in the list, turning the page with it. */
  const step = (by: number) => {
    if (!shown.length) return;
    const at = shown.findIndex((p) => p.file === plate);
    const next = at < 0 ? 0 : (at + by + shown.length) % shown.length;
    setPlate(shown[next].file);
    setPage(Math.floor(next / PAGE));
  };

  if (!selectedImage || selectedImage.isVideo) {
    return (
      <p className="p-4 text-sm text-text-secondary">{t('sky.noPhoto', { defaultValue: 'Open a photograph.' })}</p>
    );
  }

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-4 text-sm text-text-primary [&>*]:shrink-0">
      <div className="flex items-center gap-2">
        <Cloud size={18} />
        <h2 className="text-base font-medium">{t('sky.title', { defaultValue: 'Sky Replace' })}</h2>
      </div>

      {existing && (
        // The sky in the edit, as a mask row: hide it or delete it here (it is
        // also listed, with its opacity, in the Inpaint panel).
        <div className="flex items-center gap-2 rounded-md bg-surface p-2">
          <Cloud size={16} className="text-text-secondary" />
          <span className={`flex-1 truncate ${(existing as any).visible === false ? 'text-text-secondary' : ''}`}>
            {(existing as any).name}
          </span>
          <button
            type="button"
            onClick={toggleVisible}
            title={
              (existing as any).visible === false
                ? t('sky.show', { defaultValue: 'Show the sky' })
                : t('sky.hide', { defaultValue: 'Hide the sky' })
            }
            className="rounded p-1 text-text-secondary hover:bg-card-active hover:text-text-primary"
          >
            {(existing as any).visible === false ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
          <button
            type="button"
            onClick={remove}
            title={t('sky.remove', { defaultValue: 'Remove the sky' })}
            className="rounded p-1 text-text-secondary hover:bg-card-active hover:text-red-400"
          >
            <Trash2 size={16} />
          </button>
        </div>
      )}

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={detect}
          disabled={status === 'finding'}
          className="flex items-center gap-2 rounded-md bg-accent px-3 py-1.5 text-button-text hover:opacity-90 disabled:opacity-60"
        >
          {status === 'finding' ? <Loader2 size={14} className="animate-spin" /> : <Cloud size={14} />}
          {status === 'finding'
            ? t('sky.finding', { defaultValue: 'Finding the sky…' })
            : status === 'idle'
              ? t('sky.detect', { defaultValue: 'Detect sky' })
              : t('sky.detectAgain', { defaultValue: 'Detect again' })}
        </button>
        {status === 'ready' && (
          <span className="text-xs text-text-secondary">
            {t('sky.found', {
              defaultValue: 'Sky found: {{percent}}% of the photo',
              percent: Math.round(coverage * 100),
            })}
          </span>
        )}
      </div>
      {(status === 'none' || status === 'error' || message) && status !== 'finding' && (
        <p role="alert" className="text-text-secondary">
          {message}
        </p>
      )}

      <label className="flex items-center gap-2 text-xs text-text-secondary">
        <input type="checkbox" checked={onPhoto} onChange={(e) => setOnPhoto(e.target.checked)} />
        {t('sky.onPhoto', { defaultValue: 'Show on photo' })}
        {placing && <Loader2 size={12} className="animate-spin" />}
      </label>

      {!onPhoto && (
        <div className="relative overflow-hidden rounded-md bg-surface">
          {preview ? (
            <img src={preview} alt="" className={`w-full ${previewing ? 'opacity-70' : ''}`} />
          ) : (
            <div className="flex aspect-[3/2] items-center justify-center text-text-secondary">
              {status === 'ready'
                ? t('sky.pick', { defaultValue: 'Pick a sky below' })
                : status === 'finding'
                  ? t('sky.waiting', { defaultValue: 'Preparing…' })
                  : t('sky.detectFirst', { defaultValue: 'Detect the sky to start' })}
            </div>
          )}
          {previewing && preview && <Loader2 size={16} className="absolute right-2 top-2 animate-spin" />}
        </div>
      )}
      {onPhoto && status !== 'ready' && (
        <p className="text-xs text-text-secondary">
          {status === 'finding'
            ? t('sky.waiting', { defaultValue: 'Preparing…' })
            : t('sky.detectFirst', { defaultValue: 'Detect the sky to start' })}
        </p>
      )}

      <div className="flex flex-wrap gap-1">
        {['all', ...LOOKS].map((l) => (
          <button
            key={l}
            type="button"
            onClick={() => {
              setLook(l);
              setPage(0);
            }}
            aria-pressed={look === l}
            className={`rounded-md px-2 py-1 text-xs ${look === l ? 'bg-accent text-button-text' : 'bg-surface hover:bg-card-active'}`}
          >
            {t(`sky.look.${l}`, { defaultValue: l === 'all' ? 'All' : l.replace('-', ' ') })}
          </button>
        ))}
      </div>

      <div className="grid grid-cols-2 gap-1.5">
        {onPage.map((p) => (
          <button
            key={p.file}
            type="button"
            onClick={() => setPlate(p.file)}
            aria-pressed={plate === p.file}
            title={p.title}
            disabled={status !== 'ready'}
            className={`overflow-hidden rounded ${plate === p.file ? 'ring-2 ring-accent' : 'hover:opacity-90'} disabled:opacity-50`}
          >
            <img
              src={convertFileSrc(p.thumbnail)}
              alt={p.title}
              loading="lazy"
              className="aspect-[3/2] w-full object-cover"
            />
          </button>
        ))}
      </div>
      <div className="flex items-center justify-between text-xs text-text-secondary">
        <button
          type="button"
          onClick={() => setPage((n) => Math.max(0, n - 1))}
          disabled={page === 0}
          title={t('sky.prevPage', { defaultValue: 'Previous page' })}
          className="rounded p-1 hover:bg-card-active disabled:opacity-30"
        >
          <ChevronLeft size={16} />
        </button>
        <span>
          {t('sky.page', { defaultValue: 'Page {{n}} of {{total}}', n: Math.min(page, pages - 1) + 1, total: pages })}
        </span>
        <button
          type="button"
          onClick={() => setPage((n) => Math.min(pages - 1, n + 1))}
          disabled={page >= pages - 1}
          title={t('sky.nextPage', { defaultValue: 'Next page' })}
          className="rounded p-1 hover:bg-card-active disabled:opacity-30"
        >
          <ChevronRight size={16} />
        </button>
      </div>
      {chosen && (
        <div className="flex items-center gap-1 text-xs text-text-secondary">
          <button
            type="button"
            onClick={() => step(-1)}
            disabled={status !== 'ready'}
            title={t('sky.prevSky', { defaultValue: 'Previous sky' })}
            className="rounded p-1 hover:bg-card-active disabled:opacity-30"
          >
            <ChevronLeft size={14} />
          </button>
          <span className="flex-1 truncate text-center">
            {chosen.title} · {chosen.author} · {chosen.licence}
          </span>
          <button
            type="button"
            onClick={() => step(1)}
            disabled={status !== 'ready'}
            title={t('sky.nextSky', { defaultValue: 'Next sky' })}
            className="rounded p-1 hover:bg-card-active disabled:opacity-30"
          >
            <ChevronRight size={14} />
          </button>
        </div>
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
          disabled={!plate || status !== 'ready' || applying || unchanged}
          className="flex flex-1 items-center justify-center gap-2 rounded-md bg-accent px-3 py-2 text-button-text disabled:opacity-50"
        >
          {applying && <Loader2 size={14} className="animate-spin" />}
          {applying
            ? t('sky.applying', { defaultValue: 'Applying at full resolution…' })
            : existing
              ? t('sky.replace', { defaultValue: 'Replace sky' })
              : t('sky.apply', { defaultValue: 'Apply sky' })}
        </button>
      </div>
    </div>
  );
}
