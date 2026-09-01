import { useRef, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useEditorStore } from '../store/useEditorStore';
import { useEditorActions } from './useEditorActions';
import { Adjustments, AiPatch, AiPatchResultVariant, MaskContainer, Coord } from '../utils/adjustments';
import { SubMask } from '../components/panel/right/Masks';
import { Invokes } from '../components/ui/AppProperties';
import { useAuth } from '@clerk/react';

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
        aiPatches: (prev.aiPatches || []).map((p: AiPatch) => ({
          ...p,
          subMasks: p.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
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
    ) => {
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

      // Threaded explicitly rather than read back from the store: the panel
      // calls updateContainer immediately before this, and that state is not
      // visible to getState() yet.
      const patchDefinition = { ...patch, prompt, reconstructSinglePath, generateMode };

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
        const newPatchDataJson: any = await invoke(Invokes.InvokeGenerativeReplaseWithMaskDef, {
          currentAdjustments: adjustments,
          patchDefinition: patchDefinition,
          path: selectedImage.path,
          useFastInpaint: useFastInpaint,
          token: token || null,
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
                  reconstructSinglePath,
                  name: useFastInpaint ? 'Inpaint' : prompt && prompt.trim() ? prompt.trim() : p.name,
                }
              : p,
          ),
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
        // The preview strips patchData for any patch the backend has
        // already cached, and hydrate_adjustments then fills the gap from
        // that cache. Without this the next redraw rebuilt the PREVIOUS
        // heal — every other AI path drops the id here for the same reason.
        patchesSentToBackend.delete(patchId);
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          aiPatches: prev.aiPatches.map((p: AiPatch) =>
            p.id === patchId ? { ...p, patchData, isLoading: false, name: isHeal ? 'Heal' : 'Clone' } : p,
          ),
        }));
        // Heal is meant to be worked at: keep the brush pointed at the same
        // sub-mask so more strokes can be added and the source re-dragged,
        // each edit re-running the blend. Clone stays one-shot.
        if (!isHeal) {
          setEditor({ activeAiSubMaskId: null });
        }
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
              reconstructVariants: variants,
            },
          };
        }),
      }));
    },
    [setAdjustments],
  );

  const handleGenerateAiMask = async (subMaskId: string, startPoint: Coord, endPoint: Coord) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke(Invokes.GenerateAiSubjectMask, {
        jsAdjustments: transformAdjustments,
        endPoint: [endPoint.x, endPoint.y],
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        path: selectedImage.path,
        rotation: adjustments.rotation,
        startPoint: [startPoint.x, startPoint.y],
      });

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

  const paintDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleGenerateAiPaintMask = (subMaskId: string, lines: any[]) => {
    // Debounced: painting several strokes in a row batches into ONE SAM
    // run 600ms after the last release, instead of pausing between each.
    if (paintDebounceRef.current) clearTimeout(paintDebounceRef.current);
    paintDebounceRef.current = setTimeout(() => {
      void runAiPaintGeneration(subMaskId, lines);
    }, 600);
  };

  const runAiPaintGeneration = async (subMaskId: string, lines: any[]) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
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

      const subMask = [...(adjustments.masks || []), ...(adjustments.aiPatches || [])]
        .flatMap((p: any) => p.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);
      // Keep the strokes so painting more refines the same selection.
      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters, lines };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toast.error(`AI Paint Failed: ${error}`);
    } finally {
      setEditor({ isGeneratingAiMask: false });
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

  const handleGenerateAiForegroundMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    setEditor({ isGeneratingAiMask: true });

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke(Invokes.GenerateAiForegroundMask, {
        jsAdjustments: transformAdjustments,
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
      toast.error(`AI Mask Failed: ${error}`);
    } finally {
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
  };
}
