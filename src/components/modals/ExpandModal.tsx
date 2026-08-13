import { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle, XCircle, Loader2, Save, RefreshCw, ChevronLeft, ChevronRight, Expand, Grid3x3 } from 'lucide-react';
import { motion } from 'framer-motion';
import Button from '../ui/Button';
import ModelPicker, { ModelTaskType } from '../ui/ModelPicker';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useEditorStore } from '../../store/useEditorStore';

interface ExpandModalProps {
  isOpen: boolean;
  onClose(): void;
  onExpand(left: number, top: number, right: number, bottom: number): void;
  onSave(variantIndex: number): Promise<string>;
  onOpenFile(path: string): void;
  error: string | null;
  variants: Array<string>;
  isProcessing: boolean;
  progressMessage: string | null;
  loadingImageUrl?: string | null;
  targetPaths: string[];
}

type Side = 'left' | 'top' | 'right' | 'bottom';
const MAX_FRAC = 0.5;
// Keep the target under the backend's output cap.
const MAX_OUTPUT_PIXELS = 48_000_000;
const GUIDES_STORAGE_KEY = 'rapidraw-expand-guides';

const RATIO_PRESETS: Array<{ label: string; value: number }> = [
  { label: '1:1', value: 1 },
  { label: '3:2', value: 3 / 2 },
  { label: '4:3', value: 4 / 3 },
  { label: '16:9', value: 16 / 9 },
  { label: '4:5', value: 4 / 5 },
  { label: '9:16', value: 9 / 16 },
];

export default function ExpandModal({
  isOpen,
  onClose,
  onExpand,
  onSave,
  onOpenFile,
  error,
  variants,
  isProcessing,
  progressMessage,
  loadingImageUrl,
}: ExpandModalProps) {
  const { t } = useTranslation();
  const { appSettings, handleSettingsChange } = useSettingsStore();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [fracs, setFracs] = useState({ left: 0, top: 0, right: 0, bottom: 0 });
  const [aspect, setAspect] = useState(1.5);
  const [variantIndex, setVariantIndex] = useState(0);
  const [baseDims, setBaseDims] = useState<{ w: number; h: number } | null>(null);
  const [showGuides, setShowGuides] = useState(() => localStorage.getItem(GUIDES_STORAGE_KEY) !== 'off');
  // Editing buffers so typing doesn't fight the fracs-derived display value.
  const [editW, setEditW] = useState<string | null>(null);
  const [editH, setEditH] = useState<string | null>(null);
  const dragSide = useRef<{ side: Side; startPos: number; startFrac: number; imgSize: number } | null>(null);
  const mouseDownTarget = useRef<EventTarget | null>(null);

  const hasExpansion = fracs.left + fracs.top + fracs.right + fracs.bottom > 0.001;

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
        setFracs({ left: 0, top: 0, right: 0, bottom: 0 });
        setVariantIndex(0);
      }, 300);
      return () => clearTimeout(timer);
    }
  }, [isOpen]);

  useEffect(() => {
    setVariantIndex(0);
  }, [variants]);

  useEffect(() => {
    if (!isOpen) return;
    // Expansion processes the edited render, so the crop rect (when set)
    // is the real base size; otherwise the file's full dimensions.
    const { selectedImage, adjustments } = useEditorStore.getState();
    const crop = adjustments?.crop;
    if (crop?.width && crop?.height) {
      setBaseDims({ w: Math.round(crop.width), h: Math.round(crop.height) });
      setAspect(crop.width / crop.height);
    } else if (selectedImage?.width && selectedImage?.height) {
      const steps = ((((adjustments?.orientationSteps ?? 0) % 4) + 4) % 4);
      const swapped = steps === 1 || steps === 3;
      const w = swapped ? selectedImage.height : selectedImage.width;
      const h = swapped ? selectedImage.width : selectedImage.height;
      setBaseDims({ w, h });
      setAspect(w / h);
    } else {
      setBaseDims(null);
    }
  }, [isOpen]);

  const targetDims = baseDims
    ? {
        w: Math.round(baseDims.w * (1 + fracs.left + fracs.right)),
        h: Math.round(baseDims.h * (1 + fracs.top + fracs.bottom)),
      }
    : null;

  // Centered expansion toward exact pixel targets, clamped to the per-side
  // maximum and the backend's output cap.
  const applyTargetDims = (wantW: number, wantH: number) => {
    if (!baseDims) return;
    let w = Math.max(baseDims.w, Math.min(wantW, Math.round(baseDims.w * (1 + 2 * MAX_FRAC))));
    let h = Math.max(baseDims.h, Math.min(wantH, Math.round(baseDims.h * (1 + 2 * MAX_FRAC))));
    if (w * h > MAX_OUTPUT_PIXELS) {
      const scale = Math.sqrt(MAX_OUTPUT_PIXELS / (w * h));
      w = Math.max(baseDims.w, Math.floor(w * scale));
      h = Math.max(baseDims.h, Math.floor(h * scale));
    }
    const fw = (w / baseDims.w - 1) / 2;
    const fh = (h / baseDims.h - 1) / 2;
    setFracs({ left: fw, right: fw, top: fh, bottom: fh });
  };

  const applyRatio = (ratio: number) => {
    if (!baseDims) return;
    const current = baseDims.w / baseDims.h;
    if (current < ratio) {
      applyTargetDims(Math.round(baseDims.h * ratio), baseDims.h);
    } else {
      applyTargetDims(baseDims.w, Math.round(baseDims.w / ratio));
    }
  };

  const commitDimInput = () => {
    if (!baseDims || !targetDims) return;
    const w = editW !== null ? parseInt(editW, 10) : targetDims.w;
    const h = editH !== null ? parseInt(editH, 10) : targetDims.h;
    setEditW(null);
    setEditH(null);
    if (Number.isFinite(w) && Number.isFinite(h)) applyTargetDims(w, h);
  };

  const toggleGuides = () => {
    setShowGuides((v) => {
      localStorage.setItem(GUIDES_STORAGE_KEY, v ? 'off' : 'on');
      return !v;
    });
  };

  useEffect(() => {
    const move = (e: MouseEvent) => {
      const d = dragSide.current;
      if (!d) return;
      const pos = d.side === 'left' || d.side === 'right' ? e.clientX : e.clientY;
      const outward = d.side === 'left' || d.side === 'top' ? d.startPos - pos : pos - d.startPos;
      const frac = Math.min(MAX_FRAC, Math.max(0, d.startFrac + outward / d.imgSize));
      setFracs((prev) => ({ ...prev, [d.side]: frac }));
    };
    const up = () => {
      dragSide.current = null;
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
    return () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
  }, []);

  const handleClose = useCallback(() => {
    if (isSaving) return;
    onClose();
  }, [onClose, isSaving]);

  const handleModelChange = (modelId: string) => {
    if (!appSettings) return;
    handleSettingsChange({
      ...appSettings,
      preferredModels: { ...(appSettings.preferredModels || {}), inpaint: modelId },
    });
  };

  const handleSave = async () => {
    setIsSaving(true);
    setSaveError(null);
    try {
      const path = await onSave(variantIndex);
      setSavedPath(path);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setIsSaving(false);
    }
  };

  // ---- frame editor ----
  const renderFrameEditor = () => {
    // The image box is sized so image + max expansion fits the stage.
    const stage = { w: 660, h: 420 };
    const baseScale = Math.min(
      stage.w / (aspect * (1 + 2 * MAX_FRAC)),
      stage.h / (1 + 2 * MAX_FRAC),
    );
    const imgW = baseScale * aspect;
    const imgH = baseScale;
    const frame = {
      left: fracs.left * imgW,
      right: fracs.right * imgW,
      top: fracs.top * imgH,
      bottom: fracs.bottom * imgH,
    };

    const startDrag = (side: Side) => (e: React.MouseEvent) => {
      e.preventDefault();
      dragSide.current = {
        side,
        startPos: side === 'left' || side === 'right' ? e.clientX : e.clientY,
        startFrac: fracs[side],
        imgSize: side === 'left' || side === 'right' ? imgW : imgH,
      };
    };

    const handleClass =
      'absolute bg-accent rounded-full z-10 hover:scale-125 transition-transform shadow-md';

    return (
      <div className="flex flex-col items-center">
        <div className="relative flex items-center justify-center" style={{ width: stage.w, height: stage.h }}>
          {/* frame = final canvas; hatching marks the area to be generated */}
          <div
            className="absolute border-2 border-dashed border-accent/80 bg-surface/30"
            style={{
              width: imgW + frame.left + frame.right,
              height: imgH + frame.top + frame.bottom,
              left: stage.w / 2 - imgW / 2 - frame.left,
              top: stage.h / 2 - imgH / 2 - frame.top,
              backgroundImage:
                'repeating-linear-gradient(45deg, transparent, transparent 6px, rgba(255,255,255,0.06) 6px, rgba(255,255,255,0.06) 8px)',
            }}
          >
            {showGuides && (
              <div className="absolute inset-0 pointer-events-none">
                {/* rule-of-thirds over the FINAL canvas */}
                <div className="absolute top-0 bottom-0 border-l border-white/40" style={{ left: '33.333%' }} />
                <div className="absolute top-0 bottom-0 border-l border-white/40" style={{ left: '66.666%' }} />
                <div className="absolute left-0 right-0 border-t border-white/40" style={{ top: '33.333%' }} />
                <div className="absolute left-0 right-0 border-t border-white/40" style={{ top: '66.666%' }} />
              </div>
            )}
          </div>
          {/* image */}
          <div
            className={`absolute overflow-hidden bg-black ${showGuides ? 'ring-1 ring-white/60' : ''}`}
            style={{ width: imgW, height: imgH, left: stage.w / 2 - imgW / 2, top: stage.h / 2 - imgH / 2 }}
          >
            {loadingImageUrl ? (
              <img
                src={loadingImageUrl}
                alt=""
                className="w-full h-full object-cover"
                draggable={false}
                onLoad={(e) => {
                  const img = e.currentTarget;
                  if (img.naturalWidth && img.naturalHeight) setAspect(img.naturalWidth / img.naturalHeight);
                }}
              />
            ) : (
              <div className="w-full h-full bg-surface/50" />
            )}
          </div>
          {/* side handles on the frame */}
          <div
            className={`${handleClass} cursor-ew-resize`}
            style={{
              width: 12,
              height: 32,
              left: stage.w / 2 - imgW / 2 - frame.left - 6,
              top: stage.h / 2 - 16,
            }}
            onMouseDown={startDrag('left')}
          />
          <div
            className={`${handleClass} cursor-ew-resize`}
            style={{
              width: 12,
              height: 32,
              left: stage.w / 2 + imgW / 2 + frame.right - 6,
              top: stage.h / 2 - 16,
            }}
            onMouseDown={startDrag('right')}
          />
          <div
            className={`${handleClass} cursor-ns-resize`}
            style={{
              width: 32,
              height: 12,
              left: stage.w / 2 - 16,
              top: stage.h / 2 - imgH / 2 - frame.top - 6,
            }}
            onMouseDown={startDrag('top')}
          />
          <div
            className={`${handleClass} cursor-ns-resize`}
            style={{
              width: 32,
              height: 12,
              left: stage.w / 2 - 16,
              top: stage.h / 2 + imgH / 2 + frame.bottom - 6,
            }}
            onMouseDown={startDrag('bottom')}
          />
        </div>
        <div className="mt-3 flex items-center gap-4 flex-wrap justify-center">
          {baseDims && targetDims && (
            <div className="flex items-center gap-2">
              <Text variant={TextVariants.small} weight={TextWeights.medium}>
                {t('modals.expand.targetSize')}
              </Text>
              <input
                type="number"
                min={baseDims.w}
                value={editW ?? String(targetDims.w)}
                onChange={(e) => setEditW(e.target.value)}
                onBlur={commitDimInput}
                onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
                className="w-[76px] px-2 py-1 rounded-md bg-bg-primary text-sm text-text-primary text-center outline-none focus:ring-1 focus:ring-accent"
              />
              <Text variant={TextVariants.small}>×</Text>
              <input
                type="number"
                min={baseDims.h}
                value={editH ?? String(targetDims.h)}
                onChange={(e) => setEditH(e.target.value)}
                onBlur={commitDimInput}
                onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
                className="w-[76px] px-2 py-1 rounded-md bg-bg-primary text-sm text-text-primary text-center outline-none focus:ring-1 focus:ring-accent"
              />
              <Text variant={TextVariants.small} className="opacity-70">
                px
              </Text>
            </div>
          )}
          <div className="flex items-center gap-1">
            {RATIO_PRESETS.map((r) => (
              <button
                key={r.label}
                onClick={() => applyRatio(r.value)}
                className="px-2 py-1 rounded text-xs bg-bg-primary text-text-secondary hover:bg-card-active hover:text-text-primary transition-colors"
              >
                {r.label}
              </button>
            ))}
          </div>
          <button
            onClick={toggleGuides}
            data-tooltip={t('modals.expand.guidesTooltip')}
            className={`p-1.5 rounded-md transition-colors ${
              showGuides ? 'bg-accent/20 text-accent' : 'bg-bg-primary text-text-secondary hover:bg-card-active'
            }`}
          >
            <Grid3x3 size={16} />
          </button>
        </div>
        <div className="mt-2 flex items-center gap-3">
          <Text variant={TextVariants.small} className="opacity-70">
            {t('modals.expand.dragHint')}
          </Text>
          {baseDims && targetDims && hasExpansion && (
            <Text variant={TextVariants.small} className="font-mono opacity-90">
              {t('modals.expand.output')}: {targetDims.w} × {targetDims.h} px
            </Text>
          )}
        </div>
      </div>
    );
  };

  const renderContent = () => {
    if (error) {
      return (
        <div className="flex flex-col items-center justify-center py-10 h-[460px]">
          <XCircle className="w-12 h-12 text-red-500 mb-6" />
          <Text variant={TextVariants.title} className="mb-2 text-center">
            {t('modals.expand.processingFailed')}
          </Text>
          <Text className="text-center p-4 rounded-lg bg-bg-primary max-w-md mt-2 leading-relaxed">
            {String(error)}
          </Text>
        </div>
      );
    }

    if (variants.length > 0 && !isProcessing) {
      return (
        <div className="flex flex-col items-center h-[500px]">
          <div className="flex-1 min-h-0 flex items-center justify-center w-full relative">
            <img
              src={variants[variantIndex]}
              alt=""
              className="max-w-full max-h-full object-contain rounded-lg border border-surface"
              draggable={false}
            />
            {variants.length > 1 && (
              <>
                <button
                  onClick={() => setVariantIndex((i) => (i + variants.length - 1) % variants.length)}
                  className="absolute left-2 top-1/2 -translate-y-1/2 p-2 rounded-full bg-black/50 text-white hover:bg-black/70"
                >
                  <ChevronLeft size={20} />
                </button>
                <button
                  onClick={() => setVariantIndex((i) => (i + 1) % variants.length)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 p-2 rounded-full bg-black/50 text-white hover:bg-black/70"
                >
                  <ChevronRight size={20} />
                </button>
              </>
            )}
          </div>
          <div className="flex items-center gap-2 mt-3">
            {variants.map((_, i) => (
              <button
                key={i}
                onClick={() => setVariantIndex(i)}
                className={`w-2.5 h-2.5 rounded-full transition-colors ${
                  i === variantIndex ? 'bg-accent' : 'bg-surface hover:bg-card-active'
                }`}
              />
            ))}
            <Text variant={TextVariants.small} className="ml-2">
              {t('modals.expand.variantLabel', { current: variantIndex + 1, total: variants.length })}
            </Text>
          </div>
          {saveError && (
            <Text
              as="div"
              variant={TextVariants.small}
              color={TextColors.error}
              className="flex items-center justify-center gap-2 mt-3 text-center px-4"
            >
              <XCircle className="w-4 h-4 shrink-0" />
              <span>{saveError}</span>
            </Text>
          )}
          {savedPath && (
            <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.3 }}>
              <Text
                as="div"
                variant={TextVariants.heading}
                color={TextColors.success}
                className="flex items-center justify-center gap-2 mt-3"
              >
                <CheckCircle className="w-5 h-5" />
                <span>{t('modals.expand.saveSuccess')}</span>
              </Text>
            </motion.div>
          )}
        </div>
      );
    }

    if (isProcessing) {
      return (
        <div className="flex flex-col items-center justify-center h-[460px]">
          <Loader2 className="animate-spin text-accent mb-4" size={32} />
          <Text variant={TextVariants.title} className="mb-2 text-center">
            {t('modals.expand.generating')}
          </Text>
          <Text className="text-center font-mono h-6">{progressMessage || ''}</Text>
        </div>
      );
    }

    return (
      <div className="flex flex-col items-center justify-center h-[540px]">
        {renderFrameEditor()}
      </div>
    );
  };

  const renderButtons = () => {
    if (error) {
      return (
        <Button onClick={handleClose} className="w-full">
          {t('modals.expand.close')}
        </Button>
      );
    }
    const disabled = isProcessing || isSaving;
    return (
      <div className={`w-full flex items-center gap-4 ${disabled ? 'opacity-50 pointer-events-none' : ''}`}>
        <div className="flex-1 flex items-center gap-6">
          <div className="flex flex-col gap-1 w-[280px] mt-2 shrink-0">
            <Text variant={TextVariants.body} weight={TextWeights.medium}>
              {t('modelRegistry.model')}
            </Text>
            <ModelPicker
              taskType={ModelTaskType.Inpaint}
              value={appSettings?.preferredModels?.inpaint ?? null}
              onChange={handleModelChange}
            />
          </div>
        </div>
        <div className="h-10 w-px bg-surface shrink-0" />
        <div className="flex gap-2 shrink-0">
          <button
            onClick={handleClose}
            className="px-4 py-2 rounded-md text-text-secondary hover:bg-card-active transition-colors text-sm"
          >
            {t('modals.expand.cancel')}
          </button>
          <Button
            onClick={() => {
              setSavedPath(null);
              onExpand(fracs.left, fracs.top, fracs.right, fracs.bottom);
            }}
            disabled={disabled || !hasExpansion}
            variant={variants.length > 0 ? 'secondary' : 'primary'}
          >
            {isProcessing ? (
              <Loader2 className="animate-spin mr-2" size={16} />
            ) : variants.length > 0 ? (
              <RefreshCw className="mr-2" size={16} />
            ) : (
              <Expand className="mr-2" size={16} />
            )}
            {variants.length > 0 ? t('modals.expand.btnRetry') : t('modals.expand.btnGenerate')}
          </Button>
          {variants.length > 0 && (
            <Button onClick={handleSave} disabled={isSaving || isProcessing}>
              {isSaving ? <Loader2 className="animate-spin mr-2" size={16} /> : <Save className="mr-2" size={16} />}
              {t('modals.expand.btnSave')}
            </Button>
          )}

          {savedPath && (
            <Button
              className="bg-surface"
              onClick={() => {
                onOpenFile(savedPath);
                handleClose();
              }}
            >
              {t('modals.expand.openInEditor')}
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
      onMouseDown={(e) => {
        mouseDownTarget.current = e.target;
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget && mouseDownTarget.current === e.currentTarget) handleClose();
        mouseDownTarget.current = null;
      }}
    >
      <div
        className={`bg-surface rounded-xl shadow-2xl p-6 w-full max-w-5xl transform transition-all duration-300 ease-out ${
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
