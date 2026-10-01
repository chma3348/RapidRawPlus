import React, { useCallback } from 'react';
import { RotateCcw, Aperture, ChartArea } from 'lucide-react';
import { motion, AnimatePresence } from 'framer-motion';
import clsx from 'clsx';
import { useTranslation } from 'react-i18next';
import ColorV3Controls from '../../adjustments/ColorV3';
import { useEditorLayout } from '../../../hooks/useEditorLayout';
import Waveform from '../editor/Waveform';
import Resizer from '../../ui/Resizer';
import { Adjustments, SectionVisibility, INITIAL_ADJUSTMENTS, ADJUSTMENT_SECTIONS } from '../../../utils/adjustments';
import { Orientation } from '../../ui/AppProperties';
import Text from '../../ui/Text';
import { TextVariants } from '../../../types/typography';
import { useShallow } from 'zustand/react/shallow';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { useEditorActions } from '../../../hooks/useEditorActions';
import { useWaveformControls } from '../../../hooks/useWaveformControls';

export default function Controls() {
  const { t } = useTranslation();
  const { isResizingWaveform, onToggleWaveform, setActiveWaveformChannel, handleWaveformResize } =
    useWaveformControls();
  const { setAdjustments, handleAutoAdjustments, handleLutSelect, setLutPreviewOverride } = useEditorActions();

  const { appSettings, theme } = useSettingsStore(
    useShallow((state) => ({
      appSettings: state.appSettings,
      theme: state.theme,
    })),
  );

  const {
    adjustments,
    copiedSectionAdjustments,
    histogram,
    selectedImage,
    isWbPickerActive,
    isMixerPickerActive,
    isWaveformVisible,
    waveform,
    activeWaveformChannel,
    waveformHeight,
    setEditor,
  } = useEditorStore(
    useShallow((state) => ({
      adjustments: state.adjustments,
      copiedSectionAdjustments: state.copiedSectionAdjustments,
      histogram: state.histogram,
      selectedImage: state.selectedImage,
      isWbPickerActive: state.isWbPickerActive,
      isMixerPickerActive: state.isMixerPickerActive,
      isWaveformVisible: state.isWaveformVisible,
      waveform: state.waveform,
      activeWaveformChannel: state.activeWaveformChannel,
      waveformHeight: state.waveformHeight,
      setEditor: state.setEditor,
    })),
  );

  const [layout, updateLayout] = useEditorLayout();
  const mode = layout.adjustmentsMode;

  const toggleWbPicker = useCallback(
    () => setEditor((state) => ({ isWbPickerActive: !state.isWbPickerActive })),
    [setEditor],
  );

  const onDragStateChange = useCallback(
    (isDragging: boolean) => setEditor({ isSliderDragging: isDragging }),
    [setEditor],
  );

  const handleResetAdjustments = () => {
    setAdjustments((prev: Adjustments) => ({
      ...prev,
      ...Object.keys(ADJUSTMENT_SECTIONS)
        .flatMap((s) => ADJUSTMENT_SECTIONS[s])
        .reduce((acc: any, key: string) => {
          acc[key] = INITIAL_ADJUSTMENTS[key as keyof Adjustments];
          return acc;
        }, {}),
      sectionVisibility: { ...INITIAL_ADJUSTMENTS.sectionVisibility },
    }));
  };

  return (
    <div className="flex flex-col h-full">
      <div className="p-4 flex justify-between items-center shrink-0 border-b border-surface">
        <Text variant={TextVariants.title}>{t('editor.adjustments.title')}</Text>
        <div className="flex items-center gap-1">
          <button
            className="p-2 rounded-full hover:bg-surface disabled:cursor-not-allowed transition-colors"
            disabled={!selectedImage?.isReady}
            onClick={handleAutoAdjustments}
            data-tooltip={t('editor.adjustments.tooltips.autoAdjust')}
          >
            <Aperture size={18} />
          </button>
          <button
            className={clsx(
              'p-2 rounded-full transition-colors',
              isWaveformVisible ? 'bg-surface hover:bg-card-active' : 'hover:bg-surface',
            )}
            onClick={onToggleWaveform}
            data-tooltip={t('editor.adjustments.tooltips.toggleAnalytics')}
          >
            <ChartArea size={18} />
          </button>
          <button
            className="p-2 rounded-full hover:bg-surface disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            disabled={!selectedImage}
            onClick={handleResetAdjustments}
            data-tooltip={t('editor.adjustments.tooltips.resetAdjustments')}
          >
            <RotateCcw size={18} />
          </button>
        </div>
      </div>

      <AnimatePresence initial={false}>
        {isWaveformVisible && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: waveformHeight || 256, opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: isResizingWaveform ? 0 : 0.2, ease: 'easeOut' }}
            className="shrink-0 flex flex-col relative border-b border-surface overflow-hidden"
          >
            <div className="grow w-full h-full p-4 pb-2 min-h-0">
              <Waveform
                waveformData={waveform || null}
                histogram={histogram}
                displayMode={activeWaveformChannel || 'luma'}
                setDisplayMode={setActiveWaveformChannel}
                showClipping={adjustments.showClipping || false}
                onToggleClipping={() => {
                  setAdjustments((prev: Adjustments) => ({
                    ...prev,
                    showClipping: !prev.showClipping,
                  }));
                }}
                theme={theme}
              />
            </div>
            <Resizer direction={Orientation.Horizontal} onMouseDown={handleWaveformResize} />
          </motion.div>
        )}
      </AnimatePresence>

      {/* Basic: the essentials, airy. Advanced: every control, in sections. */}
      <div className="shrink-0 px-4 pt-3">
        <div className="flex rounded-lg bg-surface p-0.5" role="tablist">
          {(['basic', 'advanced'] as const).map((m) => (
            <button
              key={m}
              type="button"
              role="tab"
              aria-selected={mode === m}
              onClick={() => updateLayout({ adjustmentsMode: m })}
              className={clsx(
                'flex-1 rounded-md py-1.5 text-xs font-medium transition-colors',
                mode === m
                  ? 'bg-card-active text-text-primary shadow-sm'
                  : 'text-text-secondary hover:text-text-primary',
              )}
            >
              {m === 'basic'
                ? t('editor.adjustments.modeBasic', { defaultValue: 'Basic' })
                : t('editor.adjustments.modeAdvanced', { defaultValue: 'Advanced' })}
            </button>
          ))}
        </div>
      </div>

      <div className={clsx('grow overflow-y-auto px-4 flex flex-col', mode === 'basic' ? 'py-4' : 'py-1')}>
        <ColorV3Controls
          adjustments={adjustments}
          setAdjustments={setAdjustments}
          onDragStateChange={onDragStateChange}
          isWbPickerActive={isWbPickerActive}
          toggleWbPicker={toggleWbPicker}
          mode={mode}
          onAuto={handleAutoAdjustments}
        />
      </div>
    </div>
  );
}
