import { useEffect, useRef } from 'react';
import { ImageFile, Panel, ExifOverlay, FolderPanelMode, folderPanelMode } from '../components/ui/AppProperties';
import { KEYBIND_DEFINITIONS, normalizeCombo } from '../utils/keyboardUtils';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { useEditorActions } from './useEditorActions';
import { useLibraryActions } from './useLibraryActions';
import { getStacks, withVersions } from './useStacks';
import { stackRootOf } from '../utils/stacks';
import { undoFileOperation } from '../utils/fileUndo';
import { flagTargets, setFlagForPaths } from '../utils/flags';
import { startCulling } from '../components/panel/library/CullView';
import { useShortcutSheet } from '../components/modals/ShortcutSheet';
import { useEditorRequests } from '../store/useEditorRequests';
import { resolveEditorLayout } from '../utils/editorLayout';
import { LIBRARY_REFRESH_EVENT } from '../utils/stacks';
import { Invokes } from '../components/ui/AppProperties';
import { invoke } from '@tauri-apps/api/core';
import type { OverlayMode } from '../components/panel/right/CropPanel';

/** The stores, as the shortcut handlers read them. */
interface StoreState {
  editor: ReturnType<typeof useEditorStore.getState>;
  library: ReturnType<typeof useLibraryStore.getState>;
  ui: ReturnType<typeof useUIStore.getState>;
  settings: ReturnType<typeof useSettingsStore.getState>;
  process: ReturnType<typeof useProcessStore.getState>;
}

interface KeyboardShortcutsProps {
  sortedImageList: Array<ImageFile>;
  handleBackToLibrary(): void;
  handleDeleteSelected(): void;
  handleImageSelect(path: string): void;
  handlePasteFiles(str: string): void;
  handleToggleFullScreen(): void;
  handleZoomChange(zoomValue: number, fitToWindow?: boolean): void;
}

export const useKeyboardShortcuts = ({
  sortedImageList,
  handleBackToLibrary,
  handleDeleteSelected,
  handleImageSelect,
  handlePasteFiles,
  handleToggleFullScreen,
  handleZoomChange,
}: KeyboardShortcutsProps) => {
  const { handleRotate, handleCopyAdjustments, handlePasteAdjustments, handleAutoAdjustments, setAdjustments } =
    useEditorActions();
  const { handleRate, handleSetColorLabel } = useLibraryActions();

  const sortedListRef = useRef(sortedImageList);
  useEffect(() => {
    sortedListRef.current = sortedImageList;
  }, [sortedImageList]);

  useEffect(() => {
    const getStoreState = () => ({
      editor: useEditorStore.getState(),
      library: useLibraryStore.getState(),
      ui: useUIStore.getState(),
      settings: useSettingsStore.getState(),
      process: useProcessStore.getState(),
    });

    // A key can carry more than one action (\\ shows the original in the
    // editor and toggles the filter bar in the grid): the first whose
    // moment it is runs. An action keeps its extra keys (aliases) while it
    // keeps its default.
    const comboMap = new Map<string, string[]>();
    const keybinds = useSettingsStore.getState().appSettings?.keybinds;
    const bind = (combo: string[], action: string) => {
      if (combo.length === 0) return;
      const key = combo.join('+');
      comboMap.set(key, [...(comboMap.get(key) ?? []), action]);
    };

    for (const def of KEYBIND_DEFINITIONS) {
      const userCombo = keybinds?.[def.action];
      if (userCombo && userCombo.length > 0) {
        bind(userCombo, def.action);
      } else {
        bind(def.defaultCombo, def.action);
        def.aliases?.forEach((alias) => bind(alias, def.action));
      }
    }

    /** Open a right-hand panel (setRightPanel toggles; this only opens). */
    const openPanel = (s: StoreState, panel: Panel) => {
      if (s.ui.activeRightPanel !== panel) s.ui.setRightPanel(panel);
    };

    /** Lightroom's ⌘1–6: open (or, when all are open, close) Advanced sections. */
    const toggleSections = (s: StoreState, ids: string[], onlyOpen = false) => {
      const { appSettings, handleSettingsChange } = s.settings;
      if (!appSettings) return;
      const layout = resolveEditorLayout((appSettings as unknown as { editorLayout?: unknown }).editorLayout as never);
      const allOpen =
        layout.adjustmentsMode === 'advanced' &&
        s.ui.activeRightPanel === Panel.Adjustments &&
        ids.every((id) => layout.openSections[id]);
      const opening = onlyOpen || !allOpen;
      const openSections = { ...layout.openSections };
      ids.forEach((id) => (openSections[id] = opening));
      // (editorLayout is not in the AppSettings type; see useEditorLayout.)
      const next = {
        ...appSettings,
        editorLayout: {
          ...layout,
          adjustmentsMode: 'advanced',
          openSections,
          hiddenSections: layout.hiddenSections.filter((h: string) => !ids.includes(h)),
        },
      };
      void handleSettingsChange(next);
      openPanel(s, Panel.Adjustments);
      if (opening) {
        window.setTimeout(
          () =>
            document
              .querySelector(`[data-adjustment-section="${ids[0]}"]`)
              ?.scrollIntoView({ block: 'start', behavior: 'smooth' }),
          120,
        );
      }
    };

    /** Open a merge window on the selection, as the context menu does. */
    const openMerge = (s: StoreState, which: 'hdrModalState' | 'panoramaModalState') => {
      s.ui.setUI({
        [which]: {
          error: null,
          finalImageBase64: null,
          isOpen: true,
          isProcessing: false,
          progressMessage: null,
          stitchingSourcePaths: s.library.multiSelectedPaths,
        },
      });
    };

    const overlays: OverlayMode[] = [
      'thirds',
      'goldenTriangle',
      'goldenSpiral',
      'phiGrid',
      'armature',
      'diagonal',
      'none',
    ];
    const inBrushPanel = (s: StoreState) =>
      !!s.editor.selectedImage &&
      !!s.editor.brushSettings &&
      (s.ui.activeRightPanel === Panel.Masks || s.ui.activeRightPanel === Panel.Ai);

    const actions: Record<string, any> = {
      open_image: {
        shouldFire: (s: any) => !s.editor.selectedImage && s.library.libraryActivePath !== null,
        execute: (e: any, s: any) => {
          e.preventDefault();
          handleImageSelect(s.library.libraryActivePath!);
        },
      },
      copy_adjustments: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleCopyAdjustments();
        },
      },
      paste_adjustments: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handlePasteAdjustments();
        },
      },
      copy_files: {
        shouldFire: (s: any) => s.library.multiSelectedPaths.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.process.setProcess({ copiedFilePaths: withVersions(s.library.multiSelectedPaths) });
        },
      },
      paste_files: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handlePasteFiles('copy');
        },
      },
      select_all: {
        shouldFire: () => sortedListRef.current.length > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.library.setLibrary({ multiSelectedPaths: sortedListRef.current.map((f: ImageFile) => f.path) });
          if (!s.editor.selectedImage) {
            s.library.setLibrary({ libraryActivePath: sortedListRef.current[sortedListRef.current.length - 1].path });
          }
        },
      },
      delete_selected: {
        shouldFire: (s: any) => !s.editor.activeMaskContainerId && !s.editor.activeAiPatchContainerId,
        execute: (e: any) => {
          e.preventDefault();
          handleDeleteSelected();
        },
      },
      preview_prev: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          // While a version is open, move on from its stack's tile.
          const current = stackRootOf(getStacks(s.library.imageList), s.editor.selectedImage!.path);
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === current);
          if (currentIndex === -1) return;
          let nextIndex = currentIndex - 1 < 0 ? sortedListRef.current.length - 1 : currentIndex - 1;
          handleImageSelect(sortedListRef.current[nextIndex].path);
        },
      },
      preview_next: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          // While a version is open, move on from its stack's tile.
          const current = stackRootOf(getStacks(s.library.imageList), s.editor.selectedImage!.path);
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === current);
          if (currentIndex === -1) return;
          let nextIndex = currentIndex + 1 >= sortedListRef.current.length ? 0 : currentIndex + 1;
          handleImageSelect(sortedListRef.current[nextIndex].path);
        },
      },
      zoom_in_step: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.min(currentPercent + 0.1, 2.0));
        },
      },
      zoom_out_step: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.max(currentPercent - 0.1, 0.1));
        },
      },
      cycle_zoom: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const { originalSize, displaySize, baseRenderSize } = s.editor;
          const currentPercent =
            originalSize?.width > 0 && displaySize?.width > 0
              ? Math.round(((displaySize.width * dpr) / originalSize.width) * 100)
              : 100;
          let fitPercent = 100;

          if (originalSize?.width > 0 && baseRenderSize?.width > 0) {
            const originalAspect = originalSize.width / originalSize.height;
            const baseAspect = baseRenderSize.width / baseRenderSize.height;
            fitPercent =
              originalAspect > baseAspect
                ? Math.round(((baseRenderSize.width * dpr) / originalSize.width) * 100)
                : Math.round(((baseRenderSize.height * dpr) / originalSize.height) * 100);
          }

          const doubleFitPercent = fitPercent * 2;
          if (Math.abs(currentPercent - fitPercent) < 5) {
            handleZoomChange(doubleFitPercent < 100 ? doubleFitPercent / 100 : 1.0);
          } else if (Math.abs(currentPercent - doubleFitPercent) < 5 && doubleFitPercent < 100) {
            handleZoomChange(1.0);
          } else {
            handleZoomChange(0, true);
          }
        },
      },
      zoom_in: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.min(currentPercent * 1.2, 2.0));
        },
      },
      zoom_out: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
          const currentPercent =
            s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
              ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
              : 1.0;
          handleZoomChange(Math.max(currentPercent / 1.2, 0.1));
        },
      },
      zoom_fit: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleZoomChange(0, true);
        },
      },
      zoom_100: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleZoomChange(1.0);
        },
      },
      rotate_left: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleRotate(-90);
        },
      },
      rotate_right: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleRotate(90);
        },
      },
      undo: {
        // In the editor it undoes edits; in the library, file operations.
        shouldFire: (s: any) => !s.editor.selectedImage || s.editor.historyIndex > 0,
        execute: (e: any, s: any) => {
          e.preventDefault();
          if (s.editor.selectedImage) s.editor.undo();
          else undoFileOperation();
        },
      },
      redo: {
        shouldFire: (s: any) => !!s.editor.selectedImage && s.editor.historyIndex < s.editor.history.length - 1,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.editor.redo();
        },
      },
      toggle_fullscreen: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleToggleFullScreen();
        },
      },
      show_original: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.editor.setEditor({ showOriginal: !s.editor.showOriginal });
        },
      },
      toggle_adjustments: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Adjustments);
        },
      },
      toggle_crop_panel: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Crop);
        },
      },
      toggle_masks: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Masks);
        },
      },
      toggle_ai: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Ai);
        },
      },
      flag_pick: {
        shouldFire: () => flagTargets().length > 0,
        execute: (e: any) => {
          e.preventDefault();
          setFlagForPaths(flagTargets(), 'pick');
        },
      },
      flag_reject: {
        shouldFire: () => flagTargets().length > 0,
        execute: (e: any) => {
          e.preventDefault();
          setFlagForPaths(flagTargets(), 'reject');
        },
      },
      flag_clear: {
        shouldFire: () => flagTargets().length > 0,
        execute: (e: any) => {
          e.preventDefault();
          setFlagForPaths(flagTargets(), null);
        },
      },
      open_cull: {
        shouldFire: (s: any) => !s.editor.selectedImage && sortedListRef.current.length > 0,
        execute: (e: any) => {
          e.preventDefault();
          startCulling(sortedListRef.current);
        },
      },
      cycle_folder_panel: {
        // Kept open → pops out at the edge → collapsed → kept open.
        shouldFire: () => true,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setUI((state: any) => {
            const order: FolderPanelMode[] = ['pinned', 'auto', 'collapsed'];
            const next = order[(order.indexOf(folderPanelMode(state.uiVisibility)) + 1) % order.length];
            return { uiVisibility: { ...state.uiVisibility, folderTreeMode: next, folderTree: next === 'pinned' } };
          });
        },
      },
      toggle_filter_bar: {
        shouldFire: (s: any) => !s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setUI((state: any) => ({ isFilterBarVisible: !state.isFilterBarVisible }));
        },
      },
      back_to_grid: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any) => {
          e.preventDefault();
          handleBackToLibrary();
        },
      },
      toggle_presets: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Presets);
        },
      },
      toggle_metadata: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Metadata);
        },
      },
      toggle_analytics: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.editor.setEditor({ isWaveformVisible: !s.editor.isWaveformVisible });
        },
      },
      toggle_export: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Export);
        },
      },
      toggle_library_exif: {
        shouldFire: (s: any) => !s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const current = s.settings.appSettings?.exifOverlay || ExifOverlay.Off;
          const nextState = {
            [ExifOverlay.Off]: ExifOverlay.Hover,
            [ExifOverlay.Hover]: ExifOverlay.Always,
            [ExifOverlay.Always]: ExifOverlay.Off,
          }[current as ExifOverlay];
          s.settings.handleSettingsChange({ ...s.settings.appSettings, exifOverlay: nextState });
        },
      },
      toggle_crop: {
        shouldFire: (s: any) => !!s.editor.selectedImage,
        execute: (e: any, s: any) => {
          e.preventDefault();
          if (s.ui.activeRightPanel === Panel.Crop) {
            s.editor.setEditor({ isStraightenActive: !s.editor.isStraightenActive });
          } else {
            s.ui.setRightPanel(Panel.Crop);
            s.editor.setEditor({ isStraightenActive: true });
          }
        },
      },
      rate_0: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(0);
        },
      },
      rate_1: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(1);
        },
      },
      rate_2: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(2);
        },
      },
      rate_3: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(3);
        },
      },
      rate_4: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(4);
        },
      },
      rate_5: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleRate(5);
        },
      },
      color_label_none: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel(null);
        },
      },
      color_label_red: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('red');
        },
      },
      color_label_yellow: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('yellow');
        },
      },
      color_label_green: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('green');
        },
      },
      color_label_blue: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('blue');
        },
      },
      color_label_purple: {
        shouldFire: () => true,
        execute: (e: any) => {
          e.preventDefault();
          handleSetColorLabel('purple');
        },
      },
      brush_size_up: {
        shouldFire: inBrushPanel,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const newSize = Math.min((s.editor.brushSettings.size || 50) + 10, 200);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings, size: newSize } });
        },
      },
      brush_size_down: {
        shouldFire: inBrushPanel,
        execute: (e: any, s: any) => {
          e.preventDefault();
          const newSize = Math.max((s.editor.brushSettings.size || 50) - 10, 1);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings, size: newSize } });
        },
      },
      // Lightroom's (the Lightroom desktop app's) own shortcuts.
      brush_feather_up: {
        shouldFire: inBrushPanel,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          const feather = Math.min((s.editor.brushSettings!.feather ?? 50) + 10, 100);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings!, feather } });
        },
      },
      brush_feather_down: {
        shouldFire: inBrushPanel,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          const feather = Math.max((s.editor.brushSettings!.feather ?? 50) - 10, 0);
          s.editor.setEditor({ brushSettings: { ...s.editor.brushSettings!, feather } });
        },
      },
      auto_tone: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent) => {
          e.preventDefault();
          handleAutoAdjustments();
        },
      },
      toggle_clipping: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent) => {
          e.preventDefault();
          setAdjustments((prev) => ({ ...prev, showClipping: !prev.showClipping }));
        },
      },
      wb_picker: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Adjustments);
          s.editor.setEditor({ isWbPickerActive: !s.editor.isWbPickerActive });
        },
      },
      crop_overlay_cycle: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage && s.ui.activeRightPanel === Panel.Crop,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          const next = overlays[(overlays.indexOf(s.editor.overlayMode) + 1) % overlays.length];
          s.editor.setEditor({ overlayMode: next });
        },
      },
      mask_linear: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Masks);
          useEditorRequests.getState().send({ kind: 'mask', mask: 'linear' });
        },
      },
      mask_radial: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Masks);
          useEditorRequests.getState().send({ kind: 'mask', mask: 'radial' });
        },
      },
      mask_brush: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Masks);
          useEditorRequests.getState().send({ kind: 'mask', mask: 'brush' });
        },
      },
      heal_brush: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Ai);
          useEditorRequests.getState().send({ kind: 'heal' });
        },
      },
      create_version: {
        shouldFire: (s: StoreState) => !!(s.editor.selectedImage?.path ?? s.library.libraryActivePath),
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          const source = s.editor.selectedImage?.path ?? s.library.libraryActivePath;
          invoke(Invokes.CreateVirtualCopy, {
            sourceVirtualPath: source,
            targetAlbumId: s.library.activeAlbumId || null,
          })
            .then(() => window.dispatchEvent(new Event(LIBRARY_REFRESH_EVENT)))
            .catch((err) => console.error('Could not create a version:', err));
        },
      },
      paste_selected: {
        shouldFire: (s: StoreState) => !!s.editor.copiedAdjustments,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          s.ui.setUI({ isCopyPasteSettingsModalOpen: true });
        },
      },
      export_previous: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openPanel(s, Panel.Export);
          useEditorRequests.getState().send({ kind: 'exportPrevious' });
        },
      },
      merge_hdr: {
        shouldFire: (s: StoreState) => s.library.multiSelectedPaths.length >= 2,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openMerge(s, 'hdrModalState');
        },
      },
      merge_pano: {
        shouldFire: (s: StoreState) => s.library.multiSelectedPaths.length >= 2,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          openMerge(s, 'panoramaModalState');
        },
      },
      select_none: {
        shouldFire: (s: StoreState) => !s.editor.selectedImage && s.library.multiSelectedPaths.length > 0,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          s.library.setLibrary({ multiSelectedPaths: [] });
        },
      },
      toggle_filmstrip: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          s.ui.setUI((state) => ({
            uiVisibility: { ...state.uiVisibility, filmstrip: !state.uiVisibility.filmstrip },
          }));
        },
      },
      panel_detail: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['detail'], true);
        },
      },
      panel_light: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['light', 'toneCurve']);
        },
      },
      panel_color: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['whiteBalance', 'presence', 'colorMixer', 'colorGrading']);
        },
      },
      panel_effects: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['effects']);
        },
      },
      panel_details: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['detail']);
        },
      },
      panel_optics: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          toggleSections(s, ['optics']);
        },
      },
      panel_geometry: {
        shouldFire: (s: StoreState) => !!s.editor.selectedImage,
        execute: (e: KeyboardEvent, s: StoreState) => {
          e.preventDefault();
          s.ui.setRightPanel(Panel.Crop);
        },
      },
      shortcut_sheet: {
        shouldFire: () => true,
        execute: (e: KeyboardEvent) => {
          e.preventDefault();
          const sheet = useShortcutSheet.getState();
          sheet.setOpen(!sheet.open);
        },
      },
    };

    const builtinShortcuts = [
      {
        match: (e: KeyboardEvent) => e.code === 'Escape',
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          if (s.editor.isStraightenActive) s.editor.setEditor({ isStraightenActive: false });
          else if (s.ui.customEscapeHandler) s.ui.customEscapeHandler();
          else if (s.editor.activeAiSubMaskId) s.editor.setEditor({ activeAiSubMaskId: null });
          else if (s.editor.activeAiPatchContainerId) s.editor.setEditor({ activeAiPatchContainerId: null });
          else if (s.editor.activeMaskId) s.editor.setEditor({ activeMaskId: null });
          else if (s.editor.activeMaskContainerId) s.editor.setEditor({ activeMaskContainerId: null });
          else if (s.ui.activeRightPanel === Panel.Crop) s.ui.setRightPanel(Panel.Adjustments);
          else if (s.ui.isFullScreen) handleToggleFullScreen();
          else if (s.editor.selectedImage) handleBackToLibrary();
        },
      },
      {
        match: (e: KeyboardEvent, s: any) => {
          const isDeleteKey = s.settings.osPlatform === 'macos' ? e.code === 'Backspace' : e.code === 'Delete';
          return isDeleteKey && (!!s.editor.activeMaskContainerId || !!s.editor.activeAiPatchContainerId);
        },
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          if (s.editor.activeMaskContainerId) {
            s.editor.setEditor((state: any) => ({
              adjustments: {
                ...state.adjustments,
                masks: state.adjustments.masks.filter((c: any) => c.id !== s.editor.activeMaskContainerId),
              },
              activeMaskContainerId: null,
              activeMaskId: null,
            }));
          } else if (s.editor.activeAiPatchContainerId) {
            s.editor.setEditor((state: any) => ({
              adjustments: {
                ...state.adjustments,
                aiPatches: state.adjustments.aiPatches.filter((c: any) => c.id !== s.editor.activeAiPatchContainerId),
              },
              activeAiPatchContainerId: null,
              activeAiSubMaskId: null,
            }));
          }
        },
      },
      {
        match: (e: KeyboardEvent, s: any) =>
          !s.editor.selectedImage && ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.code),
        execute: (e: KeyboardEvent, s: any) => {
          e.preventDefault();
          const isNext = e.code === 'ArrowRight' || e.code === 'ArrowDown';
          const activePath = s.library.libraryActivePath;
          if (!activePath || sortedListRef.current.length === 0) return;
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === activePath);
          if (currentIndex === -1) return;
          let nextIndex = isNext ? currentIndex + 1 : currentIndex - 1;
          if (nextIndex >= sortedListRef.current.length) nextIndex = 0;
          if (nextIndex < 0) nextIndex = sortedListRef.current.length - 1;
          const nextImage = sortedListRef.current[nextIndex];
          if (nextImage) {
            s.library.setLibrary({ libraryActivePath: nextImage.path, multiSelectedPaths: [nextImage.path] });
          }
        },
      },
    ];

    const handleKeyDown = (event: KeyboardEvent) => {
      const state = getStoreState();

      const isModalOpen =
        state.ui.isCreateFolderModalOpen ||
        state.ui.isRenameFolderModalOpen ||
        state.ui.isRenameFileModalOpen ||
        state.ui.isImportModalOpen ||
        state.ui.isCopyPasteSettingsModalOpen ||
        state.ui.confirmModalState.isOpen ||
        state.ui.panoramaModalState.isOpen ||
        state.ui.cullingModalState.isOpen ||
        state.ui.collageModalState.isOpen ||
        state.ui.denoiseModalState.isOpen ||
        state.ui.negativeModalState.isOpen;

      if (isModalOpen) return;

      const isInputFocused =
        document.activeElement?.tagName === 'INPUT' || document.activeElement?.tagName === 'TEXTAREA';
      if (isInputFocused) return;

      for (const builtin of builtinShortcuts) {
        if (builtin.match(event, state)) {
          builtin.execute(event, state);
          return;
        }
      }

      const normalized = normalizeCombo(event, state.settings.osPlatform);
      for (const action of comboMap.get(normalized.join('+')) ?? []) {
        const handler = actions[action];
        if (handler && (!handler.shouldFire || handler.shouldFire(state))) {
          handler.execute(event, state);
          return;
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [
    handleBackToLibrary,
    handleDeleteSelected,
    handleImageSelect,
    handlePasteFiles,
    handleToggleFullScreen,
    handleZoomChange,
    handleRotate,
    handleCopyAdjustments,
    handlePasteAdjustments,
    handleRate,
    handleSetColorLabel,
    handleAutoAdjustments,
    setAdjustments,
  ]);
};
