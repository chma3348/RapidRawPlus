import { useState, useEffect, useMemo } from 'react';
import { Pipette, Sliders } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import ColorWheel from '../ui/ColorWheel';
import HueCurvesPanel from './HueCurves';
import { ColorAdjustment, ColorCalibration, HueSatLum, INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import { Adjustments, ColorGrading } from '../../utils/adjustments';
import { AppSettings } from '../ui/AppProperties';
import { useEditorStore } from '../../store/useEditorStore';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';

interface ColorProps {
  color: string;
  name: string;
  label: string;
}

interface ColorPanelProps {
  adjustments: Adjustments;
  setAdjustments(adjustments: Partial<Adjustments>): any;
  appSettings: AppSettings | null;
  isForMask?: boolean;
  isWbPickerActive?: boolean;
  toggleWbPicker?: () => void;
  isMixerPickerActive?: boolean;
  toggleMixerPicker?: () => void;
  onDragStateChange?: (isDragging: boolean) => void;
}

interface ColorSwatchProps {
  color: string;
  isActive: boolean;
  name: string;
  ariaLabel: string;
  onClick: (name: string) => void;
}

const ColorSwatch = ({ color, name, isActive, ariaLabel, onClick }: ColorSwatchProps) => {
  const [isPressed, setIsPressed] = useState(false);
  const [isHovered, setIsHovered] = useState(false);

  const handleMouseDown = () => {
    setIsPressed(true);
  };

  const handleMouseUp = () => {
    setIsPressed(false);
  };

  const handleMouseLeave = () => {
    setIsPressed(false);
    setIsHovered(false);
  };

  const handleMouseEnter = () => {
    setIsHovered(true);
  };

  const handleClick = () => {
    onClick(name);
  };

  const getTransform = () => {
    if (isPressed) return 'scale(0.95)';
    if (isActive) return 'scale(1.1)';
    if (isHovered) return 'scale(1.08)';
    return 'scale(1)';
  };

  return (
    <button
      aria-label={ariaLabel}
      className="relative w-6 h-6 focus:outline-hidden group"
      onClick={handleClick}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
      onMouseLeave={handleMouseLeave}
      onMouseEnter={handleMouseEnter}
      onTouchStart={handleMouseDown}
      onTouchEnd={handleMouseUp}
    >
      <div
        className={`absolute inset-0 rounded-full border-2 transition-all duration-200 ease-out ${
          isActive ? 'border-white opacity-100' : 'scale-100 border-transparent opacity-0'
        }`}
        style={{
          transform: isActive ? (isPressed ? 'scale(1.1)' : 'scale(1.25)') : undefined,
          transition: isPressed
            ? 'transform 100ms cubic-bezier(0.4, 0, 0.2, 1), opacity 200ms ease-out'
            : 'transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1), opacity 200ms ease-out',
        }}
      />

      <div
        className={`absolute inset-0 rounded-full transition-all duration-150 ease-out ${
          isActive ? 'shadow-lg' : 'shadow-md'
        }`}
        style={{
          backgroundColor: color,
          transform: getTransform(),
          transition: isPressed
            ? 'transform 100ms cubic-bezier(0.4, 0, 0.2, 1)'
            : 'transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1)',
        }}
      />
    </button>
  );
};

const LGG_WHEEL_KEYS = ['lift', 'gamma', 'gain', 'offset'] as const;

// Resolve-style primary corrector: four wheels whose chroma pushes a hue
// cast into a tonal band and whose luminance slider is the master control
// (lift = blacks, gamma = midtones, gain = highlights, offset = everything).
const ColorWheelsPanel = ({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) => {
  const { t } = useTranslation();
  const wheels = adjustments.colorWheels ?? INITIAL_ADJUSTMENTS.colorWheels;

  const handleChange = (key: string, newValue: HueSatLum) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorWheels: {
        ...(prev.colorWheels ?? INITIAL_ADJUSTMENTS.colorWheels),
        [key]: newValue,
      },
    }));
  };

  return (
    <div className="p-2 bg-bg-tertiary rounded-md">
      <Text variant={TextVariants.heading} className="mb-3">
        {t('adjustments.color.wheels.title')}
      </Text>
      <div className="grid grid-cols-2 gap-4">
        {LGG_WHEEL_KEYS.map((key) => (
          <div key={key} className="min-w-0">
            <ColorWheel
              defaultValue={{ hue: 0, saturation: 0, luminance: 0 }}
              label={
                key === 'lift'
                  ? t('adjustments.color.wheels.lift')
                  : key === 'gamma'
                    ? t('adjustments.color.wheels.gamma')
                    : key === 'gain'
                      ? t('adjustments.color.wheels.gain')
                      : t('adjustments.color.wheels.offset')
              }
              onChange={(val: HueSatLum) => handleChange(key, val)}
              value={wheels[key]}
              onDragStateChange={onDragStateChange}
            />
          </div>
        ))}
      </div>
    </div>
  );
};

const ColorGradingPanel = ({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) => {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<'3way' | 'global'>('3way');
  const [isExpanded, setIsExpanded] = useState(false);
  const colorGrading = adjustments.colorGrading || INITIAL_ADJUSTMENTS.colorGrading;

  const handleChange = (grading: ColorGrading, newValue: HueSatLum) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorGrading: {
        ...(prev.colorGrading || INITIAL_ADJUSTMENTS.colorGrading),
        [grading]: newValue,
      },
    }));
  };

  const handleColorGradingSliderChange = (grading: ColorGrading, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorGrading: {
        ...(prev.colorGrading || INITIAL_ADJUSTMENTS.colorGrading),
        [grading]: parseFloat(value),
      },
    }));
  };

  const tabs = useMemo(
    () => [
      {
        id: '3way',
        icon: (
          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
            <circle cx="12" cy="6" r="4.5" />
            <circle cx="5" cy="18" r="4.5" />
            <circle cx="19" cy="18" r="4.5" />
          </svg>
        ),
      },
      {
        id: 'global',
        icon: (
          <div className="w-3.5 h-3.5 rounded-full" style={{ background: 'linear-gradient(to top, #666, #fff)' }} />
        ),
      },
    ],
    [],
  );

  return (
    <div>
      <div className="flex items-center justify-start gap-2 mb-4 mt-2">
        {tabs.map((tab) => {
          const isActive = activeTab === tab.id;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id as '3way' | 'global')}
              className={`w-7 h-7 rounded-full flex items-center justify-center transition-all focus:outline-none
                ${
                  isActive
                    ? 'ring-2 ring-offset-2 ring-offset-surface ring-accent text-text-primary'
                    : 'bg-bg-secondary text-text-secondary hover:text-text-primary hover:bg-bg-secondary/80'
                }`}
            >
              {tab.icon}
            </button>
          );
        })}

        <div className="w-px h-5 bg-text-secondary/20 mx-1" />

        <button
          onClick={() => setIsExpanded(!isExpanded)}
          className={`w-7 h-7 rounded-full flex items-center justify-center transition-all focus:outline-none
            ${
              isExpanded
                ? 'bg-accent text-button-text'
                : 'bg-bg-secondary text-text-secondary hover:text-text-primary hover:bg-bg-secondary/80'
            }`}
          data-tooltip={t('adjustments.color.toggleSliders')}
        >
          <Sliders size={14} />
        </button>
      </div>

      <div className="relative w-full mb-4">
        <AnimatePresence mode="wait">
          {activeTab === '3way' ? (
            <motion.div
              key="3way"
              initial={{ opacity: 0, x: -15 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: -15 }}
              transition={{ duration: 0.2 }}
              className="w-full"
            >
              <div className="flex justify-center mb-4">
                <div className="w-[calc(50%-0.5rem)]">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.midtones}
                    label={t('adjustments.color.grading.midtones')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Midtones, val)}
                    value={colorGrading.midtones}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
              </div>
              <div className="flex justify-between mb-2 gap-4">
                <div className="w-full flex-1 min-w-0">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.shadows}
                    label={t('adjustments.color.grading.shadows')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Shadows, val)}
                    value={colorGrading.shadows}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
                <div className="w-full flex-1 min-w-0">
                  <ColorWheel
                    defaultValue={INITIAL_ADJUSTMENTS.colorGrading.highlights}
                    label={t('adjustments.color.grading.highlights')}
                    onChange={(val: HueSatLum) => handleChange(ColorGrading.Highlights, val)}
                    value={colorGrading.highlights}
                    onDragStateChange={onDragStateChange}
                    isExpanded={isExpanded}
                  />
                </div>
              </div>
            </motion.div>
          ) : (
            <motion.div
              key="global"
              initial={{ opacity: 0, x: 15 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: 15 }}
              transition={{ duration: 0.2 }}
              className="w-full flex justify-center pb-2"
            >
              <div className="w-full max-w-70">
                <ColorWheel
                  defaultValue={INITIAL_ADJUSTMENTS.colorGrading.global}
                  label={t('adjustments.color.grading.global')}
                  onChange={(val: HueSatLum) => handleChange(ColorGrading.Global, val)}
                  value={colorGrading.global || INITIAL_ADJUSTMENTS.colorGrading.global}
                  onDragStateChange={onDragStateChange}
                  isExpanded={isExpanded}
                />
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      <div>
        <Slider
          defaultValue={50}
          label={t('adjustments.color.grading.blending')}
          max={100}
          min={0}
          onChange={(e: any) => handleColorGradingSliderChange(ColorGrading.Blending, e.target.value)}
          step={1}
          value={colorGrading.blending}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          defaultValue={0}
          label={t('adjustments.color.grading.balance')}
          max={100}
          min={-100}
          onChange={(e: any) => handleColorGradingSliderChange(ColorGrading.Balance, e.target.value)}
          step={1}
          value={colorGrading.balance}
          onDragStateChange={onDragStateChange}
        />
      </div>
    </div>
  );
};

const ColorCalibrationPanel = ({ adjustments, setAdjustments, onDragStateChange }: ColorPanelProps) => {
  const { t } = useTranslation();
  const [activePrimary, setActivePrimary] = useState('red');
  const colorCalibration = adjustments.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration;

  const PRIMARY_COLORS = useMemo(
    () => [
      { name: 'red', color: '#f87171', label: t('adjustments.color.calibration.colors.red') },
      { name: 'green', color: '#4ade80', label: t('adjustments.color.calibration.colors.green') },
      { name: 'blue', color: '#60a5fa', label: t('adjustments.color.calibration.colors.blue') },
    ],
    [t],
  );

  const handleShadowsChange = (value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorCalibration: {
        ...(prev.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration),
        shadowsTint: parseFloat(value),
      },
    }));
  };

  const handlePrimaryChange = (key: 'Hue' | 'Saturation', value: string) => {
    const fullKey = `${activePrimary}${key}` as keyof ColorCalibration;
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      colorCalibration: {
        ...(prev.colorCalibration || INITIAL_ADJUSTMENTS.colorCalibration),
        [fullKey]: parseFloat(value),
      },
    }));
  };

  const currentValues = {
    hue: colorCalibration[`${activePrimary}Hue` as keyof ColorCalibration] || 0,
    saturation: colorCalibration[`${activePrimary}Saturation` as keyof ColorCalibration] || 0,
  };

  const trackSuffix = `${activePrimary}s`;

  return (
    <div className="p-2 bg-bg-tertiary rounded-md mt-4">
      <Text variant={TextVariants.heading} className="mb-2">
        {t('adjustments.color.calibration.title')}
      </Text>
      <div>
        <Text color={TextColors.primary} weight={TextWeights.medium} className="mb-1">
          {t('adjustments.color.calibration.shadows')}
        </Text>
        <Slider
          label={t('adjustments.color.calibration.tint')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={colorCalibration.shadowsTint}
          onChange={(e: any) => handleShadowsChange(e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName="tint-gradient-track"
        />
      </div>
      <div className="mt-3">
        <Text color={TextColors.primary} weight={TextWeights.medium} className="mb-3">
          {t('adjustments.color.calibration.primaries')}
        </Text>
        <div className="flex justify-center gap-6 mb-4 px-1">
          {PRIMARY_COLORS.map(({ name, color, label }) => (
            <ColorSwatch
              color={color}
              isActive={activePrimary === name}
              key={name}
              name={name}
              onClick={setActivePrimary}
              ariaLabel={t('adjustments.color.ariaSelectColor', { name: label })}
            />
          ))}
        </div>
        <Slider
          label={t('adjustments.color.calibration.hue')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={currentValues.hue}
          onChange={(e: any) => handlePrimaryChange('Hue', e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName={`hue-slider-${trackSuffix}`}
        />
        <Slider
          label={t('adjustments.color.calibration.saturation')}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          value={currentValues.saturation}
          onChange={(e: any) => handlePrimaryChange('Saturation', e.target.value)}
          onDragStateChange={onDragStateChange}
          trackClassName={`sat-slider-${trackSuffix}`}
        />
      </div>
    </div>
  );
};

export default function ColorPanel({
  adjustments,
  setAdjustments,
  appSettings,
  isForMask = false,
  isWbPickerActive = false,
  toggleWbPicker,
  isMixerPickerActive = false,
  toggleMixerPicker,
  onDragStateChange,
}: ColorPanelProps) {
  const { t } = useTranslation();
  const [activeColor, setActiveColor] = useState('reds');
  const adjustmentVisibility = appSettings?.adjustmentVisibility || {};
  const isWgpuEnabled = appSettings?.useWgpuRenderer !== false;

  const HSL_COLORS = useMemo<Array<ColorProps>>(
    () => [
      { name: 'reds', color: '#f87171', label: t('adjustments.color.mixerColors.reds') },
      { name: 'oranges', color: '#fb923c', label: t('adjustments.color.mixerColors.oranges') },
      { name: 'yellows', color: '#facc15', label: t('adjustments.color.mixerColors.yellows') },
      { name: 'greens', color: '#4ade80', label: t('adjustments.color.mixerColors.greens') },
      { name: 'aquas', color: '#2dd4bf', label: t('adjustments.color.mixerColors.aquas') },
      { name: 'blues', color: '#60a5fa', label: t('adjustments.color.mixerColors.blues') },
      { name: 'purples', color: '#a78bfa', label: t('adjustments.color.mixerColors.purples') },
      { name: 'magentas', color: '#f472b6', label: t('adjustments.color.mixerColors.magentas') },
    ],
    [t],
  );

  const colorHueMap = useMemo<Record<string, number>>(
    () => ({
      reds: 0,
      oranges: 30,
      yellows: 60,
      greens: 120,
      aquas: 180,
      blues: 240,
      purples: 300,
      magentas: 340,
    }),
    [],
  );

  const currentHsl = adjustments?.hsl?.[activeColor] || { hue: 0, saturation: 0, luminance: 0 };
  const baseHue = colorHueMap[activeColor] || 0;
  const effectiveHue = baseHue + (currentHsl.hue || 0);

  useEffect(() => {
    const normalizedHue = ((effectiveHue % 360) + 360) % 360;
    const effectiveSaturation = (currentHsl.saturation + 100) / 2;

    document.documentElement.style.setProperty(`--hsl-mixer-hue-${activeColor}`, normalizedHue.toString());
    document.documentElement.style.setProperty(`--hsl-mixer-sat-${activeColor}`, `${effectiveSaturation}%`);
  }, [effectiveHue, currentHsl.saturation, activeColor]);

  const handleAdjustmentChange = (key: ColorAdjustment, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({ ...prev, [key]: parseFloat(value) }));
  };

  const handleHslChange = (key: ColorAdjustment, value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      hsl: {
        ...(prev.hsl || {}),
        [activeColor]: {
          ...(prev.hsl?.[activeColor] || {}),
          [key]: parseFloat(value),
        },
      },
    }));
  };

  // Point Color: an eyedropper pick (sampled backend-side) becomes a chip
  // holding that EXACT color — its H/S/L sliders edit a narrow window
  // around the sampled hue, leaving presets and other colors untouched.
  const mixerPickedColor = useEditorStore((s) => s.mixerPickedColor);
  useEffect(() => {
    if (mixerPickedColor && !isForMask) {
      const priorLen = adjustments.pointColors?.length ?? 0;
      const newIdx = Math.min(priorLen, 3);
      setAdjustments((prev: Partial<Adjustments>) => {
        const list = [...(prev.pointColors ?? [])];
        if (list.length >= 4) list.shift();
        list.push({
          hue: mixerPickedColor.hue,
          sat: mixerPickedColor.sat,
          val: mixerPickedColor.val,
          range: 22,
          hueShift: 0,
          satShift: 0,
          lumShift: 0,
        });
        return { ...prev, pointColors: list };
      });
      setActiveColor(`point-${newIdx}`);
      useEditorStore.getState().setEditor({ mixerPickedColor: null });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mixerPickedColor, isForMask]);

  const pointColors = adjustments.pointColors ?? [];
  const pointIdx = activeColor.startsWith('point-') ? Number(activeColor.slice(6)) : -1;
  const activePoint = pointIdx >= 0 ? pointColors[pointIdx] : null;

  const handlePointChange = (key: 'hueShift' | 'satShift' | 'lumShift' | 'range', value: string) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      pointColors: (prev.pointColors ?? []).map((c, i) =>
        i === pointIdx ? { ...c, [key]: parseFloat(value) } : c,
      ),
    }));
  };

  const removePoint = (idx: number) => {
    setAdjustments((prev: Partial<Adjustments>) => ({
      ...prev,
      pointColors: (prev.pointColors ?? []).filter((_, i) => i !== idx),
    }));
    if (pointIdx === idx) setActiveColor('reds');
    else if (pointIdx > idx) setActiveColor(`point-${pointIdx - 1}`);
  };

  const hue_slider = `hue-slider-${activeColor}`;
  const saturation_slider = `sat-slider-${activeColor}`;
  const luminance_slider = `lum-slider-${activeColor}`;

  return (
    <div className="space-y-4">
      <div className="p-2 bg-bg-tertiary rounded-md">
        <div className="flex justify-between items-center mb-2">
          <Text variant={TextVariants.heading}>{t('adjustments.color.whiteBalance')}</Text>
          {!isForMask && toggleWbPicker && (
            <button
              onClick={toggleWbPicker}
              disabled={isWgpuEnabled}
              className={`p-1.5 rounded-md transition-colors ${
                isWgpuEnabled
                  ? 'cursor-not-allowed text-text-secondary hover:bg-transparent'
                  : isWbPickerActive
                    ? 'bg-accent text-button-text'
                    : 'hover:bg-bg-secondary text-text-secondary'
              }`}
              data-tooltip={
                isWgpuEnabled ? t('adjustments.color.wbPickerWgpuDisabled') : t('adjustments.color.wbPickerTooltip')
              }
            >
              <Pipette size={16} />
            </button>
          )}
        </div>
        <Slider
          label={t('adjustments.color.temperature')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Temperature, e.target.value)}
          step={1}
          value={adjustments.temperature || 0}
          trackClassName="temperature-gradient-track"
          onDragStateChange={onDragStateChange}
        />
        <Slider
          label={t('adjustments.color.tint')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Tint, e.target.value)}
          step={1}
          value={adjustments.tint || 0}
          trackClassName="tint-gradient-track"
          onDragStateChange={onDragStateChange}
        />
      </div>

      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-2">
          {t('adjustments.color.presence')}
        </Text>
        <Slider
          label={t('adjustments.color.vibrance')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Vibrance, e.target.value)}
          step={1}
          value={adjustments.vibrance || 0}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          label={t('adjustments.color.saturation')}
          max={100}
          min={-100}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Saturation, e.target.value)}
          step={1}
          value={adjustments.saturation || 0}
          onDragStateChange={onDragStateChange}
        />
      </div>

      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-2">
          {isForMask ? t('adjustments.color.localHue') : t('adjustments.color.hue')}
        </Text>
        <Slider
          label={t('adjustments.color.hue')}
          max={180}
          min={-180}
          onChange={(e: any) => handleAdjustmentChange(ColorAdjustment.Hue, e.target.value)}
          step={1}
          value={adjustments.hue || 0}
          trackClassName="hue-range-track"
          onDragStateChange={onDragStateChange}
        />
      </div>

      <div className="p-2 bg-bg-tertiary rounded-md">
        <Text variant={TextVariants.heading} className="mb-3">
          {t('adjustments.color.colorGrading')}
        </Text>
        <ColorGradingPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          appSettings={appSettings}
          onDragStateChange={onDragStateChange}
        />
      </div>

      {adjustmentVisibility.colorWheels !== false && (
        <ColorWheelsPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          appSettings={appSettings}
          onDragStateChange={onDragStateChange}
        />
      )}

      <div className="p-2 bg-bg-tertiary rounded-md">
        <div className="flex justify-between items-center mb-3">
          <Text variant={TextVariants.heading}>{t('adjustments.color.colorMixer')}</Text>
          {!isForMask && toggleMixerPicker && (
            <button
              onClick={toggleMixerPicker}
              className={`p-1.5 rounded-md transition-colors ${
                isMixerPickerActive
                  ? 'bg-accent text-button-text'
                  : 'hover:bg-bg-secondary text-text-secondary'
              }`}
              data-tooltip={t('adjustments.color.mixerPickerTooltip')}
            >
              <Pipette size={16} />
            </button>
          )}
        </div>
        <div className="flex justify-between mb-4 px-1">
          {HSL_COLORS.map(({ name, color, label }) => (
            <ColorSwatch
              color={color}
              isActive={activeColor === name}
              key={name}
              name={name}
              onClick={setActiveColor}
              ariaLabel={t('adjustments.color.ariaSelectColor', { name: label })}
            />
          ))}
        </div>
        {!isForMask && pointColors.length > 0 && (
          <div className="flex items-center gap-3 mb-4 px-1">
            {pointColors.map((c, i) => (
              <div key={i} className="relative">
                <ColorSwatch
                  color={`hsl(${Math.round(c.hue)}, ${Math.round(Math.min(1, c.sat) * 100)}%, ${Math.round(
                    Math.max(0.12, Math.min(0.88, c.val * (1 - c.sat / 2))) * 100,
                  )}%)`}
                  isActive={activeColor === `point-${i}`}
                  name={`point-${i}`}
                  onClick={setActiveColor}
                  ariaLabel={t('adjustments.color.pointColor')}
                />
                <button
                  onClick={() => removePoint(i)}
                  className="absolute -top-1.5 -right-1.5 w-3.5 h-3.5 rounded-full bg-bg-primary text-text-secondary hover:text-text-primary text-[9px] leading-none flex items-center justify-center border border-surface"
                  aria-label={t('adjustments.color.removePointColor')}
                >
                  ×
                </button>
              </div>
            ))}
            <Text variant={TextVariants.small} className="opacity-50">
              {t('adjustments.color.pointColor')}
            </Text>
          </div>
        )}
        <Slider
          label={t('adjustments.color.hue')}
          max={100}
          min={-100}
          onChange={(e: any) =>
            activePoint ? handlePointChange('hueShift', e.target.value) : handleHslChange(ColorAdjustment.Hue, e.target.value)
          }
          step={1}
          value={activePoint ? activePoint.hueShift : currentHsl.hue}
          trackClassName={activePoint ? undefined : hue_slider}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          label={t('adjustments.color.saturation')}
          max={100}
          min={-100}
          onChange={(e: any) =>
            activePoint
              ? handlePointChange('satShift', e.target.value)
              : handleHslChange(ColorAdjustment.Saturation, e.target.value)
          }
          step={1}
          value={activePoint ? activePoint.satShift : currentHsl.saturation}
          trackClassName={activePoint ? undefined : saturation_slider}
          onDragStateChange={onDragStateChange}
        />
        <Slider
          label={t('adjustments.color.luminance')}
          max={100}
          min={-100}
          onChange={(e: any) =>
            activePoint
              ? handlePointChange('lumShift', e.target.value)
              : handleHslChange(ColorAdjustment.Luminance, e.target.value)
          }
          step={1}
          value={activePoint ? activePoint.lumShift : currentHsl.luminance}
          trackClassName={activePoint ? undefined : luminance_slider}
          onDragStateChange={onDragStateChange}
        />
        {activePoint && (
          <Slider
            label={t('adjustments.color.pointRange')}
            max={60}
            min={5}
            onChange={(e: any) => handlePointChange('range', e.target.value)}
            step={1}
            defaultValue={22}
            value={activePoint.range ?? 22}
            fillOrigin="min"
            onDragStateChange={onDragStateChange}
          />
        )}
      </div>

      {!isForMask && adjustmentVisibility.hueCurves !== false && (
        <HueCurvesPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          onDragStateChange={onDragStateChange}
        />
      )}

      {!isForMask && adjustmentVisibility.colorCalibration !== false && (
        <ColorCalibrationPanel
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          appSettings={appSettings}
          onDragStateChange={onDragStateChange}
        />
      )}
    </div>
  );
}
