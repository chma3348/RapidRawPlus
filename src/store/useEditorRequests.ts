import { create } from 'zustand';

/**
 * Something a keyboard shortcut asks a panel to do, where the doing lives in
 * that panel (a new mask is made by the Masks panel, a healing patch by the
 * AI panel, an export by the Export panel). The shortcut opens the panel and
 * sends the request; the panel, once mounted, takes it and acts.
 */
export type EditorRequest =
  | { kind: 'mask'; mask: 'linear' | 'radial' | 'brush' }
  | { kind: 'heal' }
  | { kind: 'exportPrevious' };

interface EditorRequestState {
  request: (EditorRequest & { id: number }) | null;
  send: (request: EditorRequest) => void;
  /** Take the pending request of `kind`, if there is one, so it is acted on once. */
  take: <K extends EditorRequest['kind']>(kind: K) => Extract<EditorRequest, { kind: K }> | null;
}

export const useEditorRequests = create<EditorRequestState>((set, get) => ({
  request: null,
  send: (request) => set((s) => ({ request: { ...request, id: (s.request?.id ?? 0) + 1 } })),
  take: (kind) => {
    const request = get().request;
    if (!request || request.kind !== kind) return null;
    set({ request: null });
    return request as any;
  },
}));
