import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { useTranslation } from 'react-i18next';
import { CheckCircle, Download, FolderInput, Loader2, Trash2, X } from 'lucide-react';
import Button from '../ui/Button';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { Invokes } from '../ui/AppProperties';

/**
 * The generative engine's large models (generative fill, AI upscaling):
 * each downloaded from its publisher, under the licence shown with it, and
 * deletable in one click. "Import from folder" copies them from another
 * Mac's models folder or a drive instead of downloading them again.
 */

interface EngineModel {
  id: string;
  name: string;
  purpose: string;
  neededFor: string[];
  sizeBytes: number;
  installed: boolean;
  downloading: boolean;
  partialBytes: number;
  licence: string;
  licenceUrl: string;
  mustAccept: boolean;
  sourceUrl: string;
}

interface OtherModelFile {
  path: string;
  sizeBytes: number;
}

interface Listing {
  models: EngineModel[];
  others: OtherModelFile[];
  totalBytes: number;
  freeBytes: number | null;
}

interface Progress {
  id: string;
  downloaded: number;
  total: number;
  stage: 'downloading' | 'verifying' | 'done' | 'failed' | 'cancelled';
  message: string | null;
}

const size = (bytes: number) =>
  bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;

export default function EngineModels() {
  const { t } = useTranslation();
  const [listing, setListing] = useState<Listing | null>(null);
  const [progress, setProgress] = useState<Record<string, Progress>>({});
  const [accepted, setAccepted] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);

  const load = useCallback(() => {
    invoke<Listing>(Invokes.ListEngineModels)
      .then(setListing)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    load();
    const unlisten = listen<Progress>('engine-model-progress', (e) => {
      setProgress((p) => ({ ...p, [e.payload.id]: e.payload }));
      if (['done', 'failed', 'cancelled'].includes(e.payload.stage)) load();
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [load]);

  const download = async (m: EngineModel) => {
    setError(null);
    setListing((l) => l && { ...l, models: l.models.map((x) => (x.id === m.id ? { ...x, downloading: true } : x)) });
    try {
      setListing(await invoke(Invokes.DownloadEngineModel, { id: m.id, acceptedLicence: !!accepted[m.id] }));
    } catch (e) {
      setError(String(e));
      load();
    }
  };

  const remove = async (m: EngineModel) => {
    setError(null);
    try {
      setListing(await invoke(Invokes.DeleteEngineModel, { id: m.id }));
      setProgress((p) => {
        const { [m.id]: _gone, ...rest } = p;
        return rest;
      });
    } catch (e) {
      setError(String(e));
    }
  };

  const removeOther = async (f: OtherModelFile) => {
    setError(null);
    try {
      setListing(await invoke(Invokes.DeleteOtherEngineFile, { path: f.path }));
    } catch (e) {
      setError(String(e));
    }
  };

  const importFolder = async () => {
    const folder = await openDialog({ directory: true, multiple: false });
    if (!folder || Array.isArray(folder)) return;
    setImporting(true);
    setError(null);
    setNotice(null);
    try {
      const [result, imported] = await invoke<[Listing, string[]]>(Invokes.ImportEngineModels, { folder });
      setListing(result);
      setNotice(
        imported.length
          ? t('settings.engineModels.imported', {
              defaultValue: 'Imported: {{names}}.',
              names: imported.join(', '),
            })
          : t('settings.engineModels.nothingImported', {
              defaultValue: 'No missing models were found in that folder.',
            }),
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setImporting(false);
    }
  };

  if (!listing) return null;

  return (
    <div className="p-4 mb-6 bg-bg-primary rounded-lg border border-border-color">
      <div className="flex items-start justify-between gap-4 mb-1">
        <div>
          <Text variant={TextVariants.body} weight={TextWeights.medium}>
            {t('settings.engineModels.title', { defaultValue: 'Generative fill and upscaling models' })}
          </Text>
          <Text variant={TextVariants.small} className="opacity-70 block">
            {t('settings.engineModels.desc', {
              defaultValue:
                'Large models the AI engine uses, each downloaded from its publisher under the licence shown. Using {{used}}{{free}}.',
              used: size(listing.totalBytes),
              free:
                listing.freeBytes != null
                  ? t('settings.engineModels.free', { defaultValue: '; {{free}} free', free: size(listing.freeBytes) })
                  : '',
            })}
          </Text>
        </div>
        <Button onClick={importFolder} disabled={importing} className="px-3 shrink-0">
          {importing ? <Loader2 className="animate-spin mr-2" size={16} /> : <FolderInput size={16} className="mr-2" />}
          {t('settings.engineModels.import', { defaultValue: 'Import from folder' })}
        </Button>
      </div>

      {notice && (
        <Text variant={TextVariants.small} color={TextColors.success} className="block mt-2">
          {notice}
        </Text>
      )}
      {error && (
        <Text variant={TextVariants.small} color={TextColors.error} className="block mt-2">
          {error}
        </Text>
      )}

      <div className="space-y-2 mt-3">
        {listing.models.map((m) => {
          const p = progress[m.id];
          const busy = m.downloading || p?.stage === 'downloading' || p?.stage === 'verifying';
          const fraction = p && p.total ? Math.min(1, p.downloaded / p.total) : 0;
          return (
            <div key={m.id} className="p-3 rounded-lg border border-border-color bg-surface">
              <div className="flex items-center gap-3">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <Text variant={TextVariants.body} weight={TextWeights.medium}>
                      {m.name}
                    </Text>
                    {m.installed && <CheckCircle size={14} className="text-green-500 shrink-0" />}
                  </div>
                  <Text variant={TextVariants.small} className="block">
                    {m.purpose} · {t('settings.engineModels.neededFor', { defaultValue: 'needed for' })}{' '}
                    {m.neededFor.join(', ')}
                  </Text>
                  <Text variant={TextVariants.small} className="block opacity-70">
                    {t('settings.engineModels.licence', { defaultValue: 'Licence' })}:{' '}
                    <button className="underline" onClick={() => openUrl(m.licenceUrl)}>
                      {m.licence}
                    </button>
                    {' · '}
                    <button className="underline" onClick={() => openUrl(m.sourceUrl)}>
                      {t('settings.engineModels.source', { defaultValue: 'source' })}
                    </button>
                  </Text>
                </div>
                <Text variant={TextVariants.small} className="shrink-0">
                  {size(m.sizeBytes)}
                </Text>
                <div className="shrink-0 flex items-center gap-1">
                  {busy ? (
                    <>
                      <Text variant={TextVariants.small} className="w-28 text-right">
                        {p?.stage === 'verifying'
                          ? t('settings.engineModels.verifying', { defaultValue: 'Checking…' })
                          : `${Math.round(fraction * 100)}%`}
                      </Text>
                      <button
                        onClick={() => invoke(Invokes.CancelEngineModelDownload, { id: m.id })}
                        className="p-2 rounded-md text-text-secondary hover:bg-card-active"
                        title={t('settings.engineModels.cancel', { defaultValue: 'Cancel' })}
                      >
                        <X size={16} />
                      </button>
                    </>
                  ) : m.installed ? (
                    <button
                      onClick={() => remove(m)}
                      className="p-2 rounded-md text-text-secondary hover:text-red-400 hover:bg-card-active transition-colors"
                      title={t('settings.engineModels.delete', { defaultValue: 'Delete' })}
                    >
                      <Trash2 size={16} />
                    </button>
                  ) : (
                    <Button onClick={() => download(m)} disabled={m.mustAccept && !accepted[m.id]} className="px-3">
                      <Download size={16} className="mr-2" />
                      {m.partialBytes > 0
                        ? t('settings.engineModels.resume', { defaultValue: 'Resume' })
                        : t('settings.engineModels.download', { defaultValue: 'Download' })}
                    </Button>
                  )}
                </div>
              </div>
              {busy && (
                <div className="mt-2 h-1.5 rounded bg-bg-primary overflow-hidden">
                  <div className="h-full bg-accent transition-all" style={{ width: `${fraction * 100}%` }} />
                </div>
              )}
              {m.mustAccept && !m.installed && !busy && (
                <label className="flex items-center gap-2 mt-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={!!accepted[m.id]}
                    onChange={(e) => setAccepted((a) => ({ ...a, [m.id]: e.target.checked }))}
                  />
                  <Text variant={TextVariants.small}>
                    {t('settings.engineModels.accept', {
                      defaultValue: 'I accept the {{licence}} (free, non-commercial use).',
                      licence: m.licence,
                    })}
                  </Text>
                </label>
              )}
              {p?.stage === 'failed' && p.message && (
                <Text variant={TextVariants.small} color={TextColors.error} className="block mt-1">
                  {p.message}
                </Text>
              )}
            </div>
          );
        })}
      </div>

      {listing.others.length > 0 && (
        <div className="mt-4">
          <Text
            variant={TextVariants.small}
            weight={TextWeights.semibold}
            className="uppercase tracking-wide mb-2 block"
          >
            {t('settings.engineModels.others', { defaultValue: 'Other files in the models folder' })}
          </Text>
          <div className="space-y-1">
            {listing.others.map((f) => (
              <div key={f.path} className="flex items-center gap-3 px-3 py-1.5 rounded-md bg-surface">
                <Text variant={TextVariants.small} className="flex-1 truncate font-mono">
                  {f.path}
                </Text>
                <Text variant={TextVariants.small} className="shrink-0">
                  {size(f.sizeBytes)}
                </Text>
                <button
                  onClick={() => removeOther(f)}
                  className="p-1.5 rounded-md text-text-secondary hover:text-red-400 hover:bg-card-active transition-colors"
                  title={t('settings.engineModels.delete', { defaultValue: 'Delete' })}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
