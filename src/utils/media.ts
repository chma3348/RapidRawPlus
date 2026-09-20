/** Containers the webview plays natively; mirrors VIDEO_EXTENSIONS in Rust. */
export const VIDEO_EXTENSIONS = ['mov', 'mp4', 'm4v'];

export function isVideoPath(path: string | null | undefined): boolean {
  if (!path) return false;
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  return VIDEO_EXTENSIONS.includes(ext);
}
