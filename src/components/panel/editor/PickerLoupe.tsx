import { useEffect, useRef, useState } from 'react';

/**
 * Magnified circle that follows the cursor while an eyedropper is active.
 *
 * Picking a white balance off a 30-pixel cloud edge is guesswork at fit-to-
 * screen zoom, so the loupe shows the pixels as they really are, with the
 * area that will actually be averaged drawn on top: the crosshair marks the
 * clicked pixel and the inner square is the sample footprint. The swatch
 * underneath is that average, which is the colour the picker will use.
 */
export interface LoupeProps {
  /** Rendered preview the canvas is showing. */
  previewUrl: string | null;
  /** Cursor position in container (CSS) pixels, for placing the loupe. */
  screen: { x: number; y: number } | null;
  /** Cursor position in the preview's logical coordinates. */
  image: { x: number; y: number } | null;
  /** Logical size of the preview on canvas, matching `image`'s units. */
  logicalSize: { width: number; height: number };
  /** Half-width of the averaged sample, in preview pixels. */
  sampleRadius?: number;
  /** Magnification. */
  zoom?: number;
  /** Diameter of the loupe in CSS pixels. */
  size?: number;
  label?: string;
}

const toHex = (c: number) => Math.max(0, Math.min(255, Math.round(c))).toString(16).padStart(2, '0');

export default function PickerLoupe({
  previewUrl,
  screen,
  image,
  logicalSize,
  sampleRadius = 5,
  zoom = 10,
  size = 132,
  label,
}: LoupeProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const [ready, setReady] = useState(false);
  const [sample, setSample] = useState<{ r: number; g: number; b: number } | null>(null);

  useEffect(() => {
    if (!previewUrl) {
      imageRef.current = null;
      setReady(false);
      return;
    }
    const img = new Image();
    img.crossOrigin = 'Anonymous';
    let cancelled = false;
    img.onload = () => {
      if (cancelled) return;
      imageRef.current = img;
      setReady(true);
    };
    img.src = previewUrl;
    return () => {
      cancelled = true;
    };
  }, [previewUrl]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const img = imageRef.current;
    if (!canvas || !img || !ready || !image || logicalSize.width <= 0 || logicalSize.height <= 0) return;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    if (!ctx) return;

    // Preview pixels per logical unit: the preview may be a different
    // resolution from the on-canvas size.
    const scaleX = img.width / logicalSize.width;
    const scaleY = img.height / logicalSize.height;
    const srcX = image.x * scaleX;
    const srcY = image.y * scaleY;
    const span = size / zoom;

    ctx.imageSmoothingEnabled = false;
    ctx.clearRect(0, 0, size, size);
    ctx.drawImage(img, srcX - span / 2, srcY - span / 2, span, span, 0, 0, size, size);

    // The footprint the picker will average, in loupe pixels.
    const footprint = Math.max(2, (sampleRadius * 2 + 1) * zoom * Math.max(scaleX, 1) * 0.5);
    ctx.strokeStyle = 'rgba(255,255,255,0.9)';
    ctx.lineWidth = 1;
    ctx.strokeRect(
      Math.round(size / 2 - footprint / 2) + 0.5,
      Math.round(size / 2 - footprint / 2) + 0.5,
      Math.round(footprint),
      Math.round(footprint),
    );
    ctx.strokeStyle = 'rgba(0,0,0,0.65)';
    ctx.beginPath();
    ctx.moveTo(size / 2, 0);
    ctx.lineTo(size / 2, size);
    ctx.moveTo(0, size / 2);
    ctx.lineTo(size, size / 2);
    ctx.stroke();

    // Average the real preview pixels, so the swatch is what gets picked.
    const rx = Math.round(srcX);
    const ry = Math.round(srcY);
    const r = Math.max(1, Math.round(sampleRadius * Math.max(scaleX, 1)));
    const x0 = Math.max(0, rx - r);
    const y0 = Math.max(0, ry - r);
    const x1 = Math.min(img.width, rx + r + 1);
    const y1 = Math.min(img.height, ry + r + 1);
    if (x1 <= x0 || y1 <= y0) {
      setSample(null);
      return;
    }
    const probe = document.createElement('canvas');
    probe.width = x1 - x0;
    probe.height = y1 - y0;
    const pctx = probe.getContext('2d', { willReadFrequently: true });
    if (!pctx) return;
    pctx.drawImage(img, x0, y0, x1 - x0, y1 - y0, 0, 0, x1 - x0, y1 - y0);
    const data = pctx.getImageData(0, 0, x1 - x0, y1 - y0).data;
    let rt = 0;
    let gt = 0;
    let bt = 0;
    let n = 0;
    for (let i = 0; i < data.length; i += 4) {
      rt += data[i];
      gt += data[i + 1];
      bt += data[i + 2];
      n += 1;
    }
    setSample(n ? { r: rt / n, g: gt / n, b: bt / n } : null);
  }, [ready, image, logicalSize.width, logicalSize.height, sampleRadius, zoom, size]);

  if (!previewUrl || !screen || !image) return null;

  // Keep the loupe beside the cursor, flipping when it would run off the
  // top-left of the canvas.
  const offset = 22;
  const flipX = screen.x < size + offset;
  const flipY = screen.y < size + offset;
  const left = flipX ? screen.x + offset : screen.x - size - offset;
  const top = flipY ? screen.y + offset : screen.y - size - offset;
  const hex = sample ? `#${toHex(sample.r)}${toHex(sample.g)}${toHex(sample.b)}` : '—';

  return (
    <div
      className="absolute pointer-events-none z-50 select-none"
      style={{ left, top, width: size }}
      aria-hidden="true"
    >
      <canvas
        ref={canvasRef}
        width={size}
        height={size}
        className="rounded-full border-2 border-white/80 shadow-lg"
        style={{ background: '#111', display: 'block' }}
      />
      <div className="mt-1 flex items-center gap-2 rounded-md bg-black/70 px-2 py-1 text-[11px] text-white">
        <span
          className="inline-block h-3 w-3 rounded-sm border border-white/50"
          style={{ background: sample ? `rgb(${sample.r} ${sample.g} ${sample.b})` : 'transparent' }}
        />
        <span className="font-mono">{hex}</span>
        {label && <span className="opacity-70">{label}</span>}
      </div>
    </div>
  );
}
