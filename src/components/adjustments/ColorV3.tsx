import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { Pipette } from 'lucide-react';
import Slider from '../ui/Slider';
import Dropdown from '../ui/Dropdown';
import LUTControl from '../ui/LUTControl';
import FlatFieldControl from './FlatFieldControl';
import ColorV3Advanced from './ColorV3Advanced';
import BasicAdjustments from './Basic';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useEditorStore } from '../../store/useEditorStore';
import { useEditorActions } from '../../hooks/useEditorActions';
import {
  defaultV3Controls,
  defaultV3Detail,
  defaultV3Effects,
  V3Controls,
  V3Detail,
  V3Effects,
  V3Calibration,
  defaultV3Calibration,
} from '../../utils/colorV3';

export function ColorV3Switch({
  adjustments,
  setAdjustments,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
}) {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const active = adjustments.processVersion === 3;
  const change = async () => {
    setError('');
    if (active) {
      setAdjustments((prev: any) => ({
        ...prev,
        processVersion: prev.v3PreviousVersion ?? 2,
        // The previous engine has no Resolve rendering; give back its own.
        toneMapper: prev.toneMapper === 'resolve' ? (prev.v3PreviousToneMapper ?? 'agx') : prev.toneMapper,
      }));
      return;
    }
    if (!selectedImage) return;
    const path = selectedImage.path;
    setPending(true);
    try {
      await invoke('prepare_color_v3', { path });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      setAdjustments((prev: any) => ({
        ...prev,
        processVersion: 3,
        v3PreviousVersion: prev.processVersion ?? 2,
        // V3 renders through Resolve by default; the previous engine's
        // mappers stay one click away, and this is restored on the way back.
        v3PreviousToneMapper: prev.toneMapper ?? 'agx',
        toneMapper: 'resolve',
        v3: prev.v3 ?? defaultV3Controls(),
      }));
    } catch (e) {
      setError(String(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="mb-3 border border-surface rounded-md p-3 text-sm text-text-primary">
      <div className="flex items-center justify-between gap-2">
        <span>{t('colorV3.title', { defaultValue: 'Color engine v3' })}</span>
        <button
          type="button"
          disabled={pending || !selectedImage}
          onClick={change}
          aria-pressed={active}
          className="rounded-md px-2 py-1 bg-surface hover:bg-card-active focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50"
        >
          {pending
            ? t('colorV3.checking', { defaultValue: 'Checking image…' })
            : active
              ? t('colorV3.return', { defaultValue: 'Return to previous engine' })
              : t('colorV3.try', { defaultValue: 'Try experimental v3' })}
        </button>
      </div>
      <p className="mt-2 leading-relaxed">
        {t('colorV3.explanation', {
          defaultValue:
            'V3 uses separate color settings. Your previous color edits are preserved when you switch back. Crop and mask shapes are shared.',
        })}
      </p>
      {error && (
        <p role="alert" className="mt-2">
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * The creative LUT. The settings are the previous engine's own — film
 * simulations, presets and copy/paste all carry over — plus the one thing v3
 * needs to know to place a LUT correctly: what it expects to be fed.
 */
function V3LookSection({
  adjustments,
  setAdjustments,
  onDragStateChange,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
  onDragStateChange?: (v: boolean) => void;
}) {
  const { t } = useTranslation();
  const { handleLutSelect, setLutPreviewOverride } = useEditorActions();
  const [simulations, setSimulations] = useState<Array<any>>([]);
  useEffect(() => {
    invoke('list_managed_luts')
      .then((l: any) => setSimulations(l || []))
      .catch(() => setSimulations([]));
  }, []);
  const space = adjustments.lutInputSpace ?? 'display';
  const spaces = [
    {
      value: 'display',
      label: t('colorV3.lutDisplay', { defaultValue: 'Display (sRGB) — most downloaded LUTs' }),
    },
    {
      value: 'intermediate',
      label: t('colorV3.lutIntermediate', { defaultValue: 'DaVinci Intermediate — a look made in Resolve' }),
    },
    {
      value: 'flog2c',
      label: t('colorV3.lutFlog2c', { defaultValue: 'F-Log2 C — Fujifilm film simulation' }),
    },
  ];
  const explanation: Record<string, string> = {
    display: t('colorV3.lutDisplayHelp', {
      defaultValue: 'Applied to the finished picture, after the rendering.',
    }),
    intermediate: t('colorV3.lutIntermediateHelp', {
      defaultValue:
        'Applied to the graded scene before the rendering, as a node would in a DaVinci Wide Gamut timeline.',
    }),
    flog2c: t('colorV3.lutFlog2cHelp', {
      defaultValue: 'Replaces the rendering: the LUT receives the scene as the camera would have encoded it.',
    }),
  };
  return (
    <>
      <h3 className="mt-3 text-sm font-medium text-text-primary">{t('colorV3.lut', { defaultValue: 'LUT' })}</h3>
      {simulations.length > 0 && (
        <Dropdown
          options={simulations.map((p) => ({
            label: p.inputSpace === 'flog2c' ? `${p.name} (Fujifilm)` : p.name,
            value: p.path,
          }))}
          value={adjustments.lutPath || ''}
          onChange={(path: string) => {
            const preset = simulations.find((p) => p.path === path);
            if (!preset) return;
            handleLutSelect(preset.path);
            setAdjustments((prev: any) => ({ ...prev, lutInputSpace: preset.inputSpace }));
          }}
        />
      )}
      <LUTControl
        lutPath={adjustments.lutPath || null}
        lutName={adjustments.lutName || null}
        lutIntensity={adjustments.lutIntensity ?? 100}
        onLutSelect={handleLutSelect}
        onLutHover={setLutPreviewOverride}
        onIntensityChange={(intensity: number) => setAdjustments((prev: any) => ({ ...prev, lutIntensity: intensity }))}
        onClear={() =>
          setAdjustments((prev: any) => ({
            ...prev,
            lutPath: null,
            lutName: null,
            lutData: null,
            lutSize: 0,
            lutIntensity: 100,
            lutInputSpace: 'display',
          }))
        }
        onDragStateChange={onDragStateChange}
      />
      {adjustments.lutPath && (
        <>
          <label className="text-sm text-text-primary">
            {t('colorV3.lutSpace', { defaultValue: 'Made for' })}
            <select
              className="mt-2 w-full bg-surface text-text-primary rounded-md p-2"
              value={space}
              onChange={(e) => setAdjustments((prev: any) => ({ ...prev, lutInputSpace: e.target.value }))}
            >
              {spaces.map((s) => (
                <option value={s.value} key={s.value}>
                  {s.label}
                </option>
              ))}
            </select>
          </label>
          <p className="text-xs text-text-secondary leading-relaxed">{explanation[space] ?? explanation.display}</p>
          {space === 'flog2c' && (
            <Slider
              label={t('adjustments.effects.simExposure', { defaultValue: 'Simulation exposure' })}
              min={-3}
              max={3}
              step={0.05}
              defaultValue={0}
              value={adjustments.lutSimExposure ?? 0}
              onChange={(e: any) =>
                setAdjustments((prev: any) => ({ ...prev, lutSimExposure: parseFloat(e.target.value) }))
              }
              onDragStateChange={onDragStateChange}
            />
          )}
        </>
      )}
    </>
  );
}

export default function ColorV3Controls({
  adjustments,
  setAdjustments,
  onDragStateChange,
  showDetail = true,
  showEffects = true,
  isWbPickerActive = false,
  toggleWbPicker,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
  onDragStateChange?: (v: boolean) => void;
  /** Lets a host hide the detail section; every current host shows it. */
  showDetail?: boolean;
  /** Effects, lens corrections and LUTs describe the whole frame, so masks do not offer them. */
  showEffects?: boolean;
  /** The canvas white-balance picker; hosts without a canvas omit it. */
  isWbPickerActive?: boolean;
  toggleWbPicker?: () => void;
}) {
  const { t } = useTranslation();
  const values: V3Controls = { ...defaultV3Controls(), ...adjustments.v3 };
  const renderError = useEditorStore((s) => s.colorV3Error);
  const appSettings = useSettingsStore((s) => s.appSettings);
  const selectedPath = useEditorStore((s) => s.selectedImage?.path);
  const [band, setBand] = useState(0);
  const [wheel, setWheel] = useState(0);
  const update = (key: keyof V3Controls, value: any) =>
    setAdjustments((prev: any) => ({ ...prev, v3: { ...defaultV3Controls(), ...prev.v3, [key]: value } }));
  const slider = (key: keyof V3Controls, label: string, min = -100, max = 100, step = 1) => (
    <Slider
      key={key}
      label={label}
      value={values[key] as number}
      min={min}
      max={max}
      step={step}
      defaultValue={0}
      onChange={(e: any) => update(key, Number(e.target.value))}
      onDragStateChange={onDragStateChange}
    />
  );
  const detail: V3Detail = { ...defaultV3Detail(), ...values.detail };
  const detailSlider = (key: keyof V3Detail, label: string, min = -100, max = 100, fallback = 0) => (
    <Slider
      key={`detail-${key}`}
      label={label}
      value={detail[key]}
      min={min}
      max={max}
      step={1}
      defaultValue={fallback}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) => update('detail', { ...detail, [key]: Number(e.target.value) })}
    />
  );
  const effects: V3Effects = { ...defaultV3Effects(), ...values.effects };
  const effectSlider = (key: keyof V3Effects, label: string, min: number, max: number) => (
    <Slider
      key={`effect-${key}`}
      label={label}
      value={effects[key]}
      min={min}
      max={max}
      step={1}
      defaultValue={defaultV3Effects()[key]}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) => update('effects', { ...effects, [key]: Number(e.target.value) })}
    />
  );
  const calibration: V3Calibration = { ...defaultV3Calibration(), ...values.calibration };
  const calibrationSlider = (key: keyof V3Calibration, label: string) => (
    <Slider
      key={`calibration-${key}`}
      label={label}
      value={calibration[key]}
      min={-100}
      max={100}
      step={1}
      defaultValue={0}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) => update('calibration', { ...calibration, [key]: Number(e.target.value) })}
    />
  );
  const bands = ['Red', 'Orange', 'Yellow', 'Green', 'Aqua', 'Blue', 'Purple', 'Magenta'];
  const wheels = ['Global', 'Shadows', 'Midtones', 'Highlights'];
  const arraySlider = (
    key: 'bands' | 'grading',
    index: number,
    column: number,
    label: string,
    min: number,
    max: number,
  ) => (
    <Slider
      key={`${key}-${index}-${column}`}
      label={label}
      min={min}
      max={max}
      value={values[key][index][column]}
      defaultValue={0}
      step={1}
      onDragStateChange={onDragStateChange}
      onChange={(e: any) =>
        update(
          key,
          values[key].map((v, i) => (i === index ? v.map((c, j) => (j === column ? Number(e.target.value) : c)) : v)),
        )
      }
    />
  );
  return (
    <div className="flex flex-col gap-2">
      {renderError && (
        <p role="alert" className="border border-surface rounded-md p-3 text-sm text-text-primary">
          {t('colorV3.failed', { defaultValue: 'V3 could not render this edit. The displayed image may be outdated.' })}{' '}
          {renderError}
        </p>
      )}
      <h3 className="text-sm font-medium text-text-primary">{t('colorV3.basic', { defaultValue: 'Basic' })}</h3>
      {/* The previous engine's own Basic panel: same sliders, same saved
          settings, same behaviour — v3 runs its functions for them. */}
      <BasicAdjustments
        adjustments={adjustments}
        setAdjustments={setAdjustments}
        isForMask={!showEffects}
        onDragStateChange={onDragStateChange}
        appSettings={appSettings}
        engineV3
      />
      <h3 className="mt-3 text-sm font-medium text-text-primary">
        {t('colorV3.whiteBalance', { defaultValue: 'White balance' })}
      </h3>
      {toggleWbPicker && (
        <button
          type="button"
          onClick={toggleWbPicker}
          aria-pressed={isWbPickerActive}
          className={`self-start rounded-md px-2 py-1 text-xs flex items-center gap-1 transition-colors ${
            isWbPickerActive ? 'bg-accent text-button-text' : 'bg-surface hover:bg-card-active text-text-primary'
          }`}
        >
          <Pipette size={14} />
          {t('colorV3.wbPicker', { defaultValue: 'Pick a neutral' })}
        </button>
      )}
      {slider('temperature', t('colorV3.temperature', { defaultValue: 'Warmth' }))}
      {slider('tint', t('colorV3.tint', { defaultValue: 'Tint' }))}
      <ColorV3Advanced
        values={values}
        update={update}
        onDragStateChange={onDragStateChange}
        inspection={
          adjustments.processVersion === 3 && selectedPath ? { path: selectedPath, edits: adjustments } : undefined
        }
      />
      <h3 className="mt-3 text-sm font-medium text-text-primary">{t('colorV3.color', { defaultValue: 'Color' })}</h3>
      {slider('saturation', t('colorV3.saturation', { defaultValue: 'Saturation' }))}
      {slider('vibrance', t('colorV3.vibrance', { defaultValue: 'Vibrance' }))}
      {slider('hue', t('colorV3.hue', { defaultValue: 'Hue rotation' }), -180, 180)}
      <label className="mt-3 text-sm text-text-primary">
        {t('colorV3.selective', { defaultValue: 'Selective color' })}
        <select
          className="mt-2 w-full bg-surface text-text-primary rounded-md p-2"
          value={band}
          onChange={(e) => setBand(Number(e.target.value))}
        >
          {bands.map((b, i) => (
            <option value={i} key={b}>
              {t(`colorV3.band.${b}`, { defaultValue: b })}
            </option>
          ))}
        </select>
      </label>
      {arraySlider('bands', band, 0, t('colorV3.bandHue', { defaultValue: 'Hue shift' }), -60, 60)}
      {arraySlider('bands', band, 1, t('colorV3.bandChroma', { defaultValue: 'Chroma' }), -100, 100)}
      {arraySlider('bands', band, 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100)}
      <label className="mt-3 text-sm text-text-primary">
        {t('colorV3.grading', { defaultValue: 'Color grading' })}
        <select
          className="mt-2 w-full bg-surface text-text-primary rounded-md p-2"
          value={wheel}
          onChange={(e) => setWheel(Number(e.target.value))}
        >
          {wheels.map((w, i) => (
            <option value={i} key={w}>
              {t(`colorV3.wheel.${w}`, { defaultValue: w })}
            </option>
          ))}
        </select>
      </label>
      {arraySlider('grading', wheel, 0, t('colorV3.gradingHue', { defaultValue: 'Tint hue' }), 0, 360)}
      {arraySlider('grading', wheel, 1, t('colorV3.gradingAmount', { defaultValue: 'Tint amount' }), 0, 100)}
      {arraySlider('grading', wheel, 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100)}
      {showDetail && (
        <>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.detail', { defaultValue: 'Detail' })}
          </h3>
          {detailSlider('dehaze', t('colorV3.dehaze', { defaultValue: 'Dehaze' }))}
          {detailSlider('sharpening', t('colorV3.sharpening', { defaultValue: 'Sharpening' }))}
          {detailSlider('threshold', t('colorV3.threshold', { defaultValue: 'Sharpening threshold' }), 0, 80, 15)}
          {detailSlider('texture', t('colorV3.texture', { defaultValue: 'Texture' }))}
          {detailSlider('clarity', t('colorV3.clarity', { defaultValue: 'Clarity' }))}
          {detailSlider('structure', t('colorV3.structure', { defaultValue: 'Structure' }))}
          {detailSlider('luminance_noise', t('colorV3.luminanceNoise', { defaultValue: 'Noise reduction' }), 0, 100)}
          {detailSlider('color_noise', t('colorV3.colorNoise', { defaultValue: 'Color noise reduction' }), 0, 100)}
          <p className="text-xs text-text-secondary leading-relaxed">
            {t('colorV3.sharpeningZoom', {
              defaultValue: 'Sharpening works at the scale of single pixels, so judge it at 100% zoom.',
            })}
          </p>
        </>
      )}
      {!showEffects && (
        <>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.localLight', { defaultValue: 'Glow and halation' })}
          </h3>
          {effectSlider('glow_amount', t('colorV3.glow', { defaultValue: 'Glow' }), 0, 100)}
          {effectSlider('halation_amount', t('colorV3.halation', { defaultValue: 'Halation' }), 0, 100)}
        </>
      )}
      {showEffects && (
        <>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.effects', { defaultValue: 'Vignette and grain' })}
          </h3>
          {effectSlider('vignette_amount', t('colorV3.vignette', { defaultValue: 'Vignette' }), -100, 100)}
          {effectSlider(
            'vignette_midpoint',
            t('colorV3.vignetteMidpoint', { defaultValue: 'Vignette midpoint' }),
            0,
            100,
          )}
          {effectSlider(
            'vignette_roundness',
            t('colorV3.vignetteRoundness', { defaultValue: 'Vignette roundness' }),
            -100,
            100,
          )}
          {effectSlider('vignette_feather', t('colorV3.vignetteFeather', { defaultValue: 'Vignette feather' }), 0, 100)}
          {effectSlider('grain_amount', t('colorV3.grain', { defaultValue: 'Grain' }), 0, 100)}
          {effectSlider('grain_size', t('colorV3.grainSize', { defaultValue: 'Grain size' }), 0, 100)}
          {effectSlider('grain_roughness', t('colorV3.grainRoughness', { defaultValue: 'Grain roughness' }), 0, 100)}
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.light', { defaultValue: 'Film and lens looks' })}
          </h3>
          {effectSlider('glow_amount', t('colorV3.glow', { defaultValue: 'Glow' }), 0, 100)}
          {effectSlider('halation_amount', t('colorV3.halation', { defaultValue: 'Halation' }), 0, 100)}
          {effectSlider('flare_amount', t('colorV3.flare', { defaultValue: 'Light flares' }), 0, 100)}
          {effectSlider('film_saturation', t('colorV3.filmSaturation', { defaultValue: 'Film saturation' }), 0, 100)}
          {effectSlider('centre', t('colorV3.centre', { defaultValue: 'Centre' }), -100, 100)}
          <p className="text-xs text-text-secondary leading-relaxed">
            {t('colorV3.lightHelp', {
              defaultValue:
                'These respond to how bright highlights are after exposure, so raising exposure makes more of the picture glow.',
            })}
          </p>
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.lens', { defaultValue: 'Chromatic aberration' })}
          </h3>
          {effectSlider('ca_red_cyan', t('colorV3.caRedCyan', { defaultValue: 'Red / cyan fringe' }), -100, 100)}
          {effectSlider(
            'ca_blue_yellow',
            t('colorV3.caBlueYellow', { defaultValue: 'Blue / yellow fringe' }),
            -100,
            100,
          )}
          <h3 className="mt-3 text-sm font-medium text-text-primary">
            {t('colorV3.calibration', { defaultValue: 'Camera calibration' })}
          </h3>
          {calibrationSlider('shadows_tint', t('colorV3.calShadowsTint', { defaultValue: 'Shadows tint' }))}
          {calibrationSlider('red_hue', t('colorV3.calRedHue', { defaultValue: 'Red primary hue' }))}
          {calibrationSlider('red_saturation', t('colorV3.calRedSat', { defaultValue: 'Red primary saturation' }))}
          {calibrationSlider('green_hue', t('colorV3.calGreenHue', { defaultValue: 'Green primary hue' }))}
          {calibrationSlider(
            'green_saturation',
            t('colorV3.calGreenSat', { defaultValue: 'Green primary saturation' }),
          )}
          {calibrationSlider('blue_hue', t('colorV3.calBlueHue', { defaultValue: 'Blue primary hue' }))}
          {calibrationSlider('blue_saturation', t('colorV3.calBlueSat', { defaultValue: 'Blue primary saturation' }))}
          <div className="mt-3">
            <FlatFieldControl
              adjustments={adjustments}
              setAdjustments={setAdjustments}
              onDragStateChange={onDragStateChange}
            />
          </div>
          <V3LookSection
            adjustments={adjustments}
            setAdjustments={setAdjustments}
            onDragStateChange={onDragStateChange}
          />
        </>
      )}
    </div>
  );
}
