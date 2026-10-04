import { create } from 'zustand';
import { currentColorEngine } from '../utils/currentColorEngine';
import { Adjustments, INITIAL_ADJUSTMENTS, MaskContainer, AiPatch } from '../utils/adjustments';
import { SelectedImage, WaveformData, BrushSettings } from '../components/ui/AppProperties';
import { ChannelConfig } from '../components/adjustments/Curves';
import { ImageDimensions } from '../hooks/useImageRenderSize';
import { ToolType } from '../components/panel/right/Masks';
import { OverlayMode } from '../components/panel/right/CropPanel';

export interface InteractivePatch {
  url: string;
  normX: number;
  normY: number;
  normW: number;
  normH: number;
}

interface EditorState {
  colorV3Error: string | null;
  maskMatteView: boolean;
  setMaskMatteView: (v: boolean) => void;
  // Core Image & Adjustments
  selectedImage: SelectedImage | null;
  adjustments: Adjustments;
  previewOverride: Adjustments | null;

  // History State
  history: Adjustments[];
  historyIndex: number;

  // Previews & Overlays
  finalPreviewUrl: string | null;
  uncroppedAdjustedPreviewUrl: string | null;
  transformedOriginalUrl: string | null;
  interactivePatch: InteractivePatch | null;
  /** Zoomed in: the visible region rendered at the size it is shown at,
   * drawn over the whole-picture preview (placed like a patch). */
  zoomTile: InteractivePatch | null;
  /** When panning or zooming last came to rest (the zoom tile follows it). */
  viewSettledAt: number;
  showOriginal: boolean;

  // Analytics
  histogram: ChannelConfig | null;
  waveform: WaveformData | null;
  isWaveformVisible: boolean;
  activeWaveformChannel: string;
  waveformHeight: number;

  // Interaction State
  isSliderDragging: boolean;
  zoom: number;
  displaySize: ImageDimensions;
  previewSize: ImageDimensions;
  baseRenderSize: ImageDimensions;
  originalSize: ImageDimensions;

  // Tools State
  isRotationActive: boolean;
  overlayMode: OverlayMode;
  overlayRotation: number;
  isStraightenActive: boolean;
  isWbPickerActive: boolean;
  // Color Mixer eyedropper: click the photo to pick which HSL band to edit.
  isMixerPickerActive: boolean;
  mixerPickedColor: { hue: number; sat: number; val: number } | null;
  liveRotation: number | null;
  brushSettings: BrushSettings | null;

  // Masks & AI
  activeMaskContainerId: string | null;
  activeMaskId: string | null;
  activeAiPatchContainerId: string | null;
  activeAiSubMaskId: string | null;
  // Patch row under the cursor — the red overlay shows on hover instead
  // of permanently covering the finished edit.
  hoveredAiPatchId: string | null;
  // Sky Replace's detected sky (a mask data URL), shown in red on the
  // canvas while a sky is being chosen.
  skyMaskOverlay: string | null;
  // Set by Sky Replace while a sky is on the photo: dragging on the canvas
  // moves it. Deltas are fractions of the whole photo's width and height.
  skyDrag: ((dx: number, dy: number, done: boolean) => void) | null;
  isMaskControlHovered: boolean;
  isGeneratingAiMask: boolean;
  isGeneratingAi: boolean;
  isAIConnectorConnected: boolean;
  hasRenderedFirstFrame: boolean;
  patchesSentToBackend: Set<string>;

  // Clipboard
  copiedSectionAdjustments: any | null;
  copiedMask: MaskContainer | null;
  copiedAdjustments: Adjustments | null;

  // Actions
  setEditor: (updater: Partial<EditorState> | ((state: EditorState) => Partial<EditorState>)) => void;
  pushHistory: (newAdjustments: Adjustments) => void;
  undo: () => void;
  redo: () => void;
  resetHistory: (initialState: Adjustments) => void;
  goToHistoryIndex: (index: number) => void;
}

export const useEditorStore = create<EditorState>((set) => ({
  selectedImage: null,
  adjustments: INITIAL_ADJUSTMENTS,
  previewOverride: null,
  history: [INITIAL_ADJUSTMENTS],
  historyIndex: 0,

  finalPreviewUrl: null,
  colorV3Error: null,
  uncroppedAdjustedPreviewUrl: null,
  showOriginal: false,
  histogram: null,
  waveform: null,
  isWaveformVisible: false,
  activeWaveformChannel: 'luma',
  waveformHeight: 220,

  isSliderDragging: false,
  interactivePatch: null,
  zoomTile: null,
  viewSettledAt: 0,
  activeMaskContainerId: null,
  activeMaskId: null,
  activeAiPatchContainerId: null,
  activeAiSubMaskId: null,
  hoveredAiPatchId: null,
  skyMaskOverlay: null,
  skyDrag: null,

  zoom: 1,
  maskMatteView: false,
  setMaskMatteView: (v: boolean) => set({ maskMatteView: v }),
  displaySize: { width: 0, height: 0 },
  previewSize: { width: 0, height: 0 },
  baseRenderSize: { width: 0, height: 0 },
  originalSize: { width: 0, height: 0 },

  isRotationActive: false,
  overlayMode: 'thirds',
  overlayRotation: 0,
  transformedOriginalUrl: null,
  isStraightenActive: false,
  isWbPickerActive: false,
  isMixerPickerActive: false,
  mixerPickedColor: null,
  liveRotation: null,

  copiedSectionAdjustments: null,
  copiedMask: null,
  brushSettings: { size: 50, feather: 50, tool: ToolType.Brush },
  copiedAdjustments: null,

  isGeneratingAiMask: false,
  isAIConnectorConnected: false,
  isGeneratingAi: false,
  isMaskControlHovered: false,
  hasRenderedFirstFrame: false,
  patchesSentToBackend: new Set<string>(),

  setEditor: (updater) =>
    set((state) => {
      const update = typeof updater === 'function' ? updater(state) : updater;
      return {
        ...update,
        ...(update.adjustments ? { adjustments: currentColorEngine(update.adjustments) } : {}),
        ...(update.previewOverride ? { previewOverride: currentColorEngine(update.previewOverride) } : {}),
      };
    }),

  pushHistory: (newAdj) =>
    set((state) => {
      const newHistory = state.history.slice(0, state.historyIndex + 1);
      newHistory.push(currentColorEngine(newAdj));
      if (newHistory.length > 50) newHistory.shift();
      return { history: newHistory, historyIndex: newHistory.length - 1 };
    }),

  undo: () =>
    set((state) => {
      if (state.historyIndex > 0) {
        const newIndex = state.historyIndex - 1;
        return { historyIndex: newIndex, adjustments: currentColorEngine(state.history[newIndex]) };
      }
      return state;
    }),

  redo: () =>
    set((state) => {
      if (state.historyIndex < state.history.length - 1) {
        const newIndex = state.historyIndex + 1;
        return { historyIndex: newIndex, adjustments: currentColorEngine(state.history[newIndex]) };
      }
      return state;
    }),

  resetHistory: (initialState) =>
    set({
      history: [currentColorEngine(initialState)],
      historyIndex: 0,
      adjustments: currentColorEngine(initialState),
    }),

  goToHistoryIndex: (index) =>
    set((state) => {
      if (index >= 0 && index < state.history.length) {
        return { historyIndex: index, adjustments: currentColorEngine(state.history[index]) };
      }
      return state;
    }),
}));
