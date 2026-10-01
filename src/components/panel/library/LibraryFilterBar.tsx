import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import clsx from 'clsx';
import { Check, Flag, Star, X } from 'lucide-react';
import Dropdown from '../../ui/Dropdown';
import { useLibraryStore } from '../../../store/useLibraryStore';
import { COLOR_LABELS } from '../../../utils/adjustments';
import { EditedStatus, FlagFilter, RawStatus } from '../../ui/AppProperties';

/**
 * The library's filters in one row under the header, as Lightroom's
 * filter bar (toggled with \): flag, stars, colour label, file type and
 * edit status, with how many photos are showing and a way to clear them.
 */
export default function LibraryFilterBar({ shown, total }: { shown: number; total: number }) {
  const { t } = useTranslation();
  const filter = useLibraryStore((s) => s.filterCriteria);
  const setFilter = useLibraryStore((s) => s.setFilterCriteria);
  const flag: FlagFilter = filter.flag ?? 'all';
  const colors = useMemo(() => [...COLOR_LABELS, { name: 'none', color: '#9ca3af' }], []);
  const active =
    flag !== 'all' ||
    filter.rating !== 0 ||
    (filter.rawStatus && filter.rawStatus !== RawStatus.All) ||
    (filter.editedStatus && filter.editedStatus !== EditedStatus.All) ||
    (filter.colors || []).length > 0;

  const chip = (selected: boolean) =>
    clsx(
      'flex items-center gap-1 rounded-md px-2 py-1 text-xs transition-colors',
      selected ? 'bg-card-active text-text-primary' : 'text-text-secondary hover:bg-surface hover:text-text-primary',
    );
  const group = 'flex items-center gap-0.5 rounded-lg bg-bg-primary/60 p-0.5';
  const label = 'text-[11px] font-semibold uppercase tracking-[0.08em] text-text-secondary';

  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-surface px-4 py-2">
      <div className="flex items-center gap-2">
        <span className={label}>{t('library.filterBar.flag', { defaultValue: 'Flag' })}</span>
        <div className={group}>
          {(
            [
              ['all', t('library.filterBar.all', { defaultValue: 'All' }), null],
              [
                'picked',
                t('cull.picked', { defaultValue: 'Picked' }),
                <Flag key="p" size={12} className="fill-current" />,
              ],
              ['rejected', t('cull.rejected', { defaultValue: 'Rejected' }), <X key="r" size={13} />],
              ['unflagged', t('library.filterBar.unflagged', { defaultValue: 'Unflagged' }), null],
            ] as Array<[FlagFilter, string, React.ReactNode]>
          ).map(([value, text, icon]) => (
            <button
              key={value}
              type="button"
              aria-pressed={flag === value}
              onClick={() => setFilter((prev) => ({ ...prev, flag: value }))}
              className={chip(flag === value)}
            >
              {icon}
              {text}
            </button>
          ))}
        </div>
      </div>

      <div className="flex items-center gap-2">
        <span className={label}>{t('library.filterBar.rating', { defaultValue: 'Stars' })}</span>
        <div className={group}>
          <button
            type="button"
            aria-pressed={filter.rating === -1}
            onClick={() => setFilter((prev) => ({ ...prev, rating: prev.rating === -1 ? 0 : -1 }))}
            className={chip(filter.rating === -1)}
          >
            {t('library.filters.rating.unrated', { defaultValue: 'Unrated' })}
          </button>
          <div className="flex items-center px-1">
            {[1, 2, 3, 4, 5].map((n) => {
              const filled = filter.rating > 0 && n <= filter.rating;
              return (
                <button
                  key={n}
                  type="button"
                  onClick={() => setFilter((prev) => ({ ...prev, rating: prev.rating === n ? 0 : n }))}
                  className="p-0.5"
                  data-tooltip={
                    n === 5
                      ? t('library.filters.rating.fiveOnly', { defaultValue: '5 stars only' })
                      : t('ui.bottomBar.tooltips.filterRating', { defaultValue: 'Show {{count}}+ stars', count: n })
                  }
                >
                  <Star
                    size={14}
                    className={filled ? 'fill-accent text-accent' : 'text-text-secondary hover:text-accent'}
                  />
                </button>
              );
            })}
          </div>
        </div>
      </div>

      <div className="flex items-center gap-2">
        <span className={label}>{t('library.filterBar.label', { defaultValue: 'Label' })}</span>
        <div className="flex items-center gap-1.5">
          {colors.map((c) => {
            const selected = (filter.colors || []).includes(c.name);
            return (
              <button
                key={c.name}
                type="button"
                aria-pressed={selected}
                onClick={() =>
                  setFilter((prev) => {
                    const current = prev.colors || [];
                    return {
                      ...prev,
                      colors: current.includes(c.name) ? current.filter((x) => x !== c.name) : [...current, c.name],
                    };
                  })
                }
                className={clsx(
                  'flex h-4 w-4 items-center justify-center rounded-full transition-transform hover:scale-110',
                  selected && 'ring-2 ring-accent ring-offset-1 ring-offset-bg-secondary',
                )}
                style={{ backgroundColor: c.color }}
                data-tooltip={
                  c.name === 'none'
                    ? t('library.header.viewOptions.noLabel', { defaultValue: 'No label' })
                    : t(`contextMenus.colors.${c.name}`, { defaultValue: c.name })
                }
              >
                {selected && <Check size={10} className="text-white drop-shadow" />}
              </button>
            );
          })}
        </div>
      </div>

      <div className="w-40">
        <Dropdown
          options={[
            { value: RawStatus.All, label: t('library.filters.raw.all') },
            { value: RawStatus.RawOnly, label: t('library.filters.raw.rawOnly') },
            { value: RawStatus.NonRawOnly, label: t('library.filters.raw.nonRawOnly') },
            { value: RawStatus.RawOverNonRaw, label: t('library.filters.raw.preferRaw') },
          ]}
          value={filter.rawStatus || RawStatus.All}
          onChange={(value: RawStatus) => setFilter((prev) => ({ ...prev, rawStatus: value }))}
        />
      </div>
      <div className="w-40">
        <Dropdown
          options={[
            { value: EditedStatus.All, label: t('library.filters.edited.all') },
            { value: EditedStatus.EditedOnly, label: t('library.filters.edited.editedOnly') },
            { value: EditedStatus.UneditedOnly, label: t('library.filters.edited.uneditedOnly') },
          ]}
          value={filter.editedStatus || EditedStatus.All}
          onChange={(value: EditedStatus) => setFilter((prev) => ({ ...prev, editedStatus: value }))}
        />
      </div>

      <div className="ml-auto flex items-center gap-3 text-xs text-text-secondary">
        <span className="tabular-nums">
          {active
            ? t('library.filterBar.showingOf', { defaultValue: '{{shown}} of {{total}}', shown, total })
            : t('library.filterBar.count', { defaultValue: '{{count}} photos', count: total })}
        </span>
        {active && (
          <button
            type="button"
            onClick={() =>
              setFilter((prev) => ({
                ...prev,
                flag: 'all',
                rating: 0,
                rawStatus: RawStatus.All,
                editedStatus: EditedStatus.All,
                colors: [],
              }))
            }
            className="rounded-md px-2 py-1 hover:bg-surface hover:text-text-primary"
          >
            {t('library.filterBar.clear', { defaultValue: 'Clear filters' })}
          </button>
        )}
      </div>
    </div>
  );
}
