import { ReactNode, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { EyeOff, GripVertical, Pipette, Sparkles } from 'lucide-react';
import {
  DndContext,
  PointerSensor,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import Slider from '../ui/Slider';
import Dropdown from '../ui/Dropdown';
import Switch from '../ui/Switch';
import LUTControl from '../ui/LUTControl';
import ColorWheel from '../ui/ColorWheel';
import type { HueSatLum } from '../../utils/adjustments';
import FlatFieldControl from './FlatFieldControl';
import { ColorRangeControls, ToneCurveControls } from './ColorV3Advanced';
import BasicAdjustments from './Basic';
import AdjustmentSection, { BasicGroupTitle } from './AdjustmentSection';
import { useEditorStore } from '../../store/useEditorStore';
import { useEditorActions } from '../../hooks/useEditorActions';
import { useEditorLayout } from '../../hooks/useEditorLayout';
import {
  defaultV3Controls,
  defaultV3Detail,
  defaultV3Effects,
  V3Controls,
  V3Detail,
  V3Effects,
  V3Calibration,
  defaultV3Calibration,
  V3PipelineIdentity,
} from '../../utils/colorV3';

const differs = (a: unknown, b: unknown) => JSON.stringify(a) !== JSON.stringify(b);

/**
 * RAW interpretation for this photo: highlight recovery, and keeping the
 * photo's look fixed if the colour engine is later updated.
 */
function RawControls({ adjustments, setAdjustments }: { adjustments: any; setAdjustments: (fn: any) => void }) {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    setError('');
  }, [selectedImage?.path]);
  const pin = async (recovery?: boolean) => {
    if (!selectedImage || pending) return;
    const path = selectedImage.path;
    setError('');
    setPending(true);
    try {
      const result = await invoke<{ pipeline: V3PipelineIdentity }>('pin_color_v3', { path, edits: adjustments });
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      // Revision 2 adds the recovery choice; older builds must refuse it,
      // not silently ignore "off" and render the old highlight treatment.
      setAdjustments((prev: any) => ({
        ...prev,
        v3Pipeline: result.pipeline,
        ...(recovery === undefined ? {} : { v3RawRecovery: recovery ? 'neutral_green_v1' : 'off' }),
      }));
    } catch (e) {
      if (useEditorStore.getState().selectedImage?.path === path) setError(String(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="flex flex-col gap-3 text-sm text-text-primary">
      <Switch
        label={t('colorV3.rawRecovery', { defaultValue: 'Neutralize clipped RAW highlights' })}
        checked={(adjustments.v3RawRecovery ?? 'neutral_green_v1') === 'neutral_green_v1'}
        disabled={pending || !selectedImage}
        onChange={(checked: boolean) => void pin(checked)}
      />
      <p className="text-xs text-text-secondary leading-relaxed">
        {t('colorV3.rawRecoveryHelp', {
          defaultValue:
            'RAW only. On by default. Turn off to use the unrecovered sensor colours; clipped detail is not restored. This does not change the Highlights slider.',
        })}
      </p>
      {adjustments.v3Pipeline ? (
        <p className="text-xs text-text-secondary leading-relaxed">
          {t('colorV3.pipelinePinned', {
            defaultValue: "This photo's look is kept as it is, even if the colour engine is updated.",
          })}
        </p>
      ) : (
        <button
          type="button"
          disabled={pending || !selectedImage}
          onClick={() => void pin()}
          className="self-start rounded-md px-2 py-1 text-xs bg-surface hover:bg-card-active focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-50"
        >
          {pending
            ? t('colorV3.locking', { defaultValue: 'Keeping…' })
            : t('colorV3.lockPipeline', { defaultValue: "Keep this photo's look if the engine is updated" })}
        </button>
      )}
      {error && (
        <p role="alert" className="text-xs">
          {error}
        </p>
      )}
    </div>
  );
}

function useSimulations() {
  const [simulations, setSimulations] = useState<Array<any>>([]);
  useEffect(() => {
    invoke('list_managed_luts')
      .then((l: any) => setSimulations(l || []))
      .catch(() => setSimulations([]));
  }, []);
  return simulations;
}

const clearedLook = {
  lutPath: null,
  lutName: null,
  lutData: null,
  lutSize: 0,
  lutIntensity: 100,
  lutInputSpace: 'display',
};

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
  const simulations = useSimulations();
  const space = adjustments.lutInputSpace ?? 'display';
  const spaces = [
    { value: 'display', label: t('colorV3.lutDisplay', { defaultValue: 'Standard (most downloaded looks)' }) },
    { value: 'intermediate', label: t('colorV3.lutIntermediate', { defaultValue: 'Log (made in DaVinci Resolve)' }) },
    { value: 'flog2c', label: t('colorV3.lutFlog2c', { defaultValue: 'Log (Fujifilm film simulation)' }) },
  ];
  const explanation: Record<string, string> = {
    display: t('colorV3.lutDisplayHelp', { defaultValue: 'Applied to the finished picture.' }),
    intermediate: t('colorV3.lutIntermediateHelp', {
      defaultValue: 'Applied to the graded scene before the final rendering, as in a Resolve timeline.',
    }),
    flog2c: t('colorV3.lutFlog2cHelp', {
      defaultValue: 'Replaces the final rendering: the look receives the scene as the camera would have recorded it.',
    }),
  };
  return (
    <>
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
        onClear={() => setAdjustments((prev: any) => ({ ...prev, ...clearedLook }))}
        onDragStateChange={onDragStateChange}
      />
      {adjustments.lutPath && (
        <>
          <p className="mt-1 text-xs text-text-secondary">
            {t('colorV3.lutSpace', { defaultValue: 'This look expects' })}
          </p>
          <Dropdown
            options={spaces}
            value={space}
            onChange={(value: string) => setAdjustments((prev: any) => ({ ...prev, lutInputSpace: value }))}
          />
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

/** Basic mode's look: pick a film simulation and how strong it is. */
function BasicLook({
  adjustments,
  setAdjustments,
  onDragStateChange,
}: {
  adjustments: any;
  setAdjustments: (fn: any) => void;
  onDragStateChange?: (v: boolean) => void;
}) {
  const { t } = useTranslation();
  const { handleLutSelect } = useEditorActions();
  const simulations = useSimulations();
  const none = '__none__';
  const custom =
    adjustments.lutPath && !simulations.some((p) => p.path === adjustments.lutPath)
      ? [
          {
            label: adjustments.lutName || t('colorV3.customLook', { defaultValue: 'Custom look' }),
            value: adjustments.lutPath,
          },
        ]
      : [];
  return (
    <>
      <Dropdown
        options={[
          { label: t('colorV3.noLook', { defaultValue: 'None' }), value: none },
          ...simulations.map((p) => ({
            label: p.inputSpace === 'flog2c' ? `${p.name} (Fujifilm)` : p.name,
            value: p.path,
          })),
          ...custom,
        ]}
        value={adjustments.lutPath || none}
        onChange={(path: string) => {
          if (path === none) {
            setAdjustments((prev: any) => ({ ...prev, ...clearedLook }));
            return;
          }
          const preset = simulations.find((p) => p.path === path);
          if (!preset) return;
          handleLutSelect(preset.path);
          setAdjustments((prev: any) => ({ ...prev, lutInputSpace: preset.inputSpace }));
        }}
      />
      {adjustments.lutPath && (
        <Slider
          label={t('colorV3.lookStrength', { defaultValue: 'Strength' })}
          min={0}
          max={100}
          step={1}
          defaultValue={100}
          value={adjustments.lutIntensity ?? 100}
          fillOrigin="min"
          onChange={(e: any) => setAdjustments((prev: any) => ({ ...prev, lutIntensity: Number(e.target.value) }))}
          onDragStateChange={onDragStateChange}
        />
      )}
    </>
  );
}

/**
 * An Advanced section that can be dragged by its grip to a new place, and
 * hidden. The grip and hide buttons appear when the pointer is over the
 * header.
 */
function MovableSection({
  id,
  title,
  modified,
  open,
  onToggle,
  onHide,
  children,
}: {
  id: string;
  title: string;
  modified: boolean;
  open: boolean;
  onToggle: () => void;
  onHide: () => void;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const drag = useDraggable({ id });
  const drop = useDroppable({ id });
  const style = drag.transform
    ? {
        transform: `translate3d(0, ${drag.transform.y}px, 0)`,
        position: 'relative' as const,
        zIndex: 20,
        opacity: 0.9,
      }
    : undefined;
  return (
    <AdjustmentSection
      title={title}
      modified={modified}
      open={open && !drag.isDragging}
      onToggle={onToggle}
      sectionRef={(el) => {
        drag.setNodeRef(el);
        drop.setNodeRef(el);
      }}
      style={style}
      highlight={drop.isOver && !drag.isDragging}
      actions={
        <>
          <button
            type="button"
            onClick={onHide}
            className="rounded p-1 text-text-secondary opacity-0 transition-opacity hover:bg-surface hover:text-text-primary group-hover:opacity-100 focus-visible:opacity-100"
            data-tooltip={t('colorV3.hideSection', { defaultValue: 'Hide this section' })}
          >
            <EyeOff size={13} />
          </button>
          <button
            type="button"
            {...drag.listeners}
            {...drag.attributes}
            className="cursor-grab rounded p-1 text-text-secondary opacity-0 transition-opacity hover:bg-surface hover:text-text-primary group-hover:opacity-100 focus-visible:opacity-100 active:cursor-grabbing"
            data-tooltip={t('colorV3.moveSection', { defaultValue: 'Drag to move this section' })}
          >
            <GripVertical size={13} />
          </button>
        </>
      }
    >
      {children}
    </AdjustmentSection>
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
  mode = 'advanced',
  onAuto,
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
  /** Basic: a short set of easy controls. Advanced: everything, in sections. */
  mode?: 'basic' | 'advanced';
  /** Basic mode's Auto button. */
  onAuto?: () => void;
}) {
  const { t } = useTranslation();
  const values: V3Controls = { ...defaultV3Controls(), ...adjustments.v3 };
  const renderError = useEditorStore((s) => s.colorV3Error);
  const selectedPath = useEditorStore((s) => s.selectedImage?.path);
  const [layout, updateLayout] = useEditorLayout();
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 4 } }));
  const [band, setBand] = useState(0);
  const [gradingExpanded, setGradingExpanded] = useState(false);
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
  const wbPicker = toggleWbPicker && (
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
  );
  const errorNotice = renderError && (
    <p role="alert" className="mb-3 rounded-md bg-surface p-3 text-xs text-text-primary">
      {t('colorV3.failed', {
        defaultValue: 'This edit could not be shown. The picture on screen may be out of date.',
      })}
    </p>
  );
  const defaults = defaultV3Controls();
  const top = (k: string) => adjustments[k] ?? 0;

  // ---------------------------------------------------------------- Basic
  if (mode === 'basic') {
    return (
      <div className="flex flex-col gap-6">
        {errorNotice}
        {onAuto && (
          <button
            type="button"
            onClick={onAuto}
            disabled={!selectedPath}
            className="flex items-center justify-center gap-2 rounded-lg bg-surface py-2 text-sm font-medium text-text-primary hover:bg-card-active disabled:opacity-50"
          >
            <Sparkles size={15} className="text-accent" />
            {t('colorV3.auto', { defaultValue: 'Auto' })}
          </button>
        )}
        {showEffects && (
          <div className="flex flex-col gap-1">
            <BasicGroupTitle>{t('colorV3.look', { defaultValue: 'Look' })}</BasicGroupTitle>
            <BasicLook
              adjustments={adjustments}
              setAdjustments={setAdjustments}
              onDragStateChange={onDragStateChange}
            />
          </div>
        )}
        <div className="flex flex-col">
          <BasicGroupTitle>{t('colorV3.whiteBalance', { defaultValue: 'White Balance' })}</BasicGroupTitle>
          {wbPicker}
          {slider('temperature', t('colorV3.temperature', { defaultValue: 'Warmth' }))}
          {slider('tint', t('colorV3.tint', { defaultValue: 'Tint' }))}
        </div>
        <div>
          <BasicGroupTitle>{t('colorV3.groupLight', { defaultValue: 'Light' })}</BasicGroupTitle>
          <BasicAdjustments
            adjustments={adjustments}
            setAdjustments={setAdjustments}
            onDragStateChange={onDragStateChange}
            simple
          />
        </div>
        <div>
          <BasicGroupTitle>{t('colorV3.presence', { defaultValue: 'Presence' })}</BasicGroupTitle>
          {showDetail && detailSlider('texture', t('colorV3.texture', { defaultValue: 'Texture' }))}
          {showDetail && detailSlider('clarity', t('colorV3.clarity', { defaultValue: 'Clarity' }))}
          {showDetail && detailSlider('dehaze', t('colorV3.dehaze', { defaultValue: 'Dehaze' }))}
          {slider('vibrance', t('colorV3.vibrance', { defaultValue: 'Vibrance' }))}
          {slider('saturation', t('colorV3.saturation', { defaultValue: 'Saturation' }))}
        </div>
        {showDetail && (
          <div>
            <BasicGroupTitle>{t('colorV3.detail', { defaultValue: 'Detail' })}</BasicGroupTitle>
            {detailSlider('sharpening', t('colorV3.sharpening', { defaultValue: 'Sharpening' }))}
            {detailSlider('luminance_noise', t('colorV3.luminanceNoise', { defaultValue: 'Noise reduction' }), 0, 100)}
          </div>
        )}
        {showEffects && (
          <div>
            <BasicGroupTitle>{t('colorV3.groupEffects', { defaultValue: 'Effects' })}</BasicGroupTitle>
            {effectSlider('vignette_amount', t('colorV3.vignette', { defaultValue: 'Vignette' }), -100, 100)}
            {effectSlider('grain_amount', t('colorV3.grain', { defaultValue: 'Grain' }), 0, 100)}
          </div>
        )}
        <p className="text-center text-xs text-text-secondary">
          {t('colorV3.moreInAdvanced', { defaultValue: 'Curves, color mixing, grading and more are in Advanced.' })}
        </p>
      </div>
    );
  }

  // ------------------------------------------------------------- Advanced
  const sections: Record<string, { title: string; modified: boolean; body: ReactNode; available: boolean }> = {
    light: {
      title: t('colorV3.groupLight', { defaultValue: 'Light' }),
      modified: ['exposure', 'brightness', 'contrast', 'highlights', 'shadows', 'whites', 'blacks'].some(
        (k) => top(k) !== 0,
      ),
      available: true,
      body: (
        <BasicAdjustments
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          onDragStateChange={onDragStateChange}
        />
      ),
    },
    whiteBalance: {
      title: t('colorV3.whiteBalance', { defaultValue: 'White Balance' }),
      modified: values.temperature !== 0 || values.tint !== 0,
      available: true,
      body: (
        <>
          {wbPicker}
          {slider('temperature', t('colorV3.temperature', { defaultValue: 'Warmth' }))}
          {slider('tint', t('colorV3.tint', { defaultValue: 'Tint' }))}
        </>
      ),
    },
    toneCurve: {
      title: t('colorV3.curveTitle', { defaultValue: 'Tone Curve' }),
      modified: differs(values.curve, defaults.curve) || differs(values.channel_curves, defaults.channel_curves),
      available: true,
      body: <ToneCurveControls values={values} update={update} onDragStateChange={onDragStateChange} />,
    },
    // Presence, as Lightroom has it: local contrast and colour intensity,
    // the controls reached for right after exposure.
    presence: {
      title: t('colorV3.presence', { defaultValue: 'Presence' }),
      modified:
        values.saturation !== 0 ||
        values.vibrance !== 0 ||
        detail.texture !== 0 ||
        detail.clarity !== 0 ||
        detail.dehaze !== 0 ||
        detail.structure !== 0,
      available: true,
      body: (
        <>
          {showDetail && detailSlider('texture', t('colorV3.texture', { defaultValue: 'Texture' }))}
          {showDetail && detailSlider('clarity', t('colorV3.clarity', { defaultValue: 'Clarity' }))}
          {showDetail && detailSlider('dehaze', t('colorV3.dehaze', { defaultValue: 'Dehaze' }))}
          {showDetail && detailSlider('structure', t('colorV3.structure', { defaultValue: 'Structure' }))}
          {slider('vibrance', t('colorV3.vibrance', { defaultValue: 'Vibrance' }))}
          {slider('saturation', t('colorV3.saturation', { defaultValue: 'Saturation' }))}
        </>
      ),
    },
    colorMixer: {
      title: t('colorV3.colorMixer', { defaultValue: 'Color Mixer' }),
      modified: differs(values.bands, defaults.bands) || values.ranges.length > 0 || values.hue !== 0,
      available: true,
      body: (
        <>
          {slider('hue', t('colorV3.hue', { defaultValue: 'Hue rotation' }), -180, 180)}
          <p className="mt-2 text-xs text-text-secondary">
            {t('colorV3.selective', { defaultValue: 'Selective color' })}
          </p>
          <Dropdown
            options={bands.map((b, i) => ({ value: i, label: t(`colorV3.band.${b}`, { defaultValue: b }) }))}
            value={band}
            onChange={(i: number) => setBand(i)}
          />
          {arraySlider('bands', band, 0, t('colorV3.bandHue', { defaultValue: 'Hue shift' }), -60, 60)}
          {arraySlider('bands', band, 1, t('colorV3.bandChroma', { defaultValue: 'Chroma' }), -100, 100)}
          {arraySlider('bands', band, 2, t('colorV3.bandLightness', { defaultValue: 'Lightness' }), -100, 100)}
          <p className="mt-3 text-xs text-text-secondary">
            {t('colorV3.rangeTitle', { defaultValue: 'Custom color ranges' })}
          </p>
          <ColorRangeControls
            values={values}
            update={update}
            onDragStateChange={onDragStateChange}
            inspection={
              adjustments.processVersion === 3 && selectedPath ? { path: selectedPath, edits: adjustments } : undefined
            }
          />
        </>
      ),
    },
    colorGrading: {
      title: t('colorV3.grading', { defaultValue: 'Color Grading' }),
      modified: differs(values.grading, defaults.grading),
      available: true,
      body: (
        // One wheel per range: the angle is the tint's hue, the distance
        // from the centre its amount, the slider below its lightness.
        <>
          <button
            type="button"
            onClick={() => setGradingExpanded((v) => !v)}
            aria-pressed={gradingExpanded}
            className="self-end rounded-md px-2 py-0.5 text-xs text-text-secondary hover:bg-surface hover:text-text-primary"
          >
            {gradingExpanded
              ? t('colorV3.gradingCompact', { defaultValue: 'Compact wheels' })
              : t('colorV3.gradingPrecise', { defaultValue: 'Precise values' })}
          </button>
          <div className={gradingExpanded ? 'flex flex-col gap-4' : 'grid grid-cols-2 gap-x-4 gap-y-3'}>
            {[1, 2, 3, 0].map((index) => (
              <div key={wheels[index]} className="min-w-0">
                <ColorWheel
                  label={t(`colorV3.wheel.${wheels[index]}`, { defaultValue: wheels[index] })}
                  defaultValue={{ hue: 0, saturation: 0, luminance: 0 }}
                  value={{
                    hue: values.grading[index][0],
                    saturation: values.grading[index][1],
                    luminance: values.grading[index][2],
                  }}
                  onChange={(hsl: HueSatLum) =>
                    update(
                      'grading',
                      values.grading.map((v, i) =>
                        i === index ? [hsl.hue, Math.max(0, Math.min(100, hsl.saturation)), hsl.luminance] : v,
                      ),
                    )
                  }
                  onDragStateChange={onDragStateChange}
                  isExpanded={gradingExpanded}
                />
              </div>
            ))}
          </div>
        </>
      ),
    },
    detail: {
      title: t('colorV3.detail', { defaultValue: 'Detail' }),
      // Cleanup: sharpening and noise. Texture, clarity and dehaze are in Presence.
      modified:
        detail.sharpening !== 0 ||
        detail.threshold !== defaultV3Detail().threshold ||
        detail.luminance_noise !== 0 ||
        detail.color_noise !== 0,
      available: showDetail,
      body: (
        <>
          {detailSlider('sharpening', t('colorV3.sharpening', { defaultValue: 'Sharpening' }))}
          {detailSlider('threshold', t('colorV3.threshold', { defaultValue: 'Sharpening threshold' }), 0, 80, 15)}
          {detailSlider('luminance_noise', t('colorV3.luminanceNoise', { defaultValue: 'Noise reduction' }), 0, 100)}
          {detailSlider('color_noise', t('colorV3.colorNoise', { defaultValue: 'Color noise reduction' }), 0, 100)}
          <p className="text-xs text-text-secondary leading-relaxed">
            {t('colorV3.sharpeningZoom', {
              defaultValue: 'Sharpening works at the scale of single pixels, so judge it at 100% zoom.',
            })}
          </p>
        </>
      ),
    },
    effects: {
      title: t('colorV3.groupEffects', { defaultValue: 'Effects' }),
      modified: differs(
        { ...effects, ca_red_cyan: 0, ca_blue_yellow: 0 },
        { ...defaultV3Effects(), ca_red_cyan: 0, ca_blue_yellow: 0 },
      ),
      available: true,
      body: showEffects ? (
        <>
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
          <p className="mt-2 text-xs text-text-secondary">
            {t('colorV3.light', { defaultValue: 'Film and lens looks' })}
          </p>
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
        </>
      ) : (
        // Masks: glow and halation are local light; the rest describe the whole frame.
        <>
          {effectSlider('glow_amount', t('colorV3.glow', { defaultValue: 'Glow' }), 0, 100)}
          {effectSlider('halation_amount', t('colorV3.halation', { defaultValue: 'Halation' }), 0, 100)}
        </>
      ),
    },
    optics: {
      title: t('colorV3.optics', { defaultValue: 'Optics' }),
      modified: effects.ca_red_cyan !== 0 || effects.ca_blue_yellow !== 0 || !!adjustments.flatFieldProfile,
      available: showEffects,
      body: (
        <>
          <p className="text-xs text-text-secondary">{t('colorV3.lens', { defaultValue: 'Chromatic aberration' })}</p>
          {effectSlider('ca_red_cyan', t('colorV3.caRedCyan', { defaultValue: 'Red / cyan fringe' }), -100, 100)}
          {effectSlider(
            'ca_blue_yellow',
            t('colorV3.caBlueYellow', { defaultValue: 'Blue / yellow fringe' }),
            -100,
            100,
          )}
          <div className="mt-3">
            <FlatFieldControl
              adjustments={adjustments}
              setAdjustments={setAdjustments}
              onDragStateChange={onDragStateChange}
            />
          </div>
        </>
      ),
    },
    calibration: {
      title: t('colorV3.calibration', { defaultValue: 'Camera Calibration' }),
      modified: differs(calibration, defaultV3Calibration()),
      available: showEffects,
      body: (
        <>
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
        </>
      ),
    },
    look: {
      title: t('colorV3.look', { defaultValue: 'Look' }),
      modified: !!adjustments.lutPath,
      available: showEffects,
      body: (
        <V3LookSection
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          onDragStateChange={onDragStateChange}
        />
      ),
    },
    raw: {
      title: t('colorV3.raw', { defaultValue: 'RAW' }),
      modified: !!adjustments.v3Pipeline || adjustments.v3RawRecovery === 'off',
      available: showEffects && adjustments.processVersion === 3,
      body: <RawControls adjustments={adjustments} setAdjustments={setAdjustments} />,
    },
  };

  const shown = layout.sectionOrder.filter((id) => sections[id]?.available && !layout.hiddenSections.includes(id));
  const hidden = layout.sectionOrder.filter((id) => sections[id]?.available && layout.hiddenSections.includes(id));
  // Dropping a section on another puts it in that one's place.
  const onDragEnd = ({ active, over }: DragEndEvent) => {
    if (!over || active.id === over.id) return;
    const order = [...layout.sectionOrder];
    const from = order.indexOf(String(active.id));
    const to = order.indexOf(String(over.id));
    if (from < 0 || to < 0) return;
    order.splice(from, 1);
    order.splice(to, 0, String(active.id));
    updateLayout({ sectionOrder: order });
  };
  return (
    <div className="flex flex-col">
      {errorNotice}
      <DndContext sensors={sensors} onDragEnd={onDragEnd}>
        {shown.map((id) => (
          <MovableSection
            key={id}
            id={id}
            title={sections[id].title}
            modified={sections[id].modified}
            open={layout.openSections[id] ?? false}
            onToggle={() => updateLayout({ openSections: { ...layout.openSections, [id]: !layout.openSections[id] } })}
            onHide={() => updateLayout({ hiddenSections: [...layout.hiddenSections, id] })}
          >
            {sections[id].body}
          </MovableSection>
        ))}
      </DndContext>
      {hidden.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 py-3 text-xs text-text-secondary">
          <span>{t('colorV3.hiddenSections', { defaultValue: 'Hidden:' })}</span>
          {hidden.map((id) => (
            <button
              key={id}
              type="button"
              onClick={() => updateLayout({ hiddenSections: layout.hiddenSections.filter((h) => h !== id) })}
              className="rounded-full bg-surface px-2 py-0.5 text-text-primary hover:bg-card-active"
              data-tooltip={t('colorV3.showSection', { defaultValue: 'Show this section again' })}
            >
              + {sections[id].title}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
