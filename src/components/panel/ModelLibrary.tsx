import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import { Download, Trash2, Loader2, RefreshCw, Plus, CheckCircle, FolderOpen, X, Search } from 'lucide-react';
import Button from '../ui/Button';
import Dropdown from '../ui/Dropdown';
import Input from '../ui/Input';
import Text from '../ui/Text';
import { TextColors, TextVariants, TextWeights } from '../../types/typography';
import { Invokes } from '../ui/AppProperties';

interface LibraryModelInfo {
  id: string;
  displayName: string;
  taskType: string;
  description: string;
  sizeBytes: number;
  downloaded: boolean;
  downloadable: boolean;
  builtin: boolean;
}

interface RemoteModelFile {
  filename: string;
  sizeBytes: number;
  dataFilename: string | null;
  needsConversion: boolean;
}

interface RemoteModelRepo {
  repoId: string;
  downloads: number;
  likes: number;
  files: Array<RemoteModelFile>;
}

const TASK_ORDER = ['upscale', 'deblur', 'restore', 'inpaint', 'mask'];

const formatSize = (bytes: number): string => {
  if (!bytes) return '';
  const mb = bytes / (1024 * 1024);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
};

const ModelLibrary = () => {
  const { t } = useTranslation();
  const [models, setModels] = useState<Array<LibraryModelInfo>>([]);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);
  const [addUrl, setAddUrl] = useState('');
  const [addFile, setAddFile] = useState<string | null>(null);
  const [addName, setAddName] = useState('');
  const [addTask, setAddTask] = useState('upscale');
  const [isAdding, setIsAdding] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<Array<RemoteModelRepo> | null>(null);
  const [isSearching, setIsSearching] = useState(false);
  const [installTasks, setInstallTasks] = useState<Record<string, string>>({});
  const [installingKey, setInstallingKey] = useState<string | null>(null);
  const [convertStatus, setConvertStatus] = useState<string | null>(null);
  const [engineInstalled, setEngineInstalled] = useState<boolean | null>(null);
  const [engineInstalling, setEngineInstalling] = useState(false);
  const [engineProgress, setEngineProgress] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = listen('convert-progress', (e: any) => {
      setConvertStatus(String(e.payload));
    });
    const unlistenEngine = listen('engine-install-progress', (e: any) => {
      setEngineProgress(String(e.payload));
    });
    invoke<{ installed: boolean }>(Invokes.GetEngineStatus)
      .then((s) => setEngineInstalled(s.installed))
      .catch(() => setEngineInstalled(null));
    return () => {
      unlisten.then((f) => f());
      unlistenEngine.then((f) => f());
    };
  }, []);

  const handleInstallEngine = async () => {
    setEngineInstalling(true);
    setError(null);
    setEngineProgress(null);
    try {
      await invoke(Invokes.InstallAiEngine);
      setEngineInstalled(true);
    } catch (err) {
      setError(String(err));
    } finally {
      setEngineInstalling(false);
      setEngineProgress(null);
    }
  };

  const load = useCallback(async (refresh: boolean) => {
    setError(null);
    if (refresh) setIsRefreshing(true);
    try {
      const result: Array<LibraryModelInfo> = await invoke(Invokes.GetModelLibrary, { refresh });
      setModels(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsRefreshing(false);
    }
  }, []);

  useEffect(() => {
    load(false);
  }, [load]);

  const handleDownload = async (id: string) => {
    setBusyId(id);
    setError(null);
    try {
      const result: Array<LibraryModelInfo> = await invoke(Invokes.DownloadLibraryModel, { modelId: id });
      setModels(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusyId(null);
    }
  };

  const handleDelete = async (id: string) => {
    setBusyId(id);
    setError(null);
    try {
      const result: Array<LibraryModelInfo> = await invoke(Invokes.DeleteLibraryModel, { modelId: id });
      setModels(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusyId(null);
    }
  };

  const handleBrowse = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: 'AI model', extensions: ['onnx', 'pth', 'safetensors', 'ckpt'] }],
    });
    if (typeof selected === 'string') {
      setAddFile(selected);
      if (!addName) {
        const stem = selected.split('/').pop()?.replace(/\.(onnx|pth|safetensors|ckpt)$/i, '') ?? '';
        setAddName(stem.replace(/[_-]+/g, ' ').trim());
      }
    }
  };

  const handleAdd = async () => {
    if ((!addUrl && !addFile) || !addName) return;
    setIsAdding(true);
    setError(null);
    try {
      const args = { displayName: addName, taskType: addTask };
      const result: Array<LibraryModelInfo> = addFile
        ? await invoke(Invokes.AddModelFromFile, { ...args, filePath: addFile })
        : await invoke(Invokes.AddModelFromUrl, { ...args, url: addUrl });
      setModels(result);
      setAddUrl('');
      setAddFile(null);
      setAddName('');
      setShowAddForm(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsAdding(false);
      setConvertStatus(null);
    }
  };

  const handleSearch = async () => {
    if (!searchQuery.trim()) return;
    setIsSearching(true);
    setError(null);
    try {
      const results: Array<RemoteModelRepo> = await invoke(Invokes.SearchRemoteModels, {
        query: searchQuery,
      });
      setSearchResults(results);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsSearching(false);
    }
  };

  const handleInstall = async (repo: RemoteModelRepo, file: RemoteModelFile) => {
    const key = `${repo.repoId}/${file.filename}`;
    const stem = file.filename.split('/').pop()?.replace(/\.onnx$/i, '') ?? file.filename;
    setInstallingKey(key);
    setError(null);
    try {
      const result: Array<LibraryModelInfo> = await invoke(Invokes.InstallRemoteModel, {
        repoId: repo.repoId,
        filename: file.filename,
        dataFilename: file.dataFilename,
        displayName: stem.replace(/[_-]+/g, ' ').trim(),
        taskType: installTasks[key] || 'upscale',
      });
      setModels(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setInstallingKey(null);
      setConvertStatus(null);
    }
  };

  const grouped = useMemo(() => {
    const groups: Array<{ task: string; items: Array<LibraryModelInfo> }> = [];
    for (const task of TASK_ORDER) {
      const items = models.filter((m) => m.taskType === task);
      if (items.length > 0) groups.push({ task, items });
    }
    return groups;
  }, [models]);

  const taskLabel = (task: string) =>
    task === 'mask'
      ? t('settings.modelLibrary.taskMask')
      : t(`settings.processing.aiModels.${task}`, { defaultValue: task });

  const taskOptions = [
    { label: t('settings.processing.aiModels.upscale'), value: 'upscale' },
    { label: t('settings.processing.aiModels.deblur'), value: 'deblur' },
    { label: t('settings.processing.aiModels.restore'), value: 'restore' },
    { label: t('settings.processing.aiModels.inpaint'), value: 'inpaint' },
  ];

  return (
    <div>
      <div className="p-4 mb-4 bg-bg-primary rounded-lg border border-border-color flex items-center gap-4">
        <div className="flex-1 min-w-0">
          <Text variant={TextVariants.body} weight={TextWeights.medium}>
            {t('settings.modelLibrary.engineTitle')}
          </Text>
          <Text variant={TextVariants.small} className="opacity-70">
            {engineInstalling && engineProgress ? engineProgress : t('settings.modelLibrary.engineDesc')}
          </Text>
        </div>
        {engineInstalled ? (
          <Text variant={TextVariants.small} color={TextColors.success} className="shrink-0">
            {t('settings.modelLibrary.engineInstalled')}
          </Text>
        ) : (
          <Button onClick={handleInstallEngine} disabled={engineInstalling || engineInstalled === null} className="shrink-0">
            {engineInstalling ? <Loader2 className="animate-spin mr-2" size={16} /> : null}
            {engineInstalling
              ? t('settings.modelLibrary.engineInstalling')
              : t('settings.modelLibrary.engineInstall')}
          </Button>
        )}
      </div>

      <div className="flex items-center justify-between mb-4">
        <Text variant={TextVariants.heading}>{t('settings.modelLibrary.installedTitle')}</Text>
        <div className="flex gap-2">
          <Button onClick={() => load(true)} disabled={isRefreshing} className="px-3">
            {isRefreshing ? <Loader2 className="animate-spin" size={16} /> : <RefreshCw size={16} />}
          </Button>
          <Button onClick={() => setShowAddForm((s) => !s)} className="px-3">
            <Plus size={16} />
          </Button>
        </div>
      </div>

      {showAddForm && (
        <div className="p-4 mb-4 bg-bg-primary rounded-lg border border-border-color space-y-3">
          <Text variant={TextVariants.body} weight={TextWeights.medium}>
            {t('settings.modelLibrary.addTitle')}
          </Text>
          <Text variant={TextVariants.small}>{t('settings.modelLibrary.addDescription')}</Text>
          {addFile ? (
            <div className="flex items-center gap-2 px-3 py-2 bg-surface rounded-md border border-border-color">
              <FolderOpen size={16} className="shrink-0" />
              <Text variant={TextVariants.small} className="truncate flex-1">
                {addFile}
              </Text>
              <button
                onClick={() => setAddFile(null)}
                className="p-1 rounded hover:bg-card-active transition-colors shrink-0"
              >
                <X size={14} />
              </button>
            </div>
          ) : (
            <div className="flex gap-2">
              <Input
                className="flex-1"
                value={addUrl}
                onChange={(e: any) => setAddUrl(e.target.value)}
                onKeyDown={(e: any) => e.stopPropagation()}
                placeholder="https://huggingface.co/…/model.onnx"
              />
              <Button onClick={handleBrowse} className="px-3 shrink-0" disabled={!!addUrl}>
                <FolderOpen size={16} className="mr-2" />
                {t('settings.modelLibrary.browseButton')}
              </Button>
            </div>
          )}
          <div className="flex gap-3">
            <Input
              className="flex-1"
              value={addName}
              onChange={(e: any) => setAddName(e.target.value)}
              onKeyDown={(e: any) => e.stopPropagation()}
              placeholder={t('settings.modelLibrary.addNamePlaceholder')}
            />
            <div className="w-44">
              <Dropdown options={taskOptions} value={addTask} onChange={(v: string) => setAddTask(v)} />
            </div>
            <Button onClick={handleAdd} disabled={isAdding || (!addUrl && !addFile) || !addName}>
              {isAdding ? <Loader2 className="animate-spin mr-2" size={16} /> : <Plus className="mr-2" size={16} />}
              {t('settings.modelLibrary.addButton')}
            </Button>
          </div>
        </div>
      )}

      <div className="mb-4">
        <div className="flex gap-2">
          <Input
            className="flex-1"
            value={searchQuery}
            onChange={(e: any) => setSearchQuery(e.target.value)}
            onKeyDown={(e: any) => {
              e.stopPropagation();
              if (e.key === 'Enter') handleSearch();
            }}
            placeholder={t('settings.modelLibrary.searchPlaceholder')}
          />
          <Button onClick={handleSearch} disabled={isSearching || !searchQuery.trim()} className="px-3 shrink-0">
            {isSearching ? <Loader2 className="animate-spin" size={16} /> : <Search size={16} />}
          </Button>
        </div>

        {searchResults !== null && (
          <div className="mt-3 space-y-3 max-h-96 overflow-y-auto pr-1">
            {searchResults.length === 0 && (
              <Text variant={TextVariants.small}>{t('settings.modelLibrary.noResults')}</Text>
            )}
            {searchResults.map((repo) => (
              <div key={repo.repoId} className="p-3 bg-bg-primary rounded-lg border border-border-color">
                <div className="flex items-center justify-between mb-2">
                  <Text variant={TextVariants.small} weight={TextWeights.semibold} className="truncate">
                    {repo.repoId}
                  </Text>
                  <Text variant={TextVariants.small} className="shrink-0 ml-2">
                    ↓ {repo.downloads >= 1000 ? `${(repo.downloads / 1000).toFixed(1)}k` : repo.downloads}
                  </Text>
                </div>
                <div className="space-y-1.5">
                  {repo.files.map((file) => {
                    const key = `${repo.repoId}/${file.filename}`;
                    const alreadyBusy = installingKey !== null;
                    return (
                      <div key={key} className="flex items-center gap-2">
                        <Text variant={TextVariants.small} className="flex-1 truncate font-mono">
                          {file.filename}
                        </Text>
                        {file.needsConversion && (
                          <Text
                            as="span"
                            variant={TextVariants.small}
                            className="shrink-0 px-1.5 py-0.5 rounded-sm bg-surface border border-border-color"
                          >
                            {t('settings.modelLibrary.needsConversion')}
                          </Text>
                        )}
                        {file.sizeBytes > 0 && (
                          <Text variant={TextVariants.small} className="shrink-0">
                            {formatSize(file.sizeBytes)}
                          </Text>
                        )}
                        <div className="w-40 shrink-0">
                          <Dropdown
                            options={taskOptions}
                            value={installTasks[key] || 'upscale'}
                            onChange={(v: string) => setInstallTasks((prev) => ({ ...prev, [key]: v }))}
                          />
                        </div>
                        <Button
                          onClick={() => handleInstall(repo, file)}
                          disabled={alreadyBusy}
                          className="px-3 shrink-0"
                        >
                          {installingKey === key ? (
                            <Loader2 className="animate-spin" size={16} />
                          ) : (
                            <Download size={16} />
                          )}
                        </Button>
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {(isAdding || installingKey) && convertStatus && (
        <Text variant={TextVariants.small} className="mb-3 flex items-center gap-2">
          <Loader2 className="animate-spin shrink-0" size={14} />
          {convertStatus}
        </Text>
      )}

      {error && (
        <Text color={TextColors.error} variant={TextVariants.small} className="mb-3 block">
          {error}
        </Text>
      )}

      <div className="space-y-6">
        {grouped.map(({ task, items }) => (
          <div key={task}>
            <Text variant={TextVariants.small} weight={TextWeights.semibold} className="uppercase tracking-wide mb-2 block">
              {taskLabel(task)}
            </Text>
            <div className="space-y-2">
              {items.map((m) => (
                <div
                  key={m.id}
                  className="flex items-center gap-3 p-3 bg-bg-primary rounded-lg border border-border-color"
                >
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <Text variant={TextVariants.body} weight={TextWeights.medium}>
                        {m.displayName}
                      </Text>
                      {m.downloaded && <CheckCircle size={14} className="text-green-500 shrink-0" />}
                    </div>
                    {m.description && (
                      <Text variant={TextVariants.small} className="block mt-0.5">
                        {m.description}
                      </Text>
                    )}
                  </div>
                  {m.sizeBytes > 0 && (
                    <Text variant={TextVariants.small} className="shrink-0">
                      {formatSize(m.sizeBytes)}
                    </Text>
                  )}
                  <div className="shrink-0">
                    {busyId === m.id ? (
                      <Loader2 className="animate-spin" size={18} />
                    ) : m.downloaded ? (
                      m.downloadable && (
                        <button
                          onClick={() => handleDelete(m.id)}
                          className="p-2 rounded-md text-text-secondary hover:text-red-400 hover:bg-card-active transition-colors"
                          title={t('settings.modelLibrary.delete')}
                        >
                          <Trash2 size={16} />
                        </button>
                      )
                    ) : m.downloadable ? (
                      <Button onClick={() => handleDownload(m.id)} className="px-3">
                        <Download size={16} className="mr-2" />
                        {t('settings.modelLibrary.download')}
                      </Button>
                    ) : (
                      <Text variant={TextVariants.small}>{t('modelRegistry.notDownloaded')}</Text>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};

export default ModelLibrary;
