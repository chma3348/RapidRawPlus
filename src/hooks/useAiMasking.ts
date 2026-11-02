import { useRef, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { create } from 'zustand';
import { useEditorStore } from '../store/useEditorStore';
import { useEditorActions } from './useEditorActions';
import {
  Adjustments,
  AiPatch,
  AiPatchResultVariant,
  MaskContainer,
  Coord,
  INITIAL_MASK_CONTAINER,
  normalizeLoadedAdjustments,
} from '../utils/adjustments';
import { syncFillAdjustmentMasks } from '../utils/fillAdjustmentMasks';
import { globalImageCache } from '../utils/ImageLRUCache';
import { v4 as uuidv4 } from 'uuid';
import { SubMask } from '../components/panel/right/Masks';
import { healLines, type Offset } from '../store/useHealSelection';
import { Invokes } from '../components/ui/AppProperties';
import { useAuth } from '@clerk/react';
import type { ReplacementBlendOptions } from '../components/panel/right/ReplacementBlendControls';
import { subjectPrompts } from '../utils/subjectSelection';

const pendingSelections = new Set<string>();

/**
 * Sub-masks whose AI selection is still being worked out: waiting out the
 * paint debounce (`waiting`) or running (`running`, a count per sub-mask).
 * Generate waits for these; a fill started earlier was made from the raw
 * strokes, not the object they select.
 */
interface PendingAiSelections {
  waiting: string | null;
  running: Record<string, number>;
}
export const usePendingAiSelections = create<PendingAiSelections>(() => ({ waiting: null, running: {} }));

export const isSelectionPending = (state: PendingAiSelections, subMaskIds: string[]) =>
  subMaskIds.some((id) => state.waiting === id || (state.running[id] ?? 0) > 0);

const startSelectionRun = (subMaskId: string) =>
  usePendingAiSelections.setState((s) => ({
    running: { ...s.running, [subMaskId]: (s.running[subMaskId] ?? 0) + 1 },
  }));

const endSelectionRun = (subMaskId: string) =>
  usePendingAiSelections.setState((s) => {
    const running = { ...s.running };
    if ((running[subMaskId] ?? 0) <= 1) delete running[subMaskId];
    else running[subMaskId] -= 1;
    return { running };
  });

/**
 * A cheap fingerprint of what a patch's sub-masks select. Mask bitmaps are
 * megabytes of base64, so those are sampled rather than copied whole.
 */
export const selectionSignature = (subMasks: SubMask[] = []) =>
  JSON.stringify(
    subMasks.map((sm: SubMask) => {
      // The request id changes when a run starts, not when the selection does.
      const { maskDataBase64, lines, subjectRequestId: _request, ...rest } = sm.parameters || {};
      const data = typeof maskDataBase64 === 'string' ? maskDataBase64 : '';
      let hash = 2166136261;
      const step = Math.max(1, Math.floor(data.length / 512));
      for (let i = 0; i < data.length; i += step) hash = Math.imul(hash ^ data.charCodeAt(i), 16777619);
      return [
        sm.id, sm.type, sm.mode, sm.visible, sm.invert, sm.opacity, rest, data.length, hash,
        Array.isArray(lines)
          ? lines.map((l: { tool?: string; brushSize?: number; points?: unknown }) =>
            [l.tool, l.brushSize, JSON.stringify(l.points)])
          : null,
      ];
    }),
  );

/**
 * The selection each fill was last run on, by patch id: when it differs from
 * the patch's selection now, the fill is out of date and the panel says so.
 * Kept for this session only; a reopened photo starts with nothing to compare.
 */
const fillSelections = new Map<string, string>();
export const fillSelectionOf = (patchId: string) => fillSelections.get(patchId);

/**
 * A fill can take minutes. If the user has moved to another photo by the time
 * it finishes, the result goes into the photo it was made for, on disk,
 * instead of being dropped. Returns false if that photo no longer has the patch.
 */
const saveToPhoto = async (path: string, patchId: string, update: (p: AiPatch) => AiPatch) => {
  const metadata = await invoke<{ adjustments?: (Adjustments & { is_null?: boolean }) | null } | null>(
    Invokes.LoadMetadata,
    { path },
  );
  if (!metadata?.adjustments || metadata.adjustments.is_null) return false;
  const saved: Adjustments = normalizeLoadedAdjustments(metadata.adjustments);
  if (!saved.aiPatches?.some((p: AiPatch) => p.id === patchId)) return false;
  const next = syncFillAdjustmentMasks({
    ...saved,
    aiPatches: saved.aiPatches.map((p: AiPatch) => (p.id === patchId ? update(p) : p)),
  });
  await invoke(Invokes.SaveMetadataAndUpdateThumbnail, { path, adjustments: next });
  // The editor's cached copy of that photo predates the result.
  globalImageCache.delete(path);
  return true;
};

const getTransformAdjustments = (adj: Adjustments) => ({
  transformDistortion: adj.transformDistortion,
  transformVertical: adj.transformVertical,
  transformHorizontal: adj.transformHorizontal,
  transformRotate: adj.transformRotate,
  transformAspect: adj.transformAspect,
  transformScale: adj.transformScale,
  transformXOffset: adj.transformXOffset,
  transformYOffset: adj.transformYOffset,
  lensDistortionAmount: adj.lensDistortionAmount,
  lensVignetteAmount: adj.lensVignetteAmount,
  lensTcaAmount: adj.lensTcaAmount,
  lensDistortionParams: adj.lensDistortionParams,
  lensMaker: adj.lensMaker,
  lensModel: adj.lensModel,
  lensDistortionEnabled: adj.lensDistortionEnabled,
  lensTcaEnabled: adj.lensTcaEnabled,
  lensVignetteEnabled: adj.lensVignetteEnabled,
});

export function useAiMasking() {
  const { setAdjustments } = useEditorActions();
  const setEditor = useEditorStore((state) => state.setEditor);
  const { getToken } = useAuth();

  const updateSubMask = useCallback(
    (subMaskId: string, updatedData: any) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
        aiPatches: (prev.aiPatches || []).map((p: AiPatch) =>
          p.subMasks.some((sm: SubMask) => sm.id === subMaskId)
            ? {
                ...p,
                subMasks: p.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
                // "Too little around the selection" was about the old one.
                needsPromptReason: undefined,
              }
            : p,
        ),
      }));
    },
    [setAdjustments],
  );

  const handleGenerativeReplace = useCallback(
    async (
      patchId: string,
      prompt: string,
      useFastInpaint: boolean,
      reconstructSinglePath = false,
      generateMode = false,
      generateOptions?: {
        contentScale?: number;
        matchPhoto?: number;
        loras?: Array<{ name: string; strength: number }>;
      },
    ) => {
      // Text and Replace mode always require a generative model, even if a
      // previous quick-repair selection left the fast preference enabled.
      useFastInpaint = useFastInpaint && !generateMode && !prompt.trim();
      const { selectedImage, adjustments, isGeneratingAi, patchesSentToBackend } = useEditorStore.getState();
      // Every early exit must be LOUD: silent returns here read as "the
      // button does nothing" (console.error reaches app.log).
      if (!selectedImage?.path || isGeneratingAi) {
        console.error('[ai] generate blocked:', {
          hasImage: !!selectedImage?.path,
          isGeneratingAi,
        });
        if (isGeneratingAi) toast.info('An AI generation is already running.');
        return;
      }

      const patch: AiPatch | undefined = adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
      if (!patch) {
        console.error('[ai] generate blocked: patch not found', patchId);
        toast.error('The selection could not be found — try re-creating it.');
        return;
      }
      // The panel disables Generate meanwhile; this catches Enter in the
      // prompt and any other way in.
      if (isSelectionPending(usePendingAiSelections.getState(), patch.subMasks.map((sm: SubMask) => sm.id))) {
        console.error('[ai] generate blocked: selection still being made', patchId);
        toast.info('Wait for the selection to finish, then generate.');
        return;
      }

      // Threaded explicitly rather than read back from the store: the panel
      // calls updateContainer immediately before this, and that state is not
      // visible to getState() yet.
      const patchDefinition = {
        ...patch,
        prompt,
        reconstructSinglePath,
        generateMode,
        contentScale: generateOptions?.contentScale ?? patch.contentScale ?? 1.0,
        matchPhoto: generateOptions?.matchPhoto ?? patch.matchPhoto ?? 0.8,
        loras: generateOptions?.loras ?? patch.loras ?? [],
      };

      // Visible feedback FIRST — the token fetch used to run before any
      // state change, so a hung auth lookup looked like a dead button.
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: prev.aiPatches.map((p: AiPatch) =>
          p.id === patchId ? { ...p, isLoading: true, prompt, reconstructSinglePath, generateMode } : p,
        ),
      }));
      setEditor({ isGeneratingAi: true });

      // Cloud auth is optional for local fills; never let it hang the run.
      const token = await Promise.race([
        getToken(),
        new Promise<null>((resolve) => setTimeout(() => resolve(null), 3000)),
      ]).catch(() => null);

      try {
        // The token wait can take seconds: if another photo was opened
        // meanwhile, this one's selection and adjustments no longer match
        // what the backend has loaded.
        if (useEditorStore.getState().selectedImage?.path !== selectedImage.path) {
          console.error('[ai] generate cancelled: the photo changed before the fill started');
          toast.info('The photo changed before the fill started, so nothing was run.');
          setAdjustments((prev: Adjustments) => ({
            ...prev,
            aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: false } : p)),
          }));
          return;
        }
        const selection = selectionSignature(patch.subMasks);
        const newPatchDataJson: any = await invoke(Invokes.InvokeGenerativeReplaseWithMaskDef, {
          currentAdjustments: adjustments,
          patchDefinition: patchDefinition,
          path: selectedImage.path,
          useFastInpaint: useFastInpaint,
          token: token || null,
        });

        const newPatchData = JSON.parse(newPatchDataJson);
        fillSelections.set(patchId, selection);
        const finished = (p: AiPatch): AiPatch => ({
          ...p,
          patchData: newPatchData,
          isLoading: false,
          reconstructSinglePath,
          needsPromptReason: undefined,
          prompt,
          name: useFastInpaint ? 'Inpaint' : prompt && prompt.trim() ? prompt.trim() : p.name,
          // A fill made on an Enhance retouch makes it a Remove one.
          patchType: p.patchType === 'enhance' ? undefined : p.patchType,
        });

        if (useEditorStore.getState().selectedImage?.path !== selectedImage.path) {
          const name = selectedImage.path.split(/[\\/]/).pop();
          if (newPatchData?.needsPrompt) {
            toast.info(
              `${name}: there was too little around the selection to rebuild from. Describe what should be there.`,
            );
          } else if (await saveToPhoto(selectedImage.path, patchId, finished)) {
            toast.success(`Remove finished on ${name}.`);
          } else {
            toast.info(`Remove finished on ${name}, but that selection had been deleted.`);
          }
          return;
        }

        // A promptless repair with nothing to rebuild from comes back asking
        // for direction rather than spending minutes producing a wash. The
        // prompt field is already on screen; this just says why it is needed.
        if (newPatchData && newPatchData.needsPrompt) {
          const pct = Math.round((newPatchData.usableFraction ?? 0) * 100);
          setAdjustments((prev: Adjustments) => ({
            ...prev,
            aiPatches: prev.aiPatches.map((p: AiPatch) =>
              p.id === patchId ? { ...p, isLoading: false, needsPromptReason: pct } : p,
            ),
          }));
          toast.info(
            `Only ${pct}% of the area around this selection has usable detail, so there is nothing to rebuild from. Describe what should be here.`,
            { autoClose: 8000 },
          );
          return;
        }

        patchesSentToBackend.delete(patchId);

        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? finished(p) : p)),
        }));
        // Keep the container selected: deselecting grayed the whole
        // generative section into a pointer-events-none dead zone, so the
        // next "Inpaint Selection" click silently did nothing. Only the
        // sub-mask overlay is dismissed to reveal the result.
        setEditor({ activeAiSubMaskId: null });
      } catch (err) {
        toast.error(`AI Replace Failed: ${err}`);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: false } : p)),
        }));
      } finally {
        setEditor({ isGeneratingAi: false });
      }
    },
    [setAdjustments, setEditor],
  );

  const handleReblendReplacement = useCallback(async (patchId: string, options: ReplacementBlendOptions) => {
    const snapshot = useEditorStore.getState();
    const patch = snapshot.adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
    if (!patch?.patchData || snapshot.isGeneratingAi) return false;
    const originalData = patch.patchData;
    setEditor({ isGeneratingAi: true });
    try {
      // Keep a portable baseline in the sidecar too: restoring the original
      // must still work after the raw disk cache is removed or on another Mac.
      const baseline = originalData.replacementOriginal ?? {
        color: originalData.color, mask: originalData.mask, encoding: originalData.encoding,
      };
      const result = !options.improved && originalData.replacementOriginal
        ? { ...baseline, replacementBlend: { options, canExpand: false } }
        : JSON.parse(await invoke<string>('reblend_replacement', {
            patchId, currentAdjustments: snapshot.adjustments, options,
          }));
      result.replacementOriginal = baseline;
      const current = useEditorStore.getState();
      // Navigation, undo, variant changes, or another result must never receive
      // a late blend calculated for the previous photo/result.
      if (current.selectedImage?.path !== snapshot.selectedImage?.path
        || current.adjustments.aiPatches.find((p: AiPatch) => p.id === patchId)?.patchData !== originalData) return false;
      current.patchesSentToBackend.delete(patchId);
      setAdjustments((prev: Adjustments) => ({ ...prev,
        aiPatches: prev.aiPatches.map((p: AiPatch) => p.id === patchId && p.patchData === originalData
          ? { ...p, patchData: { ...p.patchData, ...result } } : p),
      }));
      return true;
    } catch (error) {
      toast.error(`Blend was not applied: ${error}`);
      return false;
    } finally { setEditor({ isGeneratingAi: false }); }
  }, [setAdjustments, setEditor]);

  /// Turns a finished fill into an ordinary editable mask.
  ///
  /// Patches are composited into the image BEFORE any adjustment or mask
  /// processing runs (load_and_composite), so a mask covering the fill
  /// region adjusts the FILLED pixels. That means the whole existing mask
  /// panel — exposure, contrast, colour, curves — works on the fill for
  /// free, and the user's eye decides whether it sits in the photo instead
  /// of a statistic deciding for them.
  const handleAdjustFillArea = useCallback(
    (patchId: string) => {
      const { adjustments } = useEditorStore.getState();
      const patch: AiPatch | undefined = adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
      const maskData = patch?.patchData?.mask;
      if (!patch || !maskData) {
        toast.error('Run the fill first — there is no filled area to adjust yet.');
        return;
      }

      const existing = adjustments.masks.find((mask) => mask.sourceAiPatchId === patchId);
      if (existing) return existing.id;

      const subMask: any = {
        id: uuidv4(),
        type: 'ai-paint',
        visible: true,
        mode: 'additive',
        parameters: {
          maskDataBase64: maskData,
          // The patch mask is in ORIGINAL image space. Renders and the
          // overlay place it with the photo's CURRENT orientation (as every
          // AI mask), so it follows later turns and flips; these record the
          // orientation it was made under, as ai-subject masks do.
          rotation: adjustments.rotation ?? 0,
          flipHorizontal: !!adjustments.flipHorizontal,
          flipVertical: !!adjustments.flipVertical,
          orientationSteps: adjustments.orientationSteps ?? 0,
          grow: 0,
          feather: 0,
        },
      };

      const container: any = {
        ...INITIAL_MASK_CONTAINER,
        id: uuidv4(),
        name: `Fill: ${patch.name || 'AI area'}`,
        sourceAiPatchId: patchId,
        sourceAiSubMaskId: subMask.id,
        subMasks: [subMask],
      };

      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: [...(prev.masks || []), container],
      }));
      toast.success('Fill adjustments are ready in the Masks panel.');
      return container.id;
    },
    [setAdjustments],
  );

  /// Clone/heal: deterministic copy from a source offset. No engine, so
  /// it returns in a moment and is the right tool where generation cannot
  /// work — fine repeating structure, or anything that must stay real.
  const handleCloneStamp = useCallback(
    async (patchId: string) => {
      const { selectedImage, adjustments, isGeneratingAi, patchesSentToBackend } =
        useEditorStore.getState();
      if (!selectedImage?.path) return;
      if (isGeneratingAi) {
        console.error('[clone] blocked: another AI operation is running');
        return;
      }
      const container = adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
      if (!container) {
        console.error('[clone] blocked: patch container not found', patchId);
        return;
      }
      // Heal blends the source into the destination's tone; clone copies it
      // verbatim. The backend needs to know which the user asked for.
      const isHeal = container.patchType === 'heal';

      setEditor({ isGeneratingAi: true });
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: true } : p)),
      }));

      // Heal re-runs on every stroke, drag and delete, and each run ships
      // currentAdjustments across the IPC bridge. A single hidden AI fill
      // patch on this photo measured 37.4 MB of base64 in that payload —
      // data the backend provably never reads: composite_patches_on_image
      // skips invisible patches outright, and the patch being healed is
      // removed from the source adjustments before compositing anyway.
      // Strip both so the round trip carries geometry, not dead megabytes.
      const slimAdjustments = {
        ...adjustments,
        aiPatches: (adjustments.aiPatches || []).map((p: AiPatch) =>
          p.id === patchId || !p.visible ? { ...p, patchData: null } : p,
        ),
      };

      try {
        const patchJson: any = await invoke(Invokes.ApplyClonePatch, {
          path: selectedImage.path,
          patchDefinition: container,
          heal: isHeal,
          currentAdjustments: slimAdjustments,
        });
        // A "null" body means every spot was deleted: patchData becomes
        // null and the repair comes off the photo with its markers.
        const patchData = JSON.parse(patchJson);
        // The source each stroke was healed from, in stroke order, so the
        // canvas draws where it really borrowed from. Not part of the patch.
        const sources: Array<Offset | null> = patchData?.healSources || [];
        if (patchData) delete patchData.healSources;
        // Units in the order the backend split them: a stroke each, or a
        // whole sub-mask that has no strokes. Each stroke is known by its
        // points, not its index: a spot deleted while this ran shifts the
        // indexes, and its source would have gone to the next spot.
        const strokeKey = (subMaskId: string, line: { points?: Offset[] }) =>
          `${subMaskId}:${JSON.stringify(line.points ?? [])}`;
        const units: Array<{ subMaskId: string; lineIndex: number; key: string } | null> = [];
        for (const sm of container.subMasks || []) {
          const lines = healLines(sm.parameters);
          if (lines && lines.length > 0) {
            lines.forEach((line, lineIndex) =>
              units.push({ subMaskId: sm.id, lineIndex, key: strokeKey(sm.id, line) }));
          } else {
            units.push(null);
          }
        }
        // Refresh source found nothing else that fits: say so, rather than
        // seeming to ignore the key.
        const sameAgain = units.some((unit, i) => {
          const sm = unit && container.subMasks.find((m) => m.id === unit.subMaskId);
          const line = unit && sm ? healLines(sm.parameters)?.[unit.lineIndex] : undefined;
          const turnedDown = line?.rejectedSources?.[line.rejectedSources.length - 1];
          const got = sources[i];
          return !line?.cloneOffset && turnedDown && got && Math.hypot(got.x - turnedDown.x, got.y - turnedDown.y) < 2;
        });
        if (sameAgain) toast.info('No other good source nearby');
        // The preview strips patchData for any patch the backend has
        // already cached, and hydrate_adjustments then fills the gap from
        // that cache. Without this the next redraw rebuilt the PREVIOUS
        // heal — every other AI path drops the id here for the same reason.
        patchesSentToBackend.delete(patchId);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) => {
            if (p.id !== patchId) return p;
            // Two identical strokes take their sources in order.
            const claimed = new Set<number>();
            const subMasks = p.subMasks.map((sm: SubMask) => {
              const lines = healLines(sm.parameters);
              if (!lines) return sm;
              return {
                ...sm,
                parameters: {
                  ...sm.parameters,
                  lines: lines.map((line) => {
                    const key = strokeKey(sm.id, line);
                    const at = units.findIndex((u, i) => u?.key === key && !claimed.has(i));
                    if (at >= 0) claimed.add(at);
                    const source = at >= 0 ? sources[at] : null;
                    // Only strokes left to choose their own; one dragged by
                    // hand keeps its cloneOffset.
                    return source && !line.cloneOffset ? { ...line, autoOffset: source } : line;
                  }),
                },
              } as SubMask;
            });
            return { ...p, subMasks, patchData, isLoading: false, name: isHeal ? 'Heal' : 'Clone' };
          }),
        }));
        // Heal and Clone are meant to be worked at: the brush stays on the
        // same sub-mask so more strokes can be added and sources re-dragged,
        // each edit re-running the repair.
      } catch (err) {
        toast.error(`${isHeal ? 'Heal' : 'Clone'} failed: ${err}`);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: false } : p)),
        }));
      } finally {
        setEditor({ isGeneratingAi: false });
      }
    },
    [setAdjustments, setEditor],
  );

  const handleSpotEnhance = useCallback(
    async (patchId: string, task: string, strength: number, texture: number = 0, grain: number = 0) => {
      const { selectedImage, adjustments, isGeneratingAi, patchesSentToBackend } = useEditorStore.getState();
      if (!selectedImage?.path || isGeneratingAi) return;

      const patch: AiPatch | undefined = adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
      if (!patch) return;

      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: true } : p)),
      }));
      setEditor({ isGeneratingAi: true });

      try {
        const newPatchDataJson: any = await invoke(Invokes.InvokeSpotEnhanceWithMaskDef, {
          currentAdjustments: adjustments,
          patchDefinition: { ...patch },
          path: selectedImage.path,
          task,
          strength,
          texture,
          grain,
        });
        const newPatchData = JSON.parse(newPatchDataJson);
        patchesSentToBackend.delete(patchId);

        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) =>
            p.id === patchId
              ? {
                  ...p,
                  patchData: newPatchData,
                  isLoading: false,
                  name: `Spot ${task}`,
                  // What makes it an Enhance retouch; the name is the user's.
                  patchType: 'enhance',
                }
              : p,
          ),
        }));
        setEditor({ activeAiSubMaskId: null });
      } catch (err) {
        toast.error(`Spot Enhance Failed: ${err}`);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: false } : p)),
        }));
      } finally {
        setEditor({ isGeneratingAi: false });
      }
    },
    [setAdjustments, setEditor],
  );

  // Live re-blend of a rendered spot patch from the backend's cached raw
  // region — instant, no model re-run.
  const handleRespotEnhance = useCallback(
    async (patchId: string, strength: number, texture: number, grain: number) => {
      const { adjustments, patchesSentToBackend } = useEditorStore.getState();
      const patch: AiPatch | undefined = adjustments.aiPatches.find((p: AiPatch) => p.id === patchId);
      if (!patch || !patch.patchData) return;
      try {
        const newPatchDataJson: any = await invoke(Invokes.RespotEnhance, {
          patchId,
          strength,
          texture,
          grain,
        });
        const newPatchData = JSON.parse(newPatchDataJson);
        patchesSentToBackend.delete(patchId);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) =>
            p.id === patchId ? { ...p, patchData: newPatchData } : p,
          ),
        }));
      } catch (err) {
        // Cache expired (app restart or a newer spot run) — quiet log; the
        // user can re-run Enhance for a fresh raw.
        console.error('[spot] re-blend unavailable:', err);
      }
    },
    [setAdjustments],
  );

  const handleQuickErase = useCallback(
    async (subMaskId: string | null, startPoint: Coord, endPoint: Coord) => {
      const { selectedImage, adjustments, isGeneratingAi, patchesSentToBackend } = useEditorStore.getState();
      if (!selectedImage?.path || isGeneratingAi) return;
      const token = await getToken();

      const patchId = adjustments.aiPatches.find((p: AiPatch) =>
        p.subMasks.some((sm: SubMask) => sm.id === subMaskId),
      )?.id;
      if (!patchId) return;

      setEditor({ isGeneratingAi: true });
      setAdjustments((prev: Partial<Adjustments>) => ({
        ...prev,
        aiPatches: prev.aiPatches?.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: true } : p)),
      }));

      try {
        const transformAdjustments = getTransformAdjustments(adjustments);
        const newMaskParams: any = await invoke(Invokes.GenerateAiSubjectMask, {
          jsAdjustments: transformAdjustments,
          endPoint: [endPoint.x, endPoint.y],
          flipHorizontal: adjustments.flipHorizontal,
          flipVertical: adjustments.flipVertical,
          orientationSteps: adjustments.orientationSteps,
          path: selectedImage.path,
          rotation: adjustments.rotation,
          startPoint: [startPoint.x, startPoint.y],
        });

        const subMaskToUpdate = adjustments.aiPatches
          ?.find((p: AiPatch) => p.id === patchId)
          ?.subMasks.find((sm: SubMask) => sm.id === subMaskId);
        const finalSubMaskParams: any = { ...subMaskToUpdate?.parameters, ...newMaskParams };
        const updatedAdjustmentsForBackend = {
          ...adjustments,
          aiPatches: adjustments.aiPatches.map((p: AiPatch) =>
            p.id === patchId
              ? {
                  ...p,
                  subMasks: p.subMasks.map((sm: SubMask) =>
                    sm.id === subMaskId ? { ...sm, parameters: finalSubMaskParams } : sm,
                  ),
                }
              : p,
          ),
        };

        const patchDefinitionForBackend = updatedAdjustmentsForBackend.aiPatches.find((p: AiPatch) => p.id === patchId);
        const newPatchDataJson: any = await invoke(Invokes.InvokeGenerativeReplaseWithMaskDef, {
          currentAdjustments: updatedAdjustmentsForBackend,
          patchDefinition: { ...patchDefinitionForBackend, prompt: '' },
          path: selectedImage.path,
          useFastInpaint: true,
          token: token || null,
        });

        const newPatchData = JSON.parse(newPatchDataJson);
        fillSelections.set(patchId, selectionSignature(patchDefinitionForBackend?.subMasks));
        patchesSentToBackend.delete(patchId);

        setAdjustments((prev: Partial<Adjustments>) => ({
          ...prev,
          aiPatches: prev.aiPatches?.map((p: AiPatch) =>
            p.id === patchId
              ? {
                  ...p,
                  patchData: newPatchData,
                  isLoading: false,
                  subMasks: p.subMasks.map((sm: SubMask) =>
                    sm.id === subMaskId ? { ...sm, parameters: finalSubMaskParams } : sm,
                  ),
                }
              : p,
          ),
        }));
        setEditor({ activeAiPatchContainerId: null, activeAiSubMaskId: null });
      } catch (err: any) {
        toast.error(`Quick Erase Failed: ${err.message || String(err)}`);
        setAdjustments((prev: Partial<Adjustments>) => ({
          ...prev,
          aiPatches: prev.aiPatches?.map((p: AiPatch) => (p.id === patchId ? { ...p, isLoading: false } : p)),
        }));
      } finally {
        setEditor({ isGeneratingAi: false });
      }
    },
    [setAdjustments, setEditor],
  );

  const handleDeleteMaskContainer = useCallback(
    (containerId: string) => {
      const { activeMaskContainerId } = useEditorStore.getState();
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: (prev.masks || []).filter((c) => c.id !== containerId),
      }));
      if (activeMaskContainerId === containerId) {
        setEditor({ activeMaskContainerId: null, activeMaskId: null });
      }
    },
    [setAdjustments, setEditor],
  );

  const handleDeleteAiPatch = useCallback(
    (patchId: string) => {
      const { activeAiPatchContainerId } = useEditorStore.getState();
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: (prev.aiPatches || []).filter((p) => p.id !== patchId),
      }));
      if (activeAiPatchContainerId === patchId) {
        setEditor({ activeAiPatchContainerId: null, activeAiSubMaskId: null });
      }
    },
    [setAdjustments, setEditor],
  );

  const handleToggleAiPatchVisibility = useCallback(
    (patchId: string) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: (prev.aiPatches || []).map((p: AiPatch) => (p.id === patchId ? { ...p, visible: !p.visible } : p)),
      }));
    },
    [setAdjustments],
  );

  const handleSelectAiPatchVariant = useCallback(
    (patchId: string, variantId: string) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        aiPatches: (prev.aiPatches || []).map((p: AiPatch) => {
          if (p.id !== patchId || !p.patchData?.reconstructVariants) return p;
          const variants = p.patchData.reconstructVariants as AiPatchResultVariant[];
          const variant = variants.find((v) => v.id === variantId);
          if (!variant) return p;
          return {
            ...p,
            patchData: {
              ...p.patchData,
              color: variant.color,
              mask: variant.mask,
              encoding: variant.encoding ?? p.patchData.encoding,
              reconstructActiveVariantId: variant.id,
              reconstructActiveKind: variant.kind,
              reconstructPrompt: variant.prompt ?? p.patchData.reconstructPrompt,
              reconstructDebugRunId: variant.debugRunId ?? p.patchData.reconstructDebugRunId,
              reconstructDebugDir: variant.debugDir ?? p.patchData.reconstructDebugDir,
              replacementRunId: variant.kind === 'context-replace' ? variant.debugRunId : undefined,
              replacementBlend: undefined,
              replacementOriginal: undefined,
              reconstructVariants: variants,
            },
          };
        }),
      }));
    },
    [setAdjustments],
  );

  const handleGenerateAiMask = async (subMaskId: string, startPoint: Coord, endPoint: Coord, exclude = false) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    const subMask = [...(adjustments.masks || []), ...(adjustments.aiPatches || [])]
      .flatMap((p) => p.subMasks).find((sm) => sm.id === subMaskId);
    if (!subMask) return;
    const requestId = uuidv4();
    const transformAdjustments = getTransformAdjustments(adjustments);
    const geometry = JSON.stringify([transformAdjustments, adjustments.rotation, adjustments.flipHorizontal,
      adjustments.flipVertical, adjustments.orientationSteps]);
    const prior = subMask.parameters;
    let points;
    try {
      points = subjectPrompts(prior.subjectGeometry && prior.subjectGeometry !== geometry ? {} : prior,
        startPoint, endPoint, exclude);
    } catch (error) {
      toast.error(String(error));
      return;
    }
    updateSubMask(subMaskId, { parameters: { ...prior, subjectPoints: points, subjectGeometry: geometry,
      subjectRequestId: requestId } });
    pendingSelections.add(requestId);
    startSelectionRun(subMaskId);
    setEditor({ isGeneratingAiMask: true });

    try {
      const newParameters = await invoke(Invokes.GenerateAiSubjectMask, {
        jsAdjustments: transformAdjustments,
        endPoint: [endPoint.x, endPoint.y],
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        path: selectedImage.path,
        rotation: adjustments.rotation,
        startPoint: [startPoint.x, startPoint.y],
        points,
        selectionId: subMaskId,
        wholeSubject: true,
      });

      const current = useEditorStore.getState();
      const latest = [...(current.adjustments.masks || []), ...(current.adjustments.aiPatches || [])]
        .flatMap((p) => p.subMasks).find((sm) => sm.id === subMaskId);
      if (current.selectedImage?.path !== selectedImage.path || latest?.parameters.subjectRequestId !== requestId) return;
      const currentGeometry = JSON.stringify([getTransformAdjustments(current.adjustments), current.adjustments.rotation,
        current.adjustments.flipHorizontal, current.adjustments.flipVertical, current.adjustments.orientationSteps]);
      if (currentGeometry !== geometry) return;
      const mergedParameters = { ...latest.parameters, ...(newParameters as object) };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Mask Failed: ${error}`);
    } finally {
      endSelectionRun(subMaskId);
      pendingSelections.delete(requestId);
      setEditor({ isGeneratingAiMask: pendingSelections.size > 0 });
    }
  };

  const paintDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The sub-mask this hook's debounce is waiting on, so only it is cleared.
  const paintWaitingRef = useRef<string | null>(null);
  const stopPaintWait = () => {
    if (paintDebounceRef.current) clearTimeout(paintDebounceRef.current);
    paintDebounceRef.current = null;
    if (paintWaitingRef.current && usePendingAiSelections.getState().waiting === paintWaitingRef.current) {
      usePendingAiSelections.setState({ waiting: null });
    }
    paintWaitingRef.current = null;
  };
  useEffect(() => () => stopPaintWait(), []);

  const handleGenerateAiPaintMask = (subMaskId: string, lines: any[]) => {
    // Debounced: painting several strokes in a row batches into ONE SAM
    // run 600ms after the last release, instead of pausing between each.
    // The wait counts as pending, so Generate can't run on the bare strokes.
    stopPaintWait();
    const path = useEditorStore.getState().selectedImage?.path;
    paintWaitingRef.current = subMaskId;
    usePendingAiSelections.setState({ waiting: subMaskId });
    paintDebounceRef.current = setTimeout(() => {
      stopPaintWait();
      if (useEditorStore.getState().selectedImage?.path !== path) return;
      void runAiPaintGeneration(subMaskId, lines);
    }, 600);
  };

  const runAiPaintGeneration = async (subMaskId: string, lines: any[]) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    const findMask = (adj: Adjustments) => [...(adj.masks || []), ...(adj.aiPatches || [])]
      .flatMap((p) => p.subMasks).find((sm) => sm.id === subMaskId);
    const signature = JSON.stringify(lines);
    const subMask = findMask(adjustments);
    if (!subMask || JSON.stringify(subMask.parameters.lines) !== signature) return;
    const geometryOf = (adj: Adjustments) => JSON.stringify([getTransformAdjustments(adj), adj.rotation,
      adj.flipHorizontal, adj.flipVertical, adj.orientationSteps]);
    const geometry = geometryOf(adjustments);
    const requestId = uuidv4();
    updateSubMask(subMaskId, { parameters: { ...subMask.parameters, subjectRequestId: requestId } });
    pendingSelections.add(requestId);
    startSelectionRun(subMaskId);
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters: any = await invoke(Invokes.GenerateAiPaintMask, {
        jsAdjustments: transformAdjustments,
        lines,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        path: selectedImage.path,
        rotation: adjustments.rotation,
      });

      const current = useEditorStore.getState();
      const latest = findMask(current.adjustments);
      if (current.selectedImage?.path !== selectedImage.path || latest?.parameters.subjectRequestId !== requestId
        || JSON.stringify(latest.parameters.lines) !== signature || geometryOf(current.adjustments) !== geometry) return;
      // Keep the strokes so painting more refines the same selection.
      const mergedParameters = { ...latest.parameters, ...newParameters, lines };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Paint Failed: ${error}`);
    } finally {
      endSelectionRun(subMaskId);
      pendingSelections.delete(requestId);
      setEditor({ isGeneratingAiMask: pendingSelections.size > 0 });
    }
  };

  const handleGenerateAiDepthMask = async (subMaskId: string, parameters: any) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke('generate_ai_depth_mask', {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        minDepth: parameters.minDepth ?? 20,
        maxDepth: parameters.maxDepth ?? 100,
        minFade: parameters.minFade ?? 15,
        maxFade: parameters.maxFade ?? 15,
        feather: parameters.feather ?? 10,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
      });

      const subMask = adjustments.aiPatches
        ?.flatMap((p: AiPatch) => p.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);
      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Depth Mask Failed: ${error}`);
    } finally {
      setEditor({ isGeneratingAiMask: false });
    }
  };

  const handleGenerateAiAutoSubjectMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    const subMask = [...(adjustments.masks || []), ...(adjustments.aiPatches || [])]
      .flatMap((p) => p.subMasks).find((sm) => sm.id === subMaskId);
    if (!subMask) return;
    const requestId = uuidv4();
    const transformAdjustments = getTransformAdjustments(adjustments);
    const geometry = JSON.stringify([transformAdjustments, adjustments.rotation, adjustments.flipHorizontal,
      adjustments.flipVertical, adjustments.orientationSteps]);
    updateSubMask(subMaskId, { parameters: { ...subMask.parameters, subjectRequestId: requestId } });
    pendingSelections.add(requestId);
    startSelectionRun(subMaskId);
    setEditor({ isGeneratingAiMask: true });
    try {
      const result: any = await invoke(Invokes.GenerateAiAutoSubjectMask, {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        rotation: adjustments.rotation,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
      });
      const current = useEditorStore.getState();
      const latest = [...(current.adjustments.masks || []), ...(current.adjustments.aiPatches || [])]
        .flatMap((p) => p.subMasks).find((sm) => sm.id === subMaskId);
      if (current.selectedImage?.path !== selectedImage.path || latest?.parameters.subjectRequestId !== requestId) return;
      if (!result?.found) {
        toast.info('No clear subject found. Click on the subject to select it.');
        return;
      }
      const mergedParameters = {
        ...latest.parameters,
        ...result.parameters,
        subjectPoints: result.subjectPoints,
        subjectGeometry: geometry,
      };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Mask Failed: ${error}`);
    } finally {
      endSelectionRun(subMaskId);
      pendingSelections.delete(requestId);
      setEditor({ isGeneratingAiMask: pendingSelections.size > 0 });
    }
  };

  const handleGenerateAiForegroundMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    startSelectionRun(subMaskId);
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const geometry = JSON.stringify([transformAdjustments, adjustments.rotation, adjustments.flipHorizontal,
        adjustments.flipVertical, adjustments.orientationSteps]);
      // Foreground is measured against a subject. Prefer the Subject mask the
      // user already made (same container first, then any), if it was made
      // on the current geometry; otherwise the backend finds one itself.
      const containers = [...(adjustments.masks || [])];
      const own = containers.find((c: MaskContainer) => c.subMasks.some((sm: SubMask) => sm.id === subMaskId));
      const ordered = own ? [own, ...containers.filter((c) => c !== own)] : containers;
      const subject = ordered
        .flatMap((c: MaskContainer) => c.subMasks)
        .find((sm: SubMask) => sm.type === 'ai-subject' && sm.parameters?.maskDataBase64
          && sm.parameters?.subjectGeometry === geometry);

      const newParameters: any = await invoke(Invokes.GenerateAiForegroundMask, {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        subjectMaskBase64: subject?.parameters.maskDataBase64 ?? null,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
      });

      const { declined, ...persisted } = newParameters ?? {};
      if (declined) toast.info(declined);
      const current = useEditorStore.getState().adjustments;
      const subMask = [...(current.masks || []), ...(current.aiPatches || [])]
        .flatMap((p: any) => p.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);
      const mergedParameters = { ...(subMask?.parameters || {}), ...persisted };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Mask Failed: ${error}`);
    } finally {
      endSelectionRun(subMaskId);
      setEditor({ isGeneratingAiMask: false });
    }
  };

  const handleGenerateAiSkyMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke(Invokes.GenerateAiSkyMask, {
        jsAdjustments: transformAdjustments,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
      });

      if ((newParameters as any)?.coverage === 0) toast.info('No sky found in this photo.');
      const subMask = adjustments.aiPatches
        ?.flatMap((p: AiPatch) => p.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);
      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Mask Failed: ${error}`);
    } finally {
      setEditor({ isGeneratingAiMask: false });
    }
  };

  useEffect(() => {
    const { activeMaskId, activeAiSubMaskId, adjustments, selectedImage } = useEditorStore.getState();
    const activeSubMask =
      adjustments?.masks?.flatMap((m: MaskContainer) => m.subMasks).find((sm: SubMask) => sm.id === activeMaskId) ||
      adjustments?.aiPatches?.flatMap((p: AiPatch) => p.subMasks).find((sm: SubMask) => sm.id === activeAiSubMaskId);

    if (activeSubMask?.type === 'ai-subject' && selectedImage?.path) {
      const transformAdjustments = getTransformAdjustments(adjustments);
      invoke('precompute_ai_subject_mask', {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
      }).catch((err) => console.error('Failed to precompute AI subject mask:', err));
    }
  }, [
    useEditorStore.getState().activeMaskId,
    useEditorStore.getState().activeAiSubMaskId,
    useEditorStore.getState().selectedImage?.path,
  ]);

  return {
    updateSubMask,
    handleGenerativeReplace,
    handleReblendReplacement,
    handleAdjustFillArea,
    handleCloneStamp,
    handleSpotEnhance,
    handleRespotEnhance,
    handleGenerateAiPaintMask,
    handleQuickErase,
    handleDeleteMaskContainer,
    handleDeleteAiPatch,
    handleToggleAiPatchVisibility,
    handleSelectAiPatchVariant,
    handleGenerateAiMask,
    handleGenerateAiDepthMask,
    handleGenerateAiForegroundMask,
    handleGenerateAiSkyMask,
    handleGenerateAiAutoSubjectMask,
  };
}
