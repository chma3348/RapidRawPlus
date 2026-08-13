import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { CheckCircle, XCircle, Loader2, Save, RefreshCw, Sparkles, Layers } from 'lucide-react';
import { motion } from 'framer-motion';
import Button from '../ui/Button';
import { Invokes } from '../ui/AppProperties';
import Dropdown from '../ui/Dropdown';
import ModelPicker, { ModelTaskType } from '../ui/ModelPicker';
import Slider from '../ui/Slider';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';
import { useEditorStore } from '../../store/useEditorStore';
import { ImageCompare } from './DenoiseModal';

interface PreviewData {
  original: string;
  enhanced: string;
  scale: number;
}

/** Zoomed crop compare: left of the divider is the original, right is the
 * model output blended at the current strength (opacity ≈ blend). */
const PreviewCompare = ({ original, enhanced, strength }: { original: string; enhanced: string; strength: number }) => {
  const { t } = useTranslation();
  const [pos, setPos] = useState(50);
  return (
    <div className="w-full max-w-[400px]">
      <div className="relative aspect-square rounded-lg overflow-hidden border border-border-color bg-black select-none">
        <img src={original} alt="" className="absolute inset-0 w-full h-full object-cover" draggable={false} />
        <div className="absolute inset-0" style={{ clipPath: `inset(0 0 0 ${pos}%)` }}>
          <img src={original} alt="" className="absolute inset-0 w-full h-full object-cover" draggable={false} />
          <img
            src={enhanced}
            alt=""
            className="absolute inset-0 w-full h-full object-cover"
            style={{ opacity: strength / 100 }}
            draggable={false}
          />
        </div>
        <div className="absolute top-0 bottom-0 w-0.5 bg-white/80 pointer-events-none" style={{ left: `${pos}%` }} />
        <Text
          as="div"
          variant={TextVariants.small}
          color={TextColors.white}
          className="absolute top-2 left-2 bg-black/60 px-2 py-0.5 rounded-sm pointer-events-none"
        >
          {t('modals.enhance.previewOriginal')}
        </Text>
        <Text
          as="div"
          variant={TextVariants.small}
          color={TextColors.white}
          className="absolute top-2 right-2 bg-black/60 px-2 py-0.5 rounded-sm pointer-events-none"
        >
          {t('modals.enhance.previewEnhanced')}
        </Text>
      </div>
      <input
        type="range"
        min={0}
        max={100}
        value={pos}
        onChange={(e) => setPos(Number(e.target.value))}
        className="w-full mt-2"
      />
    </div>
  );
};

interface EnhanceModalProps {
  isOpen: boolean;
  onClose(): void;
  onEnhance(strength: number, outputScale: number, chainStep: number, texture: number, grain: number): void;
  onSave(): Promise<string>;
  onOpenFile(path: string): void;
  error: string | null;
  previewBase64: string | null;
  originalBase64: string | null;
  isProcessing: boolean;
  progressMessage: string | null;
  task: 'upscale' | 'deblur' | 'restore';
  loadingImageUrl?: string | null;
  targetPaths: string[];
  resultDims?: { width: number; height: number } | null;
}

export default function EnhanceModal({
  isOpen,
  onClose,
  onEnhance,
  onSave,
  onOpenFile,
  error,
  previewBase64,
  originalBase64,
  isProcessing,
  progressMessage,
  task,
  loadingImageUrl,
  targetPaths,
  resultDims,
}: EnhanceModalProps) {
  const { t } = useTranslation();
  const { appSettings, handleSettingsChange } = useSettingsStore();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Remembered across dialog opens: snapping back to the default made it
  // easy to re-run at the same strength by accident.
  const [strength, setStrength] = useState(() => {
    const saved = Number(localStorage.getItem('rapidraw-enhance-strength'));
    return Number.isFinite(saved) && saved >= 10 && saved <= 100 ? saved : 70;
  });
  // Authentic-texture blend: how much of the ORIGINAL's fine detail layer
  // (micro-texture, grain) survives over the model output.
  const [texture, setTexture] = useState(() => {
    const saved = Number(localStorage.getItem('rapidraw-enhance-texture'));
    return Number.isFinite(saved) && saved >= 0 && saved <= 100 ? saved : 40;
  });
  const [grain, setGrain] = useState(() => {
    const saved = Number(localStorage.getItem('rapidraw-enhance-grain'));
    return Number.isFinite(saved) && saved >= 0 && saved <= 100 ? saved : 50;
  });
  const [outputScale, setOutputScale] = useState(2);
  const [previewData, setPreviewData] = useState<PreviewData | null>(null);
  const [isPreviewing, setIsPreviewing] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [overview, setOverview] = useState<string | null>(null);
  const [marquee, setMarquee] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [chainStep, setChainStep] = useState(0);
  const [chainSource, setChainSource] = useState<string | null>(null);
  const dragStart = useRef<{ x: number; y: number } | null>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);

  const taskType =
    task === 'deblur' ? ModelTaskType.Deblur : task === 'restore' ? ModelTaskType.Restore : ModelTaskType.Upscale;
  const preferredModelId = appSettings?.preferredModels?.[task] ?? null;

  useEffect(() => {
    if (isOpen && targetPaths.length > 0) {
      // The overview comes from the enhancement engine itself, so click
      // coordinates always match what gets processed (the app thumbnail
      // shows the *edited* photo and would be misaligned after a crop).
      const { selectedImage, adjustments } = useEditorStore.getState();
      invoke(Invokes.GetEnhancementOverview, {
        path: targetPaths[0],
        jsAdjustments: selectedImage?.path === targetPaths[0] ? adjustments : null,
      })
        .then((res: any) => setOverview(res?.overview ?? null))
        .catch(() => setOverview(null));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, targetPaths[0]]);

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    } else {
      setShow(false);
      const timer = setTimeout(() => {
        setIsMounted(false);
        setSavedPath(null);
        setSaveError(null);
        setIsSaving(false);
        setPreviewData(null);
        setPreviewError(null);
        setIsPreviewing(false);
        setOverview(null);
        setMarquee(null);
        setChainStep(0);
        setChainSource(null);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  const handleClose = useCallback(() => {
    if (isSaving) return;
    onClose();
  }, [onClose, isSaving]);

  const handleBackdropMouseDown = (e: React.MouseEvent) => {
    mouseDownTarget.current = e.target;
  };

  const handleBackdropClick = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget && mouseDownTarget.current === e.currentTarget) {
      handleClose();
    }
    mouseDownTarget.current = null;
  };

  const handleModelChange = (modelId: string) => {
    if (!appSettings) return;
    // A stale preview from the previous model would be misleading.
    setPreviewData(null);
    handleSettingsChange({
      ...appSettings,
      preferredModels: { ...(appSettings.preferredModels || {}), [task]: modelId },
    });
  };

  // Any settings change means what is on screen no longer matches the
  // saved file, so drop the success/error notice.
  const markDirty = () => {
    setSavedPath(null);
    setSaveError(null);
  };

  const handleRun = () => {
    setSavedPath(null);
    setSaveError(null);
    onEnhance(strength / 100, task === 'upscale' ? outputScale : 1, chainStep, texture / 100, grain / 100);
  };

  // Promote the current result to the working input and switch task —
  // restore → upscale etc. compose without saving intermediate files.
  const handleContinueWith = (nextTask: 'upscale' | 'deblur' | 'restore') => {
    setChainStep((s) => s + 1);
    setChainSource(previewBase64);
    setPreviewData(null);
    setSavedPath(null);
    useUIStore.getState().setUI((state) => ({
      enhanceModalState: {
        ...state.enhanceModalState,
        task: nextTask,
        previewBase64: null,
        resultDims: null,
      },
    }));
  };

  const runPreview = async (centerX: number, centerY: number, regionSize?: number) => {
    if (isPreviewing || targetPaths.length === 0) return;
    setIsPreviewing(true);
    setPreviewError(null);
    try {
      const { selectedImage, adjustments } = useEditorStore.getState();
      const data: PreviewData = await invoke(Invokes.PreviewEnhancement, {
        path: targetPaths[0],
        task,
        centerX,
        centerY,
        regionSize: regionSize ?? null,
        jsAdjustments: selectedImage?.path === targetPaths[0] ? adjustments : null,
      });
      setPreviewData(data);
    } catch (err) {
      setPreviewError(String(err));
    } finally {
      setIsPreviewing(false);
    }
  };

  const overviewImgRef = useRef<HTMLImageElement>(null);

  /** Maps a point in the overview element's box to normalized image
   * coordinates, accounting for object-contain letterboxing. */
  const mapToImage = (px: number, py: number): { x: number; y: number } | null => {
    const img = overviewImgRef.current;
    if (!img || !img.naturalWidth) return null;
    const rect = img.getBoundingClientRect();
    const scale = Math.min(rect.width / img.naturalWidth, rect.height / img.naturalHeight);
    const dispW = img.naturalWidth * scale;
    const dispH = img.naturalHeight * scale;
    const offX = (rect.width - dispW) / 2;
    const offY = (rect.height - dispH) / 2;
    const x = (px - offX) / dispW;
    const y = (py - offY) / dispH;
    if (x < 0 || x > 1 || y < 0 || y > 1) return null;
    return { x, y };
  };

  const handleOverviewMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    dragStart.current = { x, y };
    setMarquee({ x0: x, y0: y, x1: x, y1: y });
  };

  const handleOverviewMouseMove = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!dragStart.current) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    const s = dragStart.current;
    // Selection is a square: side = the larger drag axis.
    const side = Math.max(Math.abs(cx - s.x), Math.abs(cy - s.y));
    setMarquee({
      x0: s.x,
      y0: s.y,
      x1: s.x + Math.sign(cx - s.x || 1) * side,
      y1: s.y + Math.sign(cy - s.y || 1) * side,
    });
  };

  const handleOverviewMouseUp = () => {
    const s = dragStart.current;
    const m = marquee;
    dragStart.current = null;
    setMarquee(null);
    if (!s || !m) return;
    const img = overviewImgRef.current;
    if (!img) return;
    const rect = img.getBoundingClientRect();
    const sidePx = Math.abs(m.x1 - m.x0);
    const centerPx = { x: (m.x0 + m.x1) / 2, y: (m.y0 + m.y1) / 2 };
    const pt = mapToImage(centerPx.x, centerPx.y) ?? mapToImage(s.x, s.y);
    if (!pt) return;
    if (sidePx < 8) {
      // Plain click: default-size region at that spot.
      runPreview(pt.x, pt.y);
    } else {
      const scale = Math.min(rect.width / img.naturalWidth, rect.height / img.naturalHeight);
      const dispMin = Math.min(img.naturalWidth, img.naturalHeight) * scale;
      runPreview(pt.x, pt.y, sidePx / dispMin);
    }
  };

  const handleSave = async () => {
    setIsSaving(true);
    setSaveError(null);
    try {
      const path = await onSave();
      setSavedPath(path);
    } catch (e) {
      // Surfacing this matters: a silent failure here is indistinguishable
      // from "the button did nothing".
      setSaveError(String(e));
    } finally {
      setIsSaving(false);
    }
  };

  const handleOpen = () => {
    if (savedPath) {
      onOpenFile(savedPath);
      handleClose();
    }
  };

  const renderContent = () => {
    if (error) {
      return (
        <div className="flex flex-col items-center justify-center py-10 h-[460px]">
          <div className="flex items-center justify-center mb-6">
            <XCircle className="w-12 h-12 text-red-500" />
          </div>
          <Text variant={TextVariants.title} className="mb-2 text-center">
            {t('modals.enhance.processingFailed')}
          </Text>
          <Text className="text-center p-4 rounded-lg bg-bg-primary max-w-md mt-2 leading-relaxed">
            {String(error)}
          </Text>
        </div>
      );
    }

    if (previewBase64 && originalBase64 && !isProcessing) {
      return (
        <div className="w-full h-[500px] relative">
          <ImageCompare original={originalBase64} denoised={previewBase64} />
          {resultDims && (
            <div className="absolute top-2 right-2 pointer-events-none">
              <Text
                as="span"
                variant={TextVariants.small}
                color={TextColors.white}
                className="bg-black/60 px-2 py-1 rounded-md font-mono"
              >
                {resultDims.width} × {resultDims.height} px
              </Text>
            </div>
          )}
          {!savedPath && !isProcessing && (
            <div className="absolute bottom-2 left-1/2 -translate-x-1/2 flex items-center gap-2 bg-black/60 px-3 py-1.5 rounded-md">
              <Layers className="w-4 h-4 text-white/80" />
              <Text as="span" variant={TextVariants.small} color={TextColors.white}>
                {t('modals.enhance.continueWith')}
              </Text>
              {(['upscale', 'deblur', 'restore'] as const).map((next) => (
                <button
                  key={next}
                  onClick={() => handleContinueWith(next)}
                  className="px-2 py-0.5 rounded text-xs text-white bg-white/10 hover:bg-white/25 transition-colors"
                >
                  {t(`contextMenus.editor.${next}`)}
                </button>
              ))}
            </div>
          )}
          {savedPath && (
            <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.3 }}>
              <Text
                as="div"
                variant={TextVariants.heading}
                color={TextColors.success}
                className="flex items-center justify-center gap-2 mt-4"
              >
                <CheckCircle className="w-5 h-5" />
                <span>{t('modals.enhance.saveSuccess')}</span>
              </Text>
            </motion.div>
          )}
          {saveError && (
            <Text
              as="div"
              variant={TextVariants.small}
              color={TextColors.error}
              className="flex items-center justify-center gap-2 mt-4 text-center px-4"
            >
              <XCircle className="w-4 h-4 shrink-0" />
              <span>{saveError}</span>
            </Text>
          )}
        </div>
      );
    }

    if (isProcessing) {
      return (
        <div className="flex h-[460px] overflow-hidden rounded-lg border border-surface">
          <div className="w-2/5 relative overflow-hidden shrink-0 bg-[#0a0a0a] flex items-center justify-center">
            {loadingImageUrl ? (
              <img src={loadingImageUrl} alt="Selected preview" className="w-full h-full object-cover" />
            ) : (
              <div className="w-full h-full bg-surface/50" />
            )}
          </div>
          <div className="flex-1 flex flex-col items-center justify-center px-12 bg-bg-primary">
            <motion.div
              initial={{ opacity: 0, y: 20 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: 0.1, duration: 0.4 }}
              className="flex flex-col items-center w-full"
            >
              <Text variant={TextVariants.title} className="mb-2 text-center">
                {task === 'deblur'
                  ? t('modals.enhance.deblurProgress')
                  : task === 'restore'
                    ? t('modals.enhance.restoreProgress')
                    : t('modals.enhance.upscaleProgress')}
              </Text>
              <Text className="text-center font-mono h-6 flex justify-center items-center">
                {progressMessage || t('modals.enhance.initializing')}
              </Text>
              <div className="mt-8 w-64 relative">
                <div className="h-1 bg-surface rounded-full overflow-hidden relative w-full shadow-xs">
                  <motion.div
                    className="absolute inset-y-0 w-[80%] bg-linear-to-r from-transparent via-accent to-transparent mix-blend-screen"
                    style={{ filter: 'blur(3px)' }}
                    animate={{ x: ['-150%', '150%'] }}
                    transition={{ repeat: Infinity, duration: 1.5, ease: [0.4, 0, 0.2, 1] }}
                  />
                  <motion.div
                    className="absolute inset-y-0 w-[40%] bg-linear-to-r from-transparent via-white/90 to-transparent"
                    style={{ filter: 'blur(1px)' }}
                    animate={{ x: ['-250%', '250%'] }}
                    transition={{ repeat: Infinity, duration: 1.5, ease: [0.4, 0, 0.2, 1] }}
                  />
                </div>
              </div>
              <Text variant={TextVariants.small} className="mt-6 text-center max-w-xs opacity-60">
                {t('modals.enhance.speedNotice')}
              </Text>
            </motion.div>
          </div>
        </div>
      );
    }

    return (
      <div className="flex h-[460px] overflow-hidden rounded-lg border border-surface">
        <div
          className="w-2/5 relative overflow-hidden shrink-0 bg-[#0a0a0a] flex items-center justify-center select-none"
          onMouseDown={overview && chainStep === 0 ? handleOverviewMouseDown : undefined}
          onMouseMove={overview && chainStep === 0 ? handleOverviewMouseMove : undefined}
          onMouseUp={overview && chainStep === 0 ? handleOverviewMouseUp : undefined}
          onMouseLeave={() => {
            dragStart.current = null;
            setMarquee(null);
          }}
        >
          {chainStep > 0 && chainSource ? (
            <>
              <img src={chainSource} alt="" className="w-full h-full object-contain" draggable={false} />
              <div className="absolute bottom-2 inset-x-2 text-center pointer-events-none">
                <Text
                  as="span"
                  variant={TextVariants.small}
                  color={TextColors.white}
                  className="bg-black/60 px-2 py-1 rounded-md"
                >
                  {t('modals.enhance.chainNote')}
                </Text>
              </div>
            </>
          ) : overview || loadingImageUrl ? (
            <>
              <img
                ref={overviewImgRef}
                src={overview ?? loadingImageUrl ?? undefined}
                alt=""
                className="w-full h-full object-contain cursor-crosshair"
                draggable={false}
              />
              {marquee && (
                <div
                  className="absolute border-2 border-accent bg-accent/20 pointer-events-none"
                  style={{
                    left: Math.min(marquee.x0, marquee.x1),
                    top: Math.min(marquee.y0, marquee.y1),
                    width: Math.abs(marquee.x1 - marquee.x0),
                    height: Math.abs(marquee.y1 - marquee.y0),
                  }}
                />
              )}
              <div className="absolute bottom-2 inset-x-2 text-center pointer-events-none">
                <Text
                  as="span"
                  variant={TextVariants.small}
                  color={TextColors.white}
                  className="bg-black/60 px-2 py-1 rounded-md"
                >
                  {t('modals.enhance.dragToPreview')}
                </Text>
              </div>
            </>
          ) : (
            <div className="w-full h-full bg-surface/50" />
          )}
        </div>
        <div className="flex-1 flex flex-col items-center justify-center px-8 bg-bg-primary overflow-hidden">
          {isPreviewing ? (
            <div className="flex flex-col items-center gap-3">
              <Loader2 className="animate-spin text-accent" size={28} />
              <Text variant={TextVariants.small}>{t('modals.enhance.previewRunning')}</Text>
            </div>
          ) : previewData ? (
            <>
              <PreviewCompare
                original={previewData.original}
                enhanced={previewData.enhanced}
                strength={strength}
              />
              <Text variant={TextVariants.small} className="mt-2 text-center opacity-70">
                {t('modals.enhance.previewNote')}
              </Text>
            </>
          ) : (
            <>
              <Sparkles className="w-10 h-10 text-accent mb-4" />
              <Text variant={TextVariants.title} className="mb-3 text-center">
                {task === 'deblur'
                  ? t('modals.enhance.titleDeblur')
                  : task === 'restore'
                    ? t('modals.enhance.titleRestore')
                    : t('modals.enhance.titleUpscale')}
              </Text>
              <Text className="text-center max-w-md leading-relaxed">
                {task === 'deblur'
                  ? t('modals.enhance.descriptionDeblur')
                  : task === 'restore'
                    ? t('modals.enhance.descriptionRestore')
                    : t('modals.enhance.descriptionUpscale')}
              </Text>
            </>
          )}
          {previewError && (
            <Text variant={TextVariants.small} color={TextColors.error} className="mt-3 text-center">
              {previewError}
            </Text>
          )}
        </div>
      </div>
    );
  };

  const renderButtons = () => {
    // The controls stay on screen even after saving: tweaking strength and
    // re-running in place is the normal way to dial a result in, and
    // retries re-blend from the cached model output in a second or two.
    const disabled = isProcessing || isSaving;

    return (
      <div className={`w-full flex items-center gap-4 ${disabled ? 'opacity-50 pointer-events-none' : ''}`}>
        <div className="flex-1 flex flex-col gap-1">
          <div className="flex items-center gap-6">
            <div className="flex flex-col gap-1 w-[240px] mt-2 shrink-0">
              <Text variant={TextVariants.body} weight={TextWeights.medium}>
                {t('modelRegistry.model')}
              </Text>
              <ModelPicker taskType={taskType} value={preferredModelId} onChange={handleModelChange} />
            </div>
            {task === 'upscale' && (
              <div className="flex flex-col gap-1 w-[110px] mt-2 shrink-0">
                <Text variant={TextVariants.body} weight={TextWeights.medium}>
                  {t('modals.enhance.outputSizeLabel')}
                </Text>
                <Dropdown
                  options={[
                    { label: '2x', value: 2 },
                    { label: '4x', value: 4 },
                  ]}
                  value={outputScale}
                  onChange={(v: number) => {
                    setOutputScale(v);
                    markDirty();
                  }}
                />
              </div>
            )}
          </div>
          <div className="flex items-center gap-4">
            <div className="flex-1 min-w-0">
              <Slider
                label={t('modals.enhance.strengthLabel')}
                value={strength}
                min={10}
                max={100}
                step={5}
                defaultValue={70}
                onChange={(e: any) => {
                  const v = Number(e.target.value);
                  setStrength(v);
                  localStorage.setItem('rapidraw-enhance-strength', String(v));
                  markDirty();
                }}
                trackClassName="bg-bg-secondary"
                fillOrigin="min"
              />
            </div>
            <div className="flex-1 min-w-0" data-tooltip={t('modals.enhance.textureTooltip')}>
              <Slider
                label={t('modals.enhance.textureLabel')}
                value={texture}
                min={0}
                max={100}
                step={5}
                defaultValue={40}
                onChange={(e: any) => {
                  const v = Number(e.target.value);
                  setTexture(v);
                  localStorage.setItem('rapidraw-enhance-texture', String(v));
                  markDirty();
                }}
                trackClassName="bg-bg-secondary"
                fillOrigin="min"
              />
            </div>
            <div className="flex-1 min-w-0" data-tooltip={t('modals.enhance.grainTooltip')}>
              <Slider
                label={t('modals.enhance.grainLabel')}
                value={grain}
                min={0}
                max={100}
                step={5}
                defaultValue={50}
                onChange={(e: any) => {
                  const v = Number(e.target.value);
                  setGrain(v);
                  localStorage.setItem('rapidraw-enhance-grain', String(v));
                  markDirty();
                }}
                trackClassName="bg-bg-secondary"
                fillOrigin="min"
              />
            </div>
          </div>
        </div>

        <div className="h-10 w-px bg-surface shrink-0" />

        <div className="flex gap-2 shrink-0">
          <button
            onClick={handleClose}
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-card-active transition-colors text-sm"
          >
            {previewBase64 ? t('modals.enhance.close') : t('modals.enhance.cancel')}
          </button>

          <Button onClick={handleRun} disabled={disabled} variant={previewBase64 ? 'secondary' : 'primary'}>
            {isProcessing ? (
              <Loader2 className="animate-spin mr-2" size={16} />
            ) : previewBase64 ? (
              <RefreshCw className="mr-2" size={16} />
            ) : (
              <Sparkles className="mr-2" size={16} />
            )}
            {previewBase64 ? t('modals.enhance.btnRetry') : t('modals.enhance.btnStart')}
          </Button>

          {previewBase64 && (
            <Button onClick={handleSave} disabled={isSaving || isProcessing}>
              {isSaving ? <Loader2 className="animate-spin mr-2" size={16} /> : <Save className="mr-2" size={16} />}
              {t('modals.enhance.btnSave')}
            </Button>
          )}

          {savedPath && (
            <Button onClick={handleOpen} className="bg-surface">
              {t('modals.enhance.openInEditor')}
            </Button>
          )}
        </div>
      </div>
    );
  };

  if (!isMounted) return null;

  return (
    <div
      className={`fixed inset-0 flex items-center justify-center z-50 bg-black/40 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
      onMouseDown={handleBackdropMouseDown}
      onClick={handleBackdropClick}
    >
      <div
        className={`bg-surface rounded-xl shadow-2xl p-6 w-full max-w-4xl transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
      >
        <div className="flex flex-col">
          {renderContent()}
          <div className={`mt-4 flex justify-end gap-3 ${savedPath ? '' : 'pt-4 border-t border-surface/50'}`}>
            {renderButtons()}
          </div>
        </div>
      </div>
    </div>
  );
}
