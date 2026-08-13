import { useMemo, useRef, useState, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';

export interface HueCurvePoint {
  x: number;
  y: number;
}

type CurveKey = 'hueHue' | 'hueSat' | 'hueLum' | 'lumSat';

interface HueCurvesPanelProps {
  adjustments: Partial<Adjustments>;
  setAdjustments(updater: any): void;
  onDragStateChange?(dragging: boolean): void;
}

const W = 260;
const H = 120;
const PAD = 8;

const CURVE_TABS: Array<{ key: CurveKey; label: string; xMax: number; hueAxis: boolean }> = [
  { key: 'hueHue', label: 'H/H', xMax: 360, hueAxis: true },
  { key: 'hueSat', label: 'H/S', xMax: 360, hueAxis: true },
  { key: 'hueLum', label: 'H/L', xMax: 360, hueAxis: true },
  { key: 'lumSat', label: 'L/S', xMax: 100, hueAxis: false },
];

const toSvgX = (x: number, xMax: number) => PAD + (x / xMax) * (W - 2 * PAD);
const toSvgY = (y: number) => H / 2 - (y / 100) * (H / 2 - PAD);
const fromSvgX = (sx: number, xMax: number) => Math.min(xMax, Math.max(0, ((sx - PAD) / (W - 2 * PAD)) * xMax));
const fromSvgY = (sy: number) => Math.min(100, Math.max(-100, ((H / 2 - sy) / (H / 2 - PAD)) * 100));

/** Smoothstep-interpolated polyline matching the shader's evaluation. */
function curvePath(points: HueCurvePoint[], xMax: number): string {
  if (points.length === 0) return `M ${PAD} ${H / 2} L ${W - PAD} ${H / 2}`;
  const sorted = [...points].sort((a, b) => a.x - b.x);
  // Hue-domain wrap: phantom copies across the seam, matching the backend.
  const ext =
    xMax === 360 && sorted.length >= 2
      ? [
          { x: sorted[sorted.length - 1].x - 360, y: sorted[sorted.length - 1].y },
          ...sorted,
          { x: sorted[0].x + 360, y: sorted[0].y },
        ]
      : sorted;
  const samples: string[] = [];
  const eval1 = (x: number): number => {
    if (x <= ext[0].x) return ext[0].y;
    if (x >= ext[ext.length - 1].x) return ext[ext.length - 1].y;
    for (let i = 1; i < ext.length; i++) {
      if (x <= ext[i].x) {
        const span = Math.max(ext[i].x - ext[i - 1].x, 0.0001);
        const t = (x - ext[i - 1].x) / span;
        const ts = t * t * (3 - 2 * t);
        return ext[i - 1].y + (ext[i].y - ext[i - 1].y) * ts;
      }
    }
    return ext[ext.length - 1].y;
  };
  for (let px = 0; px <= 80; px++) {
    const x = (px / 80) * xMax;
    samples.push(`${px === 0 ? 'M' : 'L'} ${toSvgX(x, xMax).toFixed(1)} ${toSvgY(eval1(x)).toFixed(1)}`);
  }
  return samples.join(' ');
}

export default function HueCurvesPanel({ adjustments, setAdjustments, onDragStateChange }: HueCurvesPanelProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<CurveKey>('hueHue');
  const svgRef = useRef<SVGSVGElement>(null);
  const dragIndex = useRef<number | null>(null);

  const meta = CURVE_TABS.find((c) => c.key === tab)!;
  const curves = adjustments.hueCurves ?? INITIAL_ADJUSTMENTS.hueCurves;
  const points: HueCurvePoint[] = useMemo(
    () => [...(curves[tab] ?? [])].sort((a, b) => a.x - b.x),
    [curves, tab],
  );

  const setPoints = useCallback(
    (next: HueCurvePoint[]) => {
      setAdjustments((prev: any) => ({
        ...prev,
        hueCurves: { ...(prev.hueCurves ?? INITIAL_ADJUSTMENTS.hueCurves), [tab]: next },
      }));
    },
    [setAdjustments, tab],
  );

  const svgPoint = (e: React.PointerEvent): { x: number; y: number } | null => {
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) return null;
    const sx = ((e.clientX - rect.left) / rect.width) * W;
    const sy = ((e.clientY - rect.top) / rect.height) * H;
    return { x: fromSvgX(sx, meta.xMax), y: fromSvgY(sy) };
  };

  const handlePointerDown = (e: React.PointerEvent) => {
    const p = svgPoint(e);
    if (!p) return;
    (e.target as Element).setPointerCapture?.(e.pointerId);
    // Grab an existing point when close, otherwise create one.
    const threshold = meta.xMax * 0.04;
    let idx = points.findIndex((pt) => Math.abs(pt.x - p.x) < threshold);
    let next = [...points];
    if (idx === -1) {
      next.push(p);
      next.sort((a, b) => a.x - b.x);
      idx = next.findIndex((pt) => pt.x === p.x);
      if (next.length > 12) return;
      setPoints(next);
    }
    dragIndex.current = idx;
    onDragStateChange?.(true);
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (dragIndex.current === null) return;
    const p = svgPoint(e);
    if (!p) return;
    const next = [...points];
    next[dragIndex.current] = p;
    setPoints(next);
  };

  const endDrag = () => {
    if (dragIndex.current !== null) {
      dragIndex.current = null;
      onDragStateChange?.(false);
    }
  };

  const handleDoubleClick = (e: React.MouseEvent) => {
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) return;
    const sx = ((e.clientX - rect.left) / rect.width) * W;
    const x = fromSvgX(sx, meta.xMax);
    const threshold = meta.xMax * 0.04;
    const idx = points.findIndex((pt) => Math.abs(pt.x - x) < threshold);
    if (idx !== -1) {
      const next = points.filter((_, i) => i !== idx);
      setPoints(next);
    }
  };

  const hasAny = CURVE_TABS.some((c) => (curves[c.key] ?? []).some((p: HueCurvePoint) => p.y !== 0));

  return (
    <div className="p-2 bg-bg-tertiary rounded-md">
      <div className="flex items-center justify-between mb-2">
        <Text variant={TextVariants.heading}>{t('adjustments.color.hueCurves.title')}</Text>
        {hasAny && (
          <button
            className="text-xs text-text-secondary hover:text-text-primary transition-colors"
            onClick={() => setAdjustments((prev: any) => ({ ...prev, hueCurves: INITIAL_ADJUSTMENTS.hueCurves }))}
          >
            {t('adjustments.color.hueCurves.reset')}
          </button>
        )}
      </div>
      <div className="flex gap-1 mb-2">
        {CURVE_TABS.map((c) => (
          <button
            key={c.key}
            onClick={() => setTab(c.key)}
            data-tooltip={t(`adjustments.color.hueCurves.${c.key}`)}
            className={`px-2 py-0.5 rounded text-xs transition-colors ${
              tab === c.key
                ? 'bg-accent text-button-text'
                : (curves[c.key] ?? []).length > 0
                  ? 'bg-card-active text-text-primary'
                  : 'bg-bg-primary text-text-secondary hover:bg-card-active'
            }`}
          >
            {c.label}
          </button>
        ))}
      </div>
      <svg
        ref={svgRef}
        viewBox={`0 0 ${W} ${H}`}
        className="w-full rounded-md select-none touch-none cursor-crosshair"
        style={{ background: 'rgba(0,0,0,0.25)' }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={endDrag}
        onPointerLeave={endDrag}
        onDoubleClick={handleDoubleClick}
      >
        {/* axis strip: hue gradient or luminance ramp */}
        <defs>
          <linearGradient id="hue-strip" x1="0" y1="0" x2="1" y2="0">
            {[0, 60, 120, 180, 240, 300, 360].map((h) => (
              <stop key={h} offset={`${(h / 360) * 100}%`} stopColor={`hsl(${h}, 80%, 55%)`} />
            ))}
          </linearGradient>
          <linearGradient id="lum-strip" x1="0" y1="0" x2="1" y2="0">
            <stop offset="0%" stopColor="#111" />
            <stop offset="100%" stopColor="#eee" />
          </linearGradient>
        </defs>
        <rect
          x={PAD}
          y={H - 6}
          width={W - 2 * PAD}
          height={4}
          rx={2}
          fill={meta.hueAxis ? 'url(#hue-strip)' : 'url(#lum-strip)'}
        />
        <line x1={PAD} y1={H / 2} x2={W - PAD} y2={H / 2} stroke="rgba(255,255,255,0.25)" strokeDasharray="3 3" />
        <path d={curvePath(points, meta.xMax)} fill="none" stroke="var(--color-accent, #7dd3fc)" strokeWidth={1.6} />
        {points.map((p, i) => (
          <circle
            key={i}
            cx={toSvgX(p.x, meta.xMax)}
            cy={toSvgY(p.y)}
            r={4}
            fill={meta.hueAxis ? `hsl(${p.x}, 80%, 55%)` : '#ddd'}
            stroke="#fff"
            strokeWidth={1.2}
          />
        ))}
      </svg>
      <Text variant={TextVariants.small} className="mt-1 opacity-60">
        {t('adjustments.color.hueCurves.hint')}
      </Text>
    </div>
  );
}
