import { useRef, useState } from 'react';
import type { PointerEvent } from 'react';
import type { SkyBrush } from '../../../store/useEditorStore';

interface Point {
  x: number;
  y: number;
}

interface Props {
  brush: SkyBrush;
  /** Where the (cropped) picture sits, in the canvas's own unzoomed pixels. */
  left: number;
  top: number;
  width: number;
  height: number;
  /** The brush's diameter in those pixels. */
  diameter: number;
  /** A point in those pixels to a fraction of the photo as shown. */
  toPhoto(p: Point): Point;
}

/**
 * Sky Replace's Add / Subtract brush on the photograph: the round cursor, the
 * stroke being painted, and the finished stroke handed back on release.
 * Option (Alt) held while painting switches between adding and subtracting,
 * as in the Masks brush. Its own component, so a stroke only repaints this.
 */
export default function SkyBrushLayer({ brush, left, top, width, height, diameter, toPhoto }: Props) {
  const stroke = useRef<{ erase: boolean; points: Point[] } | null>(null);
  const [cursor, setCursor] = useState<(Point & { erase: boolean }) | null>(null);
  const [painting, setPainting] = useState<{ erase: boolean; points: Point[] } | null>(null);

  // On screen, as zoomed, to the layer's own pixels.
  const local = (e: PointerEvent<HTMLDivElement>): Point => {
    const rect = e.currentTarget.getBoundingClientRect();
    const perPixel = rect.width / Math.max(1, width);
    return { x: (e.clientX - rect.left) / perPixel, y: (e.clientY - rect.top) / perPixel };
  };
  const finish = () => {
    const s = stroke.current;
    stroke.current = null;
    setPainting(null);
    if (s && s.points.length) brush.onStroke(s.points.map(toPhoto), s.erase);
  };

  // Red adds sky, as the sky overlay shows it; blue takes it away.
  const tint = (erase: boolean, alpha: number) =>
    erase ? `rgba(80, 160, 255, ${alpha})` : `rgba(255, 70, 70, ${alpha})`;
  const erase = cursor ? cursor.erase : brush.erase;

  return (
    <div
      style={{
        position: 'absolute',
        left: `${left}px`,
        top: `${top}px`,
        width: `${width}px`,
        height: `${height}px`,
        cursor: 'none',
        touchAction: 'none',
        zIndex: 4,
      }}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.stopPropagation();
        e.currentTarget.setPointerCapture(e.pointerId);
        const p = local(e);
        const s = { erase: brush.erase !== e.altKey, points: [p] };
        stroke.current = s;
        setPainting({ ...s });
        setCursor({ ...p, erase: s.erase });
      }}
      onPointerMove={(e) => {
        const p = local(e);
        const s = stroke.current;
        if (s) {
          s.points.push(p);
          setPainting({ erase: s.erase, points: s.points });
        }
        setCursor({ ...p, erase: s ? s.erase : brush.erase !== e.altKey });
      }}
      onPointerUp={(e) => {
        if (!stroke.current) return;
        e.currentTarget.releasePointerCapture(e.pointerId);
        finish();
      }}
      onPointerCancel={finish}
      onPointerLeave={() => {
        if (!stroke.current) setCursor(null);
      }}
    >
      <svg
        width={width}
        height={height}
        style={{ position: 'absolute', left: 0, top: 0, overflow: 'visible', pointerEvents: 'none' }}
      >
        {painting && (
          <polyline
            points={painting.points.map((p) => `${p.x},${p.y}`).join(' ')}
            fill="none"
            stroke={tint(painting.erase, 0.45)}
            strokeWidth={diameter}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        )}
        {cursor && (
          <g>
            <circle
              cx={cursor.x}
              cy={cursor.y}
              r={diameter / 2}
              fill="none"
              stroke="rgba(0, 0, 0, 0.6)"
              strokeWidth={3}
              vectorEffect="non-scaling-stroke"
            />
            <circle
              cx={cursor.x}
              cy={cursor.y}
              r={diameter / 2}
              fill="none"
              stroke={tint(erase, 1)}
              strokeWidth={1.5}
              vectorEffect="non-scaling-stroke"
            />
            {/* Where the feather starts: full strength inside, fading out to the outer ring. */}
            {brush.feather > 0.02 && (
              <circle
                cx={cursor.x}
                cy={cursor.y}
                r={(diameter / 2) * (1 - brush.feather)}
                fill="none"
                stroke={tint(erase, 0.9)}
                strokeWidth={1}
                strokeDasharray="3 3"
                vectorEffect="non-scaling-stroke"
              />
            )}
            <line
              x1={cursor.x - 5}
              x2={cursor.x + 5}
              y1={cursor.y}
              y2={cursor.y}
              stroke="white"
              strokeWidth={1.5}
              vectorEffect="non-scaling-stroke"
            />
            {!erase && (
              <line
                x1={cursor.x}
                x2={cursor.x}
                y1={cursor.y - 5}
                y2={cursor.y + 5}
                stroke="white"
                strokeWidth={1.5}
                vectorEffect="non-scaling-stroke"
              />
            )}
          </g>
        )}
      </svg>
    </div>
  );
}
