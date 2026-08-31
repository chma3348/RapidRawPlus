import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Images, Loader2, SquaresUnite, X } from 'lucide-react';
import Text from '../ui/Text';
import { TextColors, TextVariants } from '../../types/typography';
import { useProcessStore } from '../../store/useProcessStore';

export interface MergeCandidate {
  kind: 'hdr' | 'panorama';
  paths: Array<string>;
  frameCount: number;
  confidence: string;
  evidence: string;
  timeSpanSecs: number;
}

interface MergeSuggestionsModalProps {
  candidates: Array<MergeCandidate>;
  error: string | null;
  isOpen: boolean;
  isScanning: boolean;
  onClose(): void;
  onRequestThumbnails?(paths: Array<string>): void;
  onUse(candidate: MergeCandidate, paths: Array<string>): void;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

export default function MergeSuggestionsModal({
  candidates,
  error,
  isOpen,
  isScanning,
  onClose,
  onRequestThumbnails,
  onUse,
}: MergeSuggestionsModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);
  const thumbnails = useProcessStore((state) => state.thumbnails);
  // Frames the user has excluded, and whole cards they dismissed.
  const [excluded, setExcluded] = useState<Record<string, boolean>>({});
  const [dismissed, setDismissed] = useState<Record<number, boolean>>({});

  // Highest-confidence groups first so the obvious brackets are on top.
  const ordered = useMemo(() => {
    const rank = (c: MergeCandidate) => (c.confidence === 'high' ? 0 : 1);
    return candidates
      .map((candidate, index) => ({ candidate, index }))
      .filter(({ index }) => !dismissed[index])
      .sort((a, b) => rank(a.candidate) - rank(b.candidate));
  }, [candidates, dismissed]);

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      setExcluded({});
      setDismissed({});
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    }
    setShow(false);
    const timer = setTimeout(() => setIsMounted(false), 300);
    return () => clearTimeout(timer);
  }, [isOpen]);

  // Ask for any frame thumbnails that are not cached yet.
  useEffect(() => {
    if (!isOpen || candidates.length === 0 || !onRequestThumbnails) return;
    const missing = Array.from(new Set(candidates.flatMap((c) => c.paths))).filter(
      (path) => !thumbnails[path],
    );
    if (missing.length > 0) onRequestThumbnails(missing);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, candidates]);

  if (!isMounted) return null;

  const hdrCount = ordered.filter(({ candidate }) => candidate.kind === 'hdr').length;
  const panoCount = ordered.filter(({ candidate }) => candidate.kind === 'panorama').length;

  return (
    <div
      className={`fixed inset-0 flex items-center justify-center z-50 bg-black/30 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
        show ? 'opacity-100' : 'opacity-0'
      }`}
      onClick={onClose}
      role="dialog"
    >
      <div
        className={`bg-surface rounded-lg shadow-xl p-6 w-full max-w-2xl max-h-[80vh] flex flex-col transform transition-all duration-300 ease-out ${
          show ? 'scale-100 opacity-100 translate-y-0' : 'scale-95 opacity-0 -translate-y-4'
        }`}
        onClick={(e: any) => e.stopPropagation()}
      >
        <Text variant={TextVariants.title} className="mb-1">
          {t('modals.mergeSuggestions.title')}
        </Text>
        <Text variant={TextVariants.small} className="mb-4 block text-text-secondary">
          {isScanning
            ? t('modals.mergeSuggestions.scanning')
            : t('modals.mergeSuggestions.summary', { hdr: hdrCount, pano: panoCount })}
        </Text>

        {error && (
          <Text variant={TextVariants.small} color={TextColors.error} className="mb-3">
            {error}
          </Text>
        )}

        <div className="flex-1 overflow-y-auto space-y-2 pr-1">
          {isScanning && (
            <div className="flex items-center gap-3 p-4">
              <Loader2 size={16} className="animate-spin text-accent" />
              <Text variant={TextVariants.small}>{t('modals.mergeSuggestions.scanning')}</Text>
            </div>
          )}

          {!isScanning && ordered.length === 0 && !error && (
            <div className="p-4">
              <Text variant={TextVariants.small} className="text-text-secondary">
                {t('modals.mergeSuggestions.empty')}
              </Text>
            </div>
          )}

          {ordered.map(({ candidate, index }) => {
            const isHdr = candidate.kind === 'hdr';
            const Icon = isHdr ? Images : SquaresUnite;
            const keep = candidate.paths.filter((p) => !excluded[`${index}:${p}`]);
            const canMerge = keep.length >= 2;
            return (
              <div key={`${candidate.kind}-${index}-${candidate.paths[0]}`} className="p-3 bg-bg-tertiary rounded-md">
                <div className="flex items-center gap-3 mb-2">
                  <Icon size={18} className="text-accent shrink-0" />
                  <div className="min-w-0 flex-1">
                    <Text variant={TextVariants.body} className="truncate">
                      {isHdr
                        ? t('modals.mergeSuggestions.bracketSet', { count: keep.length })
                        : t('modals.mergeSuggestions.sweep', { count: keep.length })}
                      {candidate.confidence === 'high' && ` · ${t('modals.mergeSuggestions.highConfidence')}`}
                    </Text>
                    <Text variant={TextVariants.small} className="block text-text-secondary truncate">
                      {candidate.evidence}
                    </Text>
                  </div>
                  <button
                    className="p-1.5 text-text-secondary hover:text-red-400 transition-colors shrink-0"
                    onClick={() => setDismissed((prev) => ({ ...prev, [index]: true }))}
                    title={t('modals.mergeSuggestions.dismiss')}
                  >
                    <X size={16} />
                  </button>
                </div>

                <div className="flex gap-2 overflow-x-auto pb-1">
                  {candidate.paths.map((path) => {
                    const key = `${index}:${path}`;
                    const isExcluded = !!excluded[key];
                    const thumb = thumbnails[path];
                    return (
                      <button
                        key={key}
                        className={`relative shrink-0 rounded overflow-hidden border-2 transition-all ${
                          isExcluded ? 'border-transparent opacity-30' : 'border-accent'
                        }`}
                        onClick={() => setExcluded((prev) => ({ ...prev, [key]: !prev[key] }))}
                        title={fileName(path)}
                      >
                        {thumb ? (
                          <img src={thumb} alt={fileName(path)} className="h-16 w-auto block" />
                        ) : (
                          <div className="h-16 w-20 bg-bg-primary flex items-center justify-center">
                            <Loader2 size={14} className="animate-spin text-text-secondary" />
                          </div>
                        )}
                      </button>
                    );
                  })}
                </div>

                <div className="flex items-center justify-between mt-2">
                  <Text variant={TextVariants.small} className="text-text-secondary truncate">
                    {t('modals.mergeSuggestions.frameHint')}
                  </Text>
                  <button
                    className="px-3 py-2 bg-accent text-button-text rounded-md hover:bg-accent-hover transition-colors text-sm shrink-0 disabled:opacity-40"
                    disabled={!canMerge}
                    onClick={() => onUse(candidate, keep)}
                  >
                    {isHdr
                      ? t('modals.mergeSuggestions.useHdrCount', { count: keep.length })
                      : t('modals.mergeSuggestions.usePanoCount', { count: keep.length })}
                  </button>
                </div>
              </div>
            );
          })}
        </div>

        <div className="flex justify-end pt-4">
          <button
            className="px-4 py-2 bg-bg-primary text-text-secondary rounded-md hover:bg-card-active transition-colors text-sm"
            onClick={onClose}
          >
            {t('modals.mergeSuggestions.close')}
          </button>
        </div>
      </div>
    </div>
  );
}
