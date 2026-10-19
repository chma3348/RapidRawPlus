import { Crop } from 'react-image-crop';
import { currentColorEngine } from './currentColorEngine';
import { defaultV3Controls, type V3Controls } from './colorV3';
import { v4 as uuidv4 } from 'uuid';
import { SubMask, SubMaskMode } from '../components/panel/right/Masks';

export enum ActiveChannel {
  Blue = 'blue',
  Green = 'green',
  Luma = 'luma',
  Red = 'red',
}

export enum DisplayMode {
  Luma = 'luma',
  Rgb = 'rgb',
  Parade = 'parade',
  Vectorscope = 'vectorscope',
  Histogram = 'histogram',
}

export enum PasteMode {
  Merge = 'merge',
  Replace = 'replace',
}

export interface CopyPasteSettings {
  mode: PasteMode;
  includedAdjustments: Array<string>;
  knownAdjustments: Array<string>;
}

export enum BasicAdjustment {
  Blacks = 'blacks',
  Brightness = 'brightness',
  Contrast = 'contrast',
  Exposure = 'exposure',
  Highlights = 'highlights',
  Shadows = 'shadows',
  Whites = 'whites',
}

export enum ColorAdjustment {
  ColorGrading = 'colorGrading',
  Hsl = 'hsl',
  Hue = 'hue',
  Luminance = 'luminance',
  Saturation = 'saturation',
  Temperature = 'temperature',
  Tint = 'tint',
  Vibrance = 'vibrance',
}

export enum ColorGrading {
  Balance = 'balance',
  Blending = 'blending',
  Global = 'global',
  Highlights = 'highlights',
  Midtones = 'midtones',
  Shadows = 'shadows',
}

export enum DetailsAdjustment {
  Clarity = 'clarity',
  Dehaze = 'dehaze',
  Structure = 'structure',
  Centré = 'centré',
  ColorNoiseReduction = 'colorNoiseReduction',
  LumaNoiseReduction = 'lumaNoiseReduction',
  Sharpness = 'sharpness',
  SharpnessThreshold = 'sharpnessThreshold',
  ChromaticAberrationRedCyan = 'chromaticAberrationRedCyan',
  ChromaticAberrationBlueYellow = 'chromaticAberrationBlueYellow',
}

export enum Effect {
  GrainAmount = 'grainAmount',
  GrainRoughness = 'grainRoughness',
  GrainSize = 'grainSize',
  LutData = 'lutData',
  LutIntensity = 'lutIntensity',
  LutName = 'lutName',
  LutPath = 'lutPath',
  LutSize = 'lutSize',
  VignetteAmount = 'vignetteAmount',
  VignetteFeather = 'vignetteFeather',
  VignetteMidpoint = 'vignetteMidpoint',
  VignetteRoundness = 'vignetteRoundness',
}

export enum CreativeAdjustment {
  GlowAmount = 'glowAmount',
  HalationAmount = 'halationAmount',
  FlareAmount = 'flareAmount',
  FilmSaturation = 'filmSaturation',
}

export enum TransformAdjustment {
  TransformDistortion = 'transformDistortion',
  TransformVertical = 'transformVertical',
  TransformHorizontal = 'transformHorizontal',
  TransformRotate = 'transformRotate',
  TransformAspect = 'transformAspect',
  TransformScale = 'transformScale',
  TransformXOffset = 'transformXOffset',
  TransformYOffset = 'transformYOffset',
}

export enum LensAdjustment {
  LensCorrectionMode = 'lensCorrectionMode',
  LensMaker = 'lensMaker',
  LensModel = 'lensModel',
  LensDistortionAmount = 'lensDistortionAmount',
  LensVignetteAmount = 'lensVignetteAmount',
  LensTcaAmount = 'lensTcaAmount',
  LensDistortionParams = 'lensDistortionParams',
  LensDistortionEnabled = 'lensDistortionEnabled',
  LensTcaEnabled = 'lensTcaEnabled',
  LensVignetteEnabled = 'lensVignetteEnabled',
}

export interface ColorCalibration {
  shadowsTint: number;
  redHue: number;
  redSaturation: number;
  greenHue: number;
  greenSaturation: number;
  blueHue: number;
  blueSaturation: number;
}

export interface ParametricCurveSettings {
  darks: number;
  shadows: number;
  highlights: number;
  lights: number;
  whiteLevel: number;
  blackLevel: number;
  split1: number;
  split2: number;
  split3: number;
}

export interface ParametricCurve {
  [index: string]: ParametricCurveSettings;
  blue: ParametricCurveSettings;
  green: ParametricCurveSettings;
  luma: ParametricCurveSettings;
  red: ParametricCurveSettings;
}

export interface Adjustments {
  [index: string]: any;
  aiPatches: Array<AiPatch>;
  aspectRatio: number | null;
  blacks: number;
  brightness: number;
  centré: number;
  clarity: number;
  chromaticAberrationBlueYellow: number;
  chromaticAberrationRedCyan: number;
  colorCalibration: ColorCalibration;
  colorGrading: ColorGradingProps;
  colorNoiseReduction: number;
  contrast: number;
  curves: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
  crop: Crop | null;
  dehaze: number;
  exposure: number;
  flipHorizontal: boolean;
  flipVertical: boolean;
  flareAmount: number;
  glowAmount: number;
  grainAmount: number;
  grainRoughness: number;
  grainSize: number;
  halationAmount: number;
  filmSaturation: number;
  pointColors: Array<{
    hue: number;
    sat: number;
    val: number;
    range: number;
    hueShift: number;
    satShift: number;
    lumShift: number;
  }>;
  highlights: number;
  hsl: Hsl;
  hue: number;
  lensCorrectionMode: 'auto' | 'manual';
  lensDistortionAmount: number;
  lensVignetteAmount: number;
  lensTcaAmount: number;
  lensDistortionEnabled: boolean;
  lensTcaEnabled: boolean;
  lensVignetteEnabled: boolean;
  lensDistortionParams: {
    k1: number;
    k2: number;
    k3: number;
    model: number;
    tca_vr: number;
    tca_vb: number;
    vig_k1: number;
    vig_k2: number;
    vig_k3: number;
  } | null;
  lensMaker: string | null;
  lensModel: string | null;
  lumaNoiseReduction: number;
  flatFieldProfile?: string | null;
  flatFieldStrength?: number;
  lutData?: string | null;
  lutIntensity?: number;
  lutName?: string | null;
  lutPath?: string | null;
  lutInputSpace?: 'display' | 'flog2c' | 'intermediate';
  lutSimExposure?: number;
  lutSize?: number;
  masks: Array<MaskContainer>;
  orientationSteps: number;
  rotation: number;
  saturation: number;
  sectionVisibility: SectionVisibility;
  shadows: number;
  sharpness: number;
  sharpnessThreshold: number;
  showClipping: boolean;
  structure: number;
  temperature: number;
  tint: number;
  toneMapper: 'agx' | 'basic' | 'filmic' | 'resolve';
  processVersion?: number;
  v3?: V3Controls;
  v3Pipeline?: import('./colorV3').V3PipelineIdentity;
  v3RawRecovery?: 'off' | 'neutral_green_v1';
  /** Diagnostic source snapshot refreshed by the backend when saving. */
  v3Input?: Record<string, unknown>;
  transformDistortion: number;
  transformVertical: number;
  transformHorizontal: number;
  transformRotate: number;
  transformAspect: number;
  transformScale: number;
  transformXOffset: number;
  transformYOffset: number;
  vibrance: number;
  vignetteAmount: number;
  vignetteFeather: number;
  vignetteMidpoint: number;
  vignetteRoundness: number;
  whites: number;
}

export interface AiPatch {
  id: string;
  isLoading: boolean;
  invert: boolean;
  name: string;
  patchData: any | null;
  prompt: string;
  reconstructSinglePath?: boolean;
  /// Build the fill from the prompt alone instead of continuing the
  /// surrounding photograph — for blown-out areas that carry nothing to
  /// continue.
  generateMode?: boolean;
  /// Generate mode: how much more content to make than the region needs, so
  /// only the middle is used and forms land larger. 1.0 = fit exactly.
  contentScale?: number;
  /// Generate mode: 0..1, how far the result is moved toward the tone of the
  /// photograph around the selection.
  matchPhoto?: number;
  /// LoRAs to apply, in order. Flux only.
  loras?: Array<{ name: string; strength: number }>;
  /// Set when a promptless repair found too little surrounding detail to
  /// rebuild from; the value is the percentage it did find.
  needsPromptReason?: number;
  subMasks: Array<SubMask>;
  visible: boolean;
  /** 'heal' patches clone from a source offset and stay editable; 'sky'
   *  patches come from Sky Replace and remember how they were made. */
  patchType?: 'heal' | 'sky';
  sky?: { plate: string; options: Record<string, number | boolean> };
  /** Where the heal copies FROM, relative to the painted area, in source px. */
  cloneOffset?: { x: number; y: number };
  /** Post-render blend controls, applied at composite time. */
  feather?: number;
  opacity?: number;
}

export interface AiPatchResultVariant {
  id: string;
  label: string;
  kind: 'ai' | 'fallback' | 'raw-ai' | string;
  color: string;
  mask: string;
  encoding?: 'gamma' | 'linear' | string;
  prompt?: string;
  debugRunId?: string;
  debugDir?: string | null;
  createdAt?: number;
}

export interface Color {
  color: string;
  name: string;
}

interface ColorGradingProps {
  [index: string]: number | HueSatLum;
  balance: number;
  blending: number;
  global: HueSatLum;
  highlights: HueSatLum;
  midtones: HueSatLum;
  shadows: HueSatLum;
}

export interface Coord {
  x: number;
  y: number;
}

export interface Curves {
  [index: string]: Array<Coord>;
  blue: Array<Coord>;
  green: Array<Coord>;
  luma: Array<Coord>;
  red: Array<Coord>;
}

export interface HueSatLum {
  hue: number;
  saturation: number;
  luminance: number;
}

interface Hsl {
  [index: string]: HueSatLum;
  aquas: HueSatLum;
  blues: HueSatLum;
  greens: HueSatLum;
  magentas: HueSatLum;
  oranges: HueSatLum;
  purples: HueSatLum;
  reds: HueSatLum;
  yellows: HueSatLum;
}

export interface MaskAdjustments {
  [index: string]: any;
  blacks: number;
  brightness: number;
  clarity: number;
  colorGrading: ColorGradingProps;
  colorNoiseReduction: number;
  contrast: number;
  curves: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
  dehaze: number;
  exposure: number;
  flareAmount: number;
  glowAmount: number;
  halationAmount: number;
  filmSaturation: number;
  pointColors: Array<{
    hue: number;
    sat: number;
    val: number;
    range: number;
    hueShift: number;
    satShift: number;
    lumShift: number;
  }>;
  highlights: number;
  hsl: Hsl;
  hue: number;
  id?: string;
  lumaNoiseReduction: number;
  saturation: number;
  sectionVisibility: SectionVisibility;
  shadows: number;
  sharpness: number;
  sharpnessThreshold: number;
  structure: number;
  temperature: number;
  tint: number;
  vibrance: number;
  whites: number;
}

export interface MaskContainer {
  /** Generated fill whose footprint this adjustment mask follows. */
  sourceAiPatchId?: string;
  sourceAiSubMaskId?: string;
  adjustments: MaskAdjustments;
  id?: any;
  invert: boolean;
  name: string;
  opacity: number;
  subMasks: Array<SubMask>;
  visible: boolean;
}

export interface Sections {
  [index: string]: Array<string>;
  basic: Array<string>;
  curves: Array<string>;
  color: Array<string>;
  details: Array<string>;
  effects: Array<string>;
}

export interface SectionVisibility {
  [index: string]: boolean;
  basic: boolean;
  curves: boolean;
  color: boolean;
  details: boolean;
  effects: boolean;
}

export const COLOR_LABELS: Array<Color> = [
  { name: 'red', color: '#ef4444' },
  { name: 'yellow', color: '#facc15' },
  { name: 'green', color: '#4ade80' },
  { name: 'blue', color: '#60a5fa' },
  { name: 'purple', color: '#a78bfa' },
];

const INITIAL_COLOR_GRADING: ColorGradingProps = {
  balance: 0,
  blending: 50,
  global: { hue: 0, saturation: 0, luminance: 0 },
  highlights: { hue: 0, saturation: 0, luminance: 0 },
  midtones: { hue: 0, saturation: 0, luminance: 0 },
  shadows: { hue: 0, saturation: 0, luminance: 0 },
};

const INITIAL_COLOR_CALIBRATION: ColorCalibration = {
  shadowsTint: 0,
  redHue: 0,
  redSaturation: 0,
  greenHue: 0,
  greenSaturation: 0,
  blueHue: 0,
  blueSaturation: 0,
};

export const DEFAULT_PARAMETRIC_CURVE_SETTINGS: ParametricCurveSettings = {
  darks: 0,
  shadows: 0,
  highlights: 0,
  lights: 0,
  whiteLevel: 0,
  blackLevel: 0,
  split1: 25,
  split2: 50,
  split3: 75,
};

export const getDefaultParametricCurve = (): ParametricCurve => ({
  luma: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  red: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  green: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  blue: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
});

export const getDefaultCurves = (): Curves => ({
  blue: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  green: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  luma: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  red: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
});

export const DEFAULT_PARAMETRIC_CURVE = getDefaultParametricCurve();

export const INITIAL_MASK_ADJUSTMENTS: MaskAdjustments = {
  blacks: 0,
  brightness: 0,
  clarity: 0,
  colorGrading: { ...INITIAL_COLOR_GRADING },
  colorNoiseReduction: 0,
  contrast: 0,
  curves: getDefaultCurves(),
  pointCurves: getDefaultCurves(),
  parametricCurve: getDefaultParametricCurve(),
  curveMode: 'point',
  dehaze: 0,
  exposure: 0,
  flareAmount: 0,
  glowAmount: 0,
  halationAmount: 0,
  filmSaturation: 0,
  pointColors: [],
  highlights: 0,
  hsl: {
    aquas: { hue: 0, saturation: 0, luminance: 0 },
    blues: { hue: 0, saturation: 0, luminance: 0 },
    greens: { hue: 0, saturation: 0, luminance: 0 },
    magentas: { hue: 0, saturation: 0, luminance: 0 },
    oranges: { hue: 0, saturation: 0, luminance: 0 },
    purples: { hue: 0, saturation: 0, luminance: 0 },
    reds: { hue: 0, saturation: 0, luminance: 0 },
    yellows: { hue: 0, saturation: 0, luminance: 0 },
  },
  hue: 0,
  lumaNoiseReduction: 0,
  saturation: 0,
  sectionVisibility: {
    basic: true,
    curves: true,
    color: true,
    details: true,
    effects: true,
  },
  shadows: 0,
  sharpness: 0,
  sharpnessThreshold: 15,
  structure: 0,
  temperature: 0,
  texture: 0,
  tint: 0,
  vibrance: 0,
  whites: 0,
};

export const INITIAL_MASK_CONTAINER: MaskContainer = {
  adjustments: INITIAL_MASK_ADJUSTMENTS,
  invert: false,
  name: 'New Mask',
  opacity: 100,
  subMasks: [],
  visible: true,
};

export const INITIAL_ADJUSTMENTS: Adjustments = {
  aiPatches: [],
  aspectRatio: null,
  blacks: 0,
  brightness: 0,
  centré: 0,
  clarity: 0,
  chromaticAberrationBlueYellow: 0,
  chromaticAberrationRedCyan: 0,
  colorCalibration: { ...INITIAL_COLOR_CALIBRATION },
  colorGrading: { ...INITIAL_COLOR_GRADING },
  colorWheels: {
    lift: { hue: 0, saturation: 0, luminance: 0 },
    gamma: { hue: 0, saturation: 0, luminance: 0 },
    gain: { hue: 0, saturation: 0, luminance: 0 },
    offset: { hue: 0, saturation: 0, luminance: 0 },
  },
  colorNoiseReduction: 0,
  contrast: 0,
  contrastPivot: 50,
  crop: null,
  curves: getDefaultCurves(),
  pointCurves: getDefaultCurves(),
  parametricCurve: getDefaultParametricCurve(),
  curveMode: 'point',
  dehaze: 0,
  exposure: 0,
  flipHorizontal: false,
  flipVertical: false,
  flareAmount: 0,
  glowAmount: 0,
  grainAmount: 0,
  grainRoughness: 50,
  grainSize: 25,
  halationAmount: 0,
  filmSaturation: 0,
  pointColors: [],
  highlights: 0,
  hsl: {
    aquas: { hue: 0, saturation: 0, luminance: 0 },
    blues: { hue: 0, saturation: 0, luminance: 0 },
    greens: { hue: 0, saturation: 0, luminance: 0 },
    magentas: { hue: 0, saturation: 0, luminance: 0 },
    oranges: { hue: 0, saturation: 0, luminance: 0 },
    purples: { hue: 0, saturation: 0, luminance: 0 },
    reds: { hue: 0, saturation: 0, luminance: 0 },
    yellows: { hue: 0, saturation: 0, luminance: 0 },
  },
  hue: 0,
  hueCurves: { hueHue: [], hueSat: [], hueLum: [], lumSat: [], satSat: [] },
  lensCorrectionMode: 'manual',
  lensDistortionAmount: 100,
  lensVignetteAmount: 100,
  lensTcaAmount: 100,
  lensDistortionEnabled: true,
  lensTcaEnabled: true,
  lensVignetteEnabled: true,
  lensDistortionParams: null,
  lensMaker: null,
  lensModel: null,
  lumaNoiseReduction: 0,
  flatFieldProfile: null,
  flatFieldStrength: 100,
  lutData: null,
  lutIntensity: 100,
  lutName: null,
  lutPath: null,
  lutInputSpace: 'display',
  lutSimExposure: 0,
  lutSize: 0,
  masks: [],
  orientationSteps: 0,
  processVersion: 3,
  rotation: 0,
  saturation: 0,
  sectionVisibility: {
    basic: true,
    curves: true,
    color: true,
    details: true,
    effects: true,
  },
  shadows: 0,
  sharpness: 0,
  sharpnessThreshold: 15,
  showClipping: false,
  structure: 0,
  temperature: 0,
  texture: 0,
  tint: 0,
  toneMapper: 'resolve',
  transformDistortion: 0,
  transformVertical: 0,
  transformHorizontal: 0,
  transformRotate: 0,
  transformAspect: 0,
  transformScale: 100,
  transformXOffset: 0,
  transformYOffset: 0,
  vibrance: 0,
  vignetteAmount: 0,
  vignetteFeather: 50,
  vignetteMidpoint: 50,
  vignetteRoundness: 0,
  whites: 0,
};

const deepCloneCurves = (curves: any): Curves => ({
  blue: curves?.blue?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  green: curves?.green?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  luma: curves?.luma?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  red: curves?.red?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
});

const deepCloneParametric = (pCurve: any): ParametricCurve => ({
  luma: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.luma || {}) },
  red: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.red || {}) },
  green: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.green || {}) },
  blue: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.blue || {}) },
});

export const normalizeLoadedAdjustments = (loadedAdjustments: Adjustments): any => {
  if (!loadedAdjustments) {
    return INITIAL_ADJUSTMENTS;
  }
  loadedAdjustments = currentColorEngine(loadedAdjustments);

  const normalizeSubMasks = (subMasks: any[]) => {
    return (subMasks || []).map((subMask: Partial<SubMask>) => ({
      visible: true,
      mode: SubMaskMode.Additive,
      invert: false,
      opacity: 100,
      ...subMask,
    }));
  };

  const normalizedMasks = (loadedAdjustments.masks || []).map((maskContainer: MaskContainer) => {
    const containerAdjustments = maskContainer.adjustments || {};
    const normalizedSubMasks = normalizeSubMasks(maskContainer.subMasks);

    return {
      ...INITIAL_MASK_CONTAINER,
      id: maskContainer.id || uuidv4(),
      ...maskContainer,
      adjustments: {
        ...INITIAL_MASK_ADJUSTMENTS,
        ...containerAdjustments,
        flareAmount: containerAdjustments.flareAmount ?? INITIAL_MASK_ADJUSTMENTS.flareAmount,
        glowAmount: containerAdjustments.glowAmount ?? INITIAL_MASK_ADJUSTMENTS.glowAmount,
        halationAmount: containerAdjustments.halationAmount ?? INITIAL_MASK_ADJUSTMENTS.halationAmount,
        hue: containerAdjustments.hue ?? INITIAL_MASK_ADJUSTMENTS.hue,
        colorGrading: { ...INITIAL_MASK_ADJUSTMENTS.colorGrading, ...(containerAdjustments.colorGrading || {}) },
        hsl: { ...INITIAL_MASK_ADJUSTMENTS.hsl, ...(containerAdjustments.hsl || {}) },
        curves: containerAdjustments.curves ? deepCloneCurves(containerAdjustments.curves) : getDefaultCurves(),
        pointCurves: containerAdjustments.pointCurves
          ? deepCloneCurves(containerAdjustments.pointCurves)
          : getDefaultCurves(),
        parametricCurve: containerAdjustments.parametricCurve
          ? deepCloneParametric(containerAdjustments.parametricCurve)
          : getDefaultParametricCurve(),
        curveMode: containerAdjustments.curveMode || INITIAL_MASK_ADJUSTMENTS.curveMode,
        sectionVisibility: {
          ...INITIAL_MASK_ADJUSTMENTS.sectionVisibility,
          ...(containerAdjustments.sectionVisibility || {}),
        },
        sharpnessThreshold: containerAdjustments.sharpnessThreshold ?? INITIAL_MASK_ADJUSTMENTS.sharpnessThreshold,
      },
      subMasks: normalizedSubMasks,
    };
  });

  const normalizedAiPatches = (loadedAdjustments.aiPatches || []).map((patch: any) => ({
    visible: true,
    ...patch,
    subMasks: normalizeSubMasks(patch.subMasks),
  }));

  return {
    ...INITIAL_ADJUSTMENTS,
    ...loadedAdjustments,
    flareAmount: loadedAdjustments.flareAmount ?? INITIAL_ADJUSTMENTS.flareAmount,
    glowAmount: loadedAdjustments.glowAmount ?? INITIAL_ADJUSTMENTS.glowAmount,
    halationAmount: loadedAdjustments.halationAmount ?? INITIAL_ADJUSTMENTS.halationAmount,
    filmSaturation: loadedAdjustments.filmSaturation ?? 0,
    pointColors: loadedAdjustments.pointColors ?? [],
    lensCorrectionMode: loadedAdjustments.lensCorrectionMode || 'manual',
    lensMaker: loadedAdjustments.lensMaker ?? INITIAL_ADJUSTMENTS.lensMaker,
    lensModel: loadedAdjustments.lensModel ?? INITIAL_ADJUSTMENTS.lensModel,
    lensDistortionAmount: loadedAdjustments.lensDistortionAmount ?? INITIAL_ADJUSTMENTS.lensDistortionAmount,
    lensVignetteAmount: loadedAdjustments.lensVignetteAmount ?? INITIAL_ADJUSTMENTS.lensVignetteAmount,
    lensTcaAmount: loadedAdjustments.lensTcaAmount ?? INITIAL_ADJUSTMENTS.lensTcaAmount,
    lensDistortionEnabled: loadedAdjustments.lensDistortionEnabled ?? INITIAL_ADJUSTMENTS.lensDistortionEnabled,
    lensTcaEnabled: loadedAdjustments.lensTcaEnabled ?? INITIAL_ADJUSTMENTS.lensTcaEnabled,
    lensVignetteEnabled: loadedAdjustments.lensVignetteEnabled ?? INITIAL_ADJUSTMENTS.lensVignetteEnabled,
    lensDistortionParams: loadedAdjustments.lensDistortionParams ?? INITIAL_ADJUSTMENTS.lensDistortionParams,
    transformDistortion: loadedAdjustments.transformDistortion ?? INITIAL_ADJUSTMENTS.transformDistortion,
    transformVertical: loadedAdjustments.transformVertical ?? INITIAL_ADJUSTMENTS.transformVertical,
    transformHorizontal: loadedAdjustments.transformHorizontal ?? INITIAL_ADJUSTMENTS.transformHorizontal,
    transformRotate: loadedAdjustments.transformRotate ?? INITIAL_ADJUSTMENTS.transformRotate,
    transformAspect: loadedAdjustments.transformAspect ?? INITIAL_ADJUSTMENTS.transformAspect,
    transformScale: loadedAdjustments.transformScale ?? INITIAL_ADJUSTMENTS.transformScale,
    transformXOffset: loadedAdjustments.transformXOffset ?? INITIAL_ADJUSTMENTS.transformXOffset,
    transformYOffset: loadedAdjustments.transformYOffset ?? INITIAL_ADJUSTMENTS.transformYOffset,
    colorCalibration: { ...INITIAL_ADJUSTMENTS.colorCalibration, ...(loadedAdjustments.colorCalibration || {}) },
    colorGrading: { ...INITIAL_ADJUSTMENTS.colorGrading, ...(loadedAdjustments.colorGrading || {}) },
    hsl: { ...INITIAL_ADJUSTMENTS.hsl, ...(loadedAdjustments.hsl || {}) },
    curves: loadedAdjustments.curves ? deepCloneCurves(loadedAdjustments.curves) : getDefaultCurves(),
    pointCurves: loadedAdjustments.pointCurves ? deepCloneCurves(loadedAdjustments.pointCurves) : getDefaultCurves(),
    parametricCurve: loadedAdjustments.parametricCurve
      ? deepCloneParametric(loadedAdjustments.parametricCurve)
      : getDefaultParametricCurve(),
    curveMode: loadedAdjustments.curveMode || INITIAL_ADJUSTMENTS.curveMode,
    masks: normalizedMasks,
    aiPatches: normalizedAiPatches,
    sectionVisibility: {
      ...INITIAL_ADJUSTMENTS.sectionVisibility,
      ...(loadedAdjustments.sectionVisibility || {}),
    },
    sharpnessThreshold: loadedAdjustments.sharpnessThreshold ?? INITIAL_ADJUSTMENTS.sharpnessThreshold,
    contrastPivot: loadedAdjustments.contrastPivot ?? INITIAL_ADJUSTMENTS.contrastPivot,
    // Development-era sidecars adopt the only active renderer.
    processVersion: loadedAdjustments.processVersion ?? 3,
    texture: loadedAdjustments.texture ?? INITIAL_ADJUSTMENTS.texture,
    colorWheels: loadedAdjustments.colorWheels ?? INITIAL_ADJUSTMENTS.colorWheels,
    hueCurves: { ...INITIAL_ADJUSTMENTS.hueCurves, ...(loadedAdjustments.hueCurves ?? {}) },
  };
};

export interface AdjustmentGroup {
  label: string;
  keys: string[];
}

export const ADJUSTMENT_GROUPS: Record<string, AdjustmentGroup[]> = {
  basic: [
    {
      label: 'modals.copyPaste.groups.exposureToneMapper',
      keys: [BasicAdjustment.Exposure, 'toneMapper'],
    },
    {
      label: 'modals.copyPaste.groups.tone',
      keys: [
        BasicAdjustment.Brightness,
        BasicAdjustment.Contrast,
        'contrastPivot',
        BasicAdjustment.Highlights,
        BasicAdjustment.Shadows,
        BasicAdjustment.Whites,
        BasicAdjustment.Blacks,
      ],
    },
    {
      label: 'modals.copyPaste.groups.curves',
      keys: ['curves', 'pointCurves', 'parametricCurve', 'curveMode'],
    },
  ],
  color: [
    { label: 'modals.copyPaste.groups.whiteBalance', keys: [ColorAdjustment.Temperature, ColorAdjustment.Tint] },
    { label: 'modals.copyPaste.groups.presence', keys: [ColorAdjustment.Saturation, ColorAdjustment.Vibrance] },
    {
      label: 'modals.copyPaste.groups.hueShift',
      keys: [ColorAdjustment.Hue],
    },
    { label: 'modals.copyPaste.groups.colorGrading', keys: [ColorAdjustment.ColorGrading] },
    { label: 'modals.copyPaste.groups.colorWheels', keys: ['colorWheels'] },
    { label: 'modals.copyPaste.groups.hueCurves', keys: ['hueCurves'] },
    { label: 'modals.copyPaste.groups.colorMixer', keys: [ColorAdjustment.Hsl] },
    { label: 'modals.copyPaste.groups.colorCalibration', keys: ['colorCalibration'] },
  ],
  details: [
    {
      label: 'modals.copyPaste.groups.clarityDehaze',
      keys: [
        DetailsAdjustment.Clarity,
        DetailsAdjustment.Structure,
        DetailsAdjustment.Dehaze,
        DetailsAdjustment.Centré,
      ],
    },
    {
      label: 'modals.copyPaste.groups.sharpness',
      keys: [DetailsAdjustment.Sharpness, DetailsAdjustment.SharpnessThreshold],
    },
    {
      label: 'modals.copyPaste.groups.noiseReduction',
      keys: [DetailsAdjustment.LumaNoiseReduction, DetailsAdjustment.ColorNoiseReduction],
    },
    {
      label: 'modals.copyPaste.groups.chromaticAberration',
      keys: [DetailsAdjustment.ChromaticAberrationRedCyan, DetailsAdjustment.ChromaticAberrationBlueYellow],
    },
  ],
  effects: [
    {
      label: 'modals.copyPaste.groups.vignette',
      keys: [Effect.VignetteAmount, Effect.VignetteFeather, Effect.VignetteMidpoint, Effect.VignetteRoundness],
    },
    { label: 'modals.copyPaste.groups.grain', keys: [Effect.GrainAmount, Effect.GrainRoughness, Effect.GrainSize] },
    {
      label: 'modals.copyPaste.groups.halationGlow',
      keys: [CreativeAdjustment.GlowAmount, CreativeAdjustment.HalationAmount, CreativeAdjustment.FlareAmount],
    },
    {
      label: 'modals.copyPaste.groups.lut',
      keys: [Effect.LutIntensity, Effect.LutName, Effect.LutPath, Effect.LutSize, Effect.LutData],
    },
  ],
  geometry: [
    { label: 'modals.copyPaste.groups.cropAspectRatio', keys: ['crop', 'aspectRatio'] },
    {
      label: 'modals.copyPaste.groups.transformRotation',
      keys: [
        'rotation',
        'flipHorizontal',
        'flipVertical',
        'orientationSteps',
        TransformAdjustment.TransformDistortion,
        TransformAdjustment.TransformVertical,
        TransformAdjustment.TransformHorizontal,
        TransformAdjustment.TransformRotate,
        TransformAdjustment.TransformAspect,
        TransformAdjustment.TransformScale,
        TransformAdjustment.TransformXOffset,
        TransformAdjustment.TransformYOffset,
      ],
    },
    {
      label: 'modals.copyPaste.groups.lensCorrection',
      keys: [
        LensAdjustment.LensCorrectionMode,
        LensAdjustment.LensMaker,
        LensAdjustment.LensModel,
        LensAdjustment.LensDistortionAmount,
        LensAdjustment.LensVignetteAmount,
        LensAdjustment.LensTcaAmount,
        LensAdjustment.LensDistortionEnabled,
        LensAdjustment.LensTcaEnabled,
        LensAdjustment.LensVignetteEnabled,
      ],
    },
  ],
  masks: [{ label: 'modals.copyPaste.groups.masks', keys: ['masks'] }],
};

// `v3` holds the new engine's own settings for many of these choices at
// once: it is always copied, and pasted part by part (see `v3ForPaste`).
export const COPYABLE_ADJUSTMENT_KEYS: string[] = [
  ...Object.values(ADJUSTMENT_GROUPS)
    .flat()
    .flatMap((group) => group.keys),
  'v3',
];

/**
 * Which paste choice (a key in ADJUSTMENT_GROUPS) carries each of v3's own
 * settings: Warmth goes with White balance, v3 sharpening with Sharpness,
 * and so on, so the choices mean the same in either engine.
 */
const V3_PASTE_CHOICES = {
  top: {
    temperature: ColorAdjustment.Temperature,
    tint: ColorAdjustment.Tint,
    saturation: ColorAdjustment.Saturation,
    vibrance: ColorAdjustment.Vibrance,
    hue: ColorAdjustment.Hue,
    bands: ColorAdjustment.Hsl,
    ranges: ColorAdjustment.Hsl,
    grading: ColorAdjustment.ColorGrading,
    curve: 'curves',
    channel_curves: 'curves',
    calibration: 'colorCalibration',
  },
  detail: {
    sharpening: DetailsAdjustment.Sharpness,
    sharpen_radius: DetailsAdjustment.Sharpness,
    sharpen_detail: DetailsAdjustment.Sharpness,
    sharpen_masking: DetailsAdjustment.Sharpness,
    threshold: DetailsAdjustment.SharpnessThreshold,
    texture: DetailsAdjustment.Clarity,
    clarity: DetailsAdjustment.Clarity,
    structure: DetailsAdjustment.Structure,
    dehaze: DetailsAdjustment.Dehaze,
    luminance_noise: DetailsAdjustment.LumaNoiseReduction,
    luminance_noise_detail: DetailsAdjustment.LumaNoiseReduction,
    luminance_noise_contrast: DetailsAdjustment.LumaNoiseReduction,
    color_noise: DetailsAdjustment.ColorNoiseReduction,
    color_noise_detail: DetailsAdjustment.ColorNoiseReduction,
    color_noise_smoothness: DetailsAdjustment.ColorNoiseReduction,
  },
  effects: {
    vignette_amount: Effect.VignetteAmount,
    vignette_midpoint: Effect.VignetteMidpoint,
    vignette_roundness: Effect.VignetteRoundness,
    vignette_feather: Effect.VignetteFeather,
    grain_amount: Effect.GrainAmount,
    grain_size: Effect.GrainSize,
    grain_roughness: Effect.GrainRoughness,
    glow_amount: CreativeAdjustment.GlowAmount,
    halation_amount: CreativeAdjustment.HalationAmount,
    flare_amount: CreativeAdjustment.FlareAmount,
    film_saturation: CreativeAdjustment.GlowAmount,
    ca_red_cyan: DetailsAdjustment.ChromaticAberrationRedCyan,
    ca_blue_yellow: DetailsAdjustment.ChromaticAberrationBlueYellow,
    centre: DetailsAdjustment.Centré,
  },
} as const;

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

type Fields = Record<string, unknown>;
/** Part of a photo's v3 settings, as pasted: detail and effects hold only some of their fields. */
export type V3Paste = Fields & { detail?: Fields; effects?: Fields };

/** `object[path[0]][path[1]]…`, or undefined. */
const at = (object: unknown, ...path: string[]): unknown =>
  path.reduce<unknown>((value, key) => (value as Fields | undefined)?.[key], object);

/**
 * The part of copied v3 settings that `included` pastes: only those
 * settings, so a paste never resets the rest of the photo's v3 settings.
 * Merge pastes only settings changed from their defaults (a RAW's own
 * defaults, Sharpening 40 and Color NR 25, count as unchanged).
 */
export function v3ForPaste(
  source: Partial<V3Controls> | undefined,
  included: string[],
  merge: boolean,
): V3Paste | null {
  if (!source) return null;
  const defaults = [defaultV3Controls(false), defaultV3Controls(true)];
  const wanted = (choice: string, value: unknown, ...path: string[]) =>
    value !== undefined && included.includes(choice) && !(merge && defaults.some((d) => same(at(d, ...path), value)));
  const out: V3Paste = {};
  for (const [field, choice] of Object.entries(V3_PASTE_CHOICES.top)) {
    const value = at(source, field);
    if (wanted(choice, value, field)) out[field] = structuredClone(value);
  }
  for (const part of ['detail', 'effects'] as const) {
    const picked: Fields = {};
    for (const [field, choice] of Object.entries(V3_PASTE_CHOICES[part])) {
      const value = at(source, part, field);
      if (wanted(choice, value, part, field)) picked[field] = value;
    }
    if (Object.keys(picked).length) out[part] = picked;
  }
  return Object.keys(out).length ? out : null;
}

/** The v3 settings that differ between `before` and `after`, one level deeper for detail and effects. */
export function changedV3(before: unknown, after: unknown): Partial<V3Controls> {
  const out: V3Paste = {};
  for (const [key, value] of Object.entries((after ?? {}) as Fields)) {
    if (key === 'detail' || key === 'effects') {
      const part = Object.fromEntries(
        Object.entries((value ?? {}) as Fields).filter(([field, v]) => !same(v, at(before, key, field))),
      );
      if (Object.keys(part).length) out[key] = part;
    } else if (!same(value, at(before, key))) {
      out[key] = value;
    }
  }
  return out as Partial<V3Controls>;
}

/** A photo's v3 settings with pasted ones (from `v3ForPaste`) laid over them. */
export function mergePastedV3(target: V3Controls | undefined, pasted: V3Paste): V3Controls {
  const merged: V3Paste = { ...(target as unknown as Fields | undefined), ...pasted };
  for (const part of ['detail', 'effects'] as const) {
    if (pasted[part]) merged[part] = { ...((target?.[part] ?? {}) as unknown as Fields), ...pasted[part] };
  }
  return merged as unknown as V3Controls;
}

export const ADJUSTMENT_SECTIONS: Sections = {
  basic: [
    BasicAdjustment.Brightness,
    BasicAdjustment.Contrast,
    BasicAdjustment.Highlights,
    BasicAdjustment.Shadows,
    BasicAdjustment.Whites,
    BasicAdjustment.Blacks,
    BasicAdjustment.Exposure,
    'toneMapper',
    'contrastPivot',
    'processVersion',
    'v3',
  ],
  curves: ['curves', 'pointCurves', 'parametricCurve', 'curveMode'],
  color: [
    ColorAdjustment.Saturation,
    ColorAdjustment.Temperature,
    ColorAdjustment.Tint,
    ColorAdjustment.Vibrance,
    ColorAdjustment.Hsl,
    ColorAdjustment.ColorGrading,
    'colorCalibration',
    ColorAdjustment.Hue,
    'colorWheels',
    'hueCurves',
  ],
  details: [
    'texture',
    DetailsAdjustment.Clarity,
    DetailsAdjustment.Dehaze,
    DetailsAdjustment.Structure,
    DetailsAdjustment.Centré,
    DetailsAdjustment.Sharpness,
    DetailsAdjustment.SharpnessThreshold,
    DetailsAdjustment.LumaNoiseReduction,
    DetailsAdjustment.ColorNoiseReduction,
    DetailsAdjustment.ChromaticAberrationRedCyan,
    DetailsAdjustment.ChromaticAberrationBlueYellow,
  ],
  effects: [
    CreativeAdjustment.GlowAmount,
    CreativeAdjustment.HalationAmount,
    CreativeAdjustment.FlareAmount,
    Effect.GrainAmount,
    Effect.GrainRoughness,
    Effect.GrainSize,
    Effect.LutIntensity,
    Effect.LutName,
    Effect.LutPath,
    Effect.LutSize,
    Effect.VignetteAmount,
    Effect.VignetteFeather,
    Effect.VignetteMidpoint,
    Effect.VignetteRoundness,
  ],
};
