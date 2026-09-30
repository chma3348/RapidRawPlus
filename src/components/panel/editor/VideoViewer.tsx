import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { Camera } from 'lucide-react';
import { toast } from 'react-toastify';
import { Invokes } from '../../ui/AppProperties';

/**
 * Plays a video from the library.
 *
 * Decoding is the webview's, not ours: WebKit already has the H.264 and
 * HEVC decoders these files use, so a `<video>` element pointed at the
 * file gives hardware-accelerated playback, scrubbing and audio for free.
 * Bundling a decoder to do the same would be a large dependency for no
 * gain.
 *
 * Adjustments do not apply here. What bridges the gap is "Save frame":
 * the frame on screen is written out as a PNG beside the video, and that
 * still is an ordinary photo the editor can work on.
 */
export default function VideoViewer({ path }: { path: string }) {
  const { t } = useTranslation();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const [info, setInfo] = useState<{
    durationSeconds?: number | null;
    width?: number | null;
    height?: number | null;
    codecs?: string[];
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [src, setSrc] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke(Invokes.LoadVideoInfo, { path })
      .then((v: any) => {
        if (!cancelled) setInfo(v);
      })
      .catch(() => {
        if (!cancelled) setInfo(null);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  // Why the clip is streamed from a local address.
  //
  // WebKit plays media through AVFoundation, which opens the element's URL
  // itself in another process. It cannot read Tauri's `asset:` scheme, and
  // it cannot read a page's `blob:` URLs either: both give a sized black box
  // that never plays and never reports an error. The backend serves each
  // clip over HTTP on 127.0.0.1 (see video_server.rs), which AVFoundation
  // plays natively, with seeking and no need to hold the clip in memory.
  useEffect(() => {
    let cancelled = false;
    setSrc(null);
    setProblem(null);
    invoke('frontend_log', { level: 'info', message: `[video] opening ${path}` }).catch(() => {});
    invoke<string>(Invokes.VideoStreamUrl, { path })
      .then((url) => {
        if (!cancelled) setSrc(url);
      })
      .catch((err) => {
        if (cancelled) return;
        setProblem(String(err?.message ?? err));
        invoke('frontend_log', {
          level: 'error',
          message: `[video] could not open ${path}: ${err}`,
        }).catch(() => {});
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  // Playback lives in the webview, so when it fails there is nothing in
  // the Rust log to explain it. Put the media element's own verdict there.
  const report = (level: string, what: string) => {
    const v = videoRef.current;
    invoke('frontend_log', {
      level,
      message:
        `[video] ${what} network=${v?.networkState} ready=${v?.readyState} ` +
        `error=${v?.error ? `${v.error.code}:${v.error.message}` : 'none'} ` +
        `size=${v?.videoWidth}x${v?.videoHeight}`,
    }).catch(() => {});
  };

  const saveFrame = async () => {
    const video = videoRef.current;
    if (!video || !video.videoWidth) {
      report('warn', 'save frame with no frame to save');
      return;
    }
    setSaving(true);
    try {
      const canvas = document.createElement('canvas');
      canvas.width = video.videoWidth;
      canvas.height = video.videoHeight;
      const ctx = canvas.getContext('2d');
      if (!ctx) throw new Error('no 2d context');
      ctx.drawImage(video, 0, 0);
      const dataUrl = canvas.toDataURL('image/png');
      const saved: string = await invoke(Invokes.SaveVideoFrame, {
        videoPath: path,
        pngBase64: dataUrl.split(',')[1] ?? '',
        timeSeconds: video.currentTime,
      });
      toast.success(t('editor.video.frameSaved', 'Frame saved as {{name}}', {
        name: saved.split('/').pop() ?? saved,
      }));
    } catch (err) {
      report('error', `save frame failed: ${err}`);
      toast.error(`${t('editor.video.frameFailed', 'Could not save the frame')}: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const duration = info?.durationSeconds
    ? `${Math.floor(info.durationSeconds / 60)}:${String(Math.round(info.durationSeconds % 60)).padStart(2, '0')}`
    : null;
  const facts = [
    info?.width && info?.height ? `${info.width} × ${info.height}` : null,
    duration,
    info?.codecs?.find((c) => !c.toLowerCase().includes('aac') && !c.toLowerCase().includes('metadata')),
  ].filter(Boolean);

  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-3 p-4">
      {problem ? (
        <p className="max-w-md text-center text-sm text-text-secondary">
          {t('editor.video.loadFailed', 'This clip could not be opened')}: {problem}
        </p>
      ) : src ? (
        <video
          ref={videoRef}
          key={src}
          src={src}
          // The server allows any origin, so frames drawn to a canvas stay
          // readable for "Save frame".
          crossOrigin="anonymous"
          controls
          autoFocus
          onError={() => report('error', 'failed')}
          onStalled={() => report('warn', 'stalled')}
          onLoadedMetadata={() => report('info', 'metadata')}
          onCanPlay={() => report('info', 'canplay')}
          playsInline
          className="max-h-[calc(100%-3rem)] max-w-full rounded-lg shadow-lg bg-black"
        />
      ) : (
        <p className="text-sm text-text-secondary">
          {t('editor.video.loading', 'Loading the clip…')}
        </p>
      )}
      <div className="flex items-center gap-3 text-xs text-text-secondary">
        {facts.length > 0 && <span>{facts.join('  ·  ')}</span>}
        <button
          type="button"
          onClick={saveFrame}
          disabled={saving || !src}
          className="flex items-center gap-1.5 rounded-md border border-surface px-2 py-1 text-text-primary hover:bg-card-active disabled:opacity-50"
          data-tooltip={t('editor.video.saveFrameTooltip', 'Write the current frame beside the video as a PNG you can edit')}
        >
          <Camera size={14} />
          {t('editor.video.saveFrame', 'Save frame')}
        </button>
      </div>
    </div>
  );
}
