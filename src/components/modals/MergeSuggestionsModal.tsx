import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Images, Loader2, SquaresUnite } from 'lucide-react';
import Text from '../ui/Text';
import { TextColors, TextVariants } from '../../types/typography';

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
  onUse(candidate: MergeCandidate): void;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

export default function MergeSuggestionsModal({
  candidates,
  error,
  isOpen,
  isScanning,
  onClose,
  onUse,
}: MergeSuggestionsModalProps) {
  const { t } = useTranslation();
  const [isMounted, setIsMounted] = useState(false);
  const [show, setShow] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setIsMounted(true);
      const timer = setTimeout(() => setShow(true), 10);
      return () => clearTimeout(timer);
    }
    setShow(false);
    const timer = setTimeout(() => setIsMounted(false), 300);
    return () => clearTimeout(timer);
  }, [isOpen]);

  if (!isMounted) return null;

  const hdrCount = candidates.filter((c) => c.kind === 'hdr').length;
  const panoCount = candidates.filter((c) => c.kind === 'panorama').length;

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

          {!isScanning && candidates.length === 0 && !error && (
            <div className="p-4">
              <Text variant={TextVariants.small} className="text-text-secondary">
                {t('modals.mergeSuggestions.empty')}
              </Text>
            </div>
          )}

          {candidates.map((candidate, index) => {
            const isHdr = candidate.kind === 'hdr';
            const Icon = isHdr ? Images : SquaresUnite;
            return (
              <div
                key={`${candidate.kind}-${index}-${candidate.paths[0]}`}
                className="flex items-center gap-3 p-3 bg-bg-tertiary rounded-md"
              >
                <Icon size={18} className="text-accent shrink-0" />
                <div className="min-w-0 flex-1">
                  <Text variant={TextVariants.body} className="truncate">
                    {isHdr
                      ? t('modals.mergeSuggestions.bracketSet', { count: candidate.frameCount })
                      : t('modals.mergeSuggestions.sweep', { count: candidate.frameCount })}
                    {candidate.confidence === 'high' && ` · ${t('modals.mergeSuggestions.highConfidence')}`}
                  </Text>
                  <Text variant={TextVariants.small} className="block text-text-secondary truncate">
                    {candidate.evidence}
                  </Text>
                  <Text variant={TextVariants.small} className="block text-text-secondary truncate">
                    {fileName(candidate.paths[0])} … {fileName(candidate.paths[candidate.paths.length - 1])}
                  </Text>
                </div>
                <button
                  className="px-3 py-2 bg-accent text-button-text rounded-md hover:bg-accent-hover transition-colors text-sm shrink-0"
                  onClick={() => onUse(candidate)}
                >
                  {isHdr ? t('modals.mergeSuggestions.useHdr') : t('modals.mergeSuggestions.usePano')}
                </button>
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
