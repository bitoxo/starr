import { ArrowDownIcon, ArrowUpIcon, ChevronsUpDownIcon, SearchIcon } from 'lucide-react'
import { type ReactNode, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { Checkbox } from '@/components/ui/checkbox'
import { compareText, formatNumber } from '@/lib/format'
import { cn } from '@/lib/utils'

/*
 * Starr's one table for admin lists (spec 0008 F5, Q21 B): sortable headers with `aria-sort`,
 * a search and filter slot, the count of what is shown, loading/empty/error states, a sticky
 * first column with inner horizontal scroll, and stacked cards below 768 px (one DOM: every
 * cell carries its column's name for the card layout). Sort and search are controlled so a page
 * can keep them in the URL (`useUrlState`, F6); without `onSortChange` the table keeps its own.
 */

export type Column<T> = {
  id: string
  header: string
  cell: (row: T) => ReactNode
  /** Makes the column sortable by this value. */
  sortValue?: (row: T) => string | number | null | undefined
  /** Width rules, e.g. `w-40` or `min-w-48`. */
  className?: string
  align?: 'start' | 'end'
}

export type Sort = { column: string; direction: 'asc' | 'desc' } | null

/**
 * Rows the user can choose for bulk actions (F5, ADM-31): a narrow column of its own before the
 * first one, with a checkbox in each selectable row and "select all" in its header, and `bar`
 * (the actions for what is chosen) sticky above the table while something is. Only shown rows
 * count as chosen, so a filter change never acts on rows the user no longer sees.
 */
export type TableSelection<T> = {
  selected: ReadonlySet<string | number>
  onChange: (next: Set<string | number>) => void
  isSelectable: (row: T) => boolean
  /** The checkbox's name, e.g. "Select Inception". */
  rowLabel: (row: T) => string
  allLabel: string
  /** Rendered while something is chosen; gets the chosen rows. */
  bar: (chosen: T[]) => ReactNode
}

export type TableSearch<T> = {
  label: string
  value: string
  onChange: (value: string) => void
  matches: (row: T, query: string) => boolean
}

const isMissing = (v: string | number | null | undefined) => v == null || v === ''

function compare(a: string | number | null | undefined, b: string | number | null | undefined) {
  // Missing values sort last either way.
  if (isMissing(a)) return isMissing(b) ? 0 : 1
  if (isMissing(b)) return -1
  if (typeof a === 'number' && typeof b === 'number') return a - b
  return compareText(String(a), String(b))
}

const ARIA_SORT = { asc: 'ascending', desc: 'descending' } as const

export function DataTable<T>({
  label,
  columns,
  rows,
  rowKey,
  sort: controlledSort,
  onSortChange,
  search,
  filters,
  loading = false,
  error,
  onRetry,
  empty,
  selection,
  className,
}: {
  /** The table's accessible name. */
  label: string
  columns: Column<T>[]
  /** Undefined while loading or after an error. */
  rows: T[] | undefined
  rowKey: (row: T) => string | number
  sort?: Sort
  onSortChange?: (sort: Sort) => void
  search?: TableSearch<T>
  /** Further filter controls beside the search (chips, selects), in the URL as well. */
  filters?: ReactNode
  loading?: boolean
  /** A plain sentence; shown instead of the rows with `onRetry`. */
  error?: string | null
  onRetry?: () => void
  empty?: ReactNode
  selection?: TableSelection<T>
  className?: string
}) {
  const { t } = useTranslation()
  const searchId = useId()
  const [ownSort, setOwnSort] = useState<Sort>(null)
  const sort = onSortChange ? (controlledSort ?? null) : (controlledSort ?? ownSort)
  const setSort = onSortChange ?? setOwnSort

  const query = search?.value.trim() ?? ''
  const all = rows ?? []
  const shown = all.filter((row) => !search || !query || search.matches(row, query))
  const sortColumn = columns.find((c) => c.id === sort?.column && c.sortValue)
  if (sort && sortColumn?.sortValue) {
    const value = sortColumn.sortValue
    const factor = sort.direction === 'asc' ? 1 : -1
    shown.sort((a, b) => {
      const order = compare(value(a), value(b))
      // Missing values stay last in both directions.
      const missing = isMissing(value(a)) || isMissing(value(b))
      return missing ? order : order * factor
    })
  }

  const selectable = selection ? shown.filter(selection.isSelectable) : []
  const chosen = selection ? selectable.filter((row) => selection.selected.has(rowKey(row))) : []
  const allChosen = selectable.length > 0 && chosen.length === selectable.length
  const choose = (row: T, on: boolean) => {
    if (!selection) return
    const next = new Set(chosen.map(rowKey))
    if (on) next.add(rowKey(row))
    else next.delete(rowKey(row))
    selection.onChange(next)
  }
  const checkbox = (row: T) =>
    selection?.isSelectable(row) ? (
      <Checkbox
        className="align-middle"
        aria-label={selection.rowLabel(row)}
        checked={selection.selected.has(rowKey(row))}
        onCheckedChange={(on) => choose(row, on === true)}
      />
    ) : null
  // Selection has its own narrow column (sticky like the first one, which then sits beside it).
  // On phone cards the checkbox sits beside the title cell, centred against it.
  const selectCell = 'sticky left-0 z-[1] w-11 max-w-11 min-w-11 py-2.5 pr-0 pl-4 align-middle'
  const firstLeft = selection ? 'left-11' : 'left-0'

  const toggle = (column: string) =>
    setSort(
      sort?.column === column && sort.direction === 'asc'
        ? { column, direction: 'desc' }
        : { column, direction: 'asc' },
    )

  const toolbar = (search || filters) && (
    <div className="flex flex-wrap items-center gap-3">
      {search && (
        <div className="relative w-full max-w-xs">
          <label htmlFor={searchId} className="sr-only">
            {search.label}
          </label>
          <SearchIcon
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-text-3"
          />
          <input
            id={searchId}
            type="search"
            value={search.value}
            placeholder={search.label}
            onChange={(e) => search.onChange(e.target.value)}
            className="h-9 w-full rounded-full border border-line bg-night-1 pr-3 pl-9 max-lg:h-11 text-sm text-text placeholder:text-text-3 hover:border-text-3"
          />
        </div>
      )}
      {filters}
    </div>
  )

  if (error) {
    return (
      <div className={cn('flex flex-col gap-4', className)}>
        {toolbar}
        <QueryError message={error} onRetry={onRetry} />
      </div>
    )
  }

  const busy = loading && rows === undefined
  return (
    <div className={cn('flex flex-col gap-4', className)}>
      {toolbar}
      {!busy && (
        <p role="status" className="type-data text-xs text-text-3">
          {shown.length === all.length
            ? t('table.count', { count: all.length, total: formatNumber(all.length) })
            : t('table.countFiltered', {
                count: all.length,
                shown: formatNumber(shown.length),
                total: formatNumber(all.length),
              })}
        </p>
      )}
      {selection && chosen.length > 0 && (
        <div
          role="region"
          aria-label={t('table.selected', { count: chosen.length })}
          className="sticky top-14 z-20 flex flex-wrap items-center gap-3 rounded-card border border-line bg-night-2 px-4 py-2.5 lg:top-2"
        >
          <span className="text-sm font-medium text-text" aria-live="polite">
            {t('table.selected', { count: chosen.length })}
          </span>
          <div className="ml-auto flex flex-wrap gap-2 max-md:grid max-md:w-full max-md:grid-cols-2 max-md:[&>*]:h-11">
            {selection.bar(chosen)}
          </div>
        </div>
      )}
      {!busy && shown.length === 0 ? (
        <p className="rounded-card border border-dashed border-line px-5 py-8 text-base text-text-2">
          {empty ?? t('table.empty')}
        </p>
      ) : (
        <div className="overflow-x-auto md:rounded-card md:border md:border-line">
          <table
            // Explicit roles: the card layout's display values drop the native ones.
            role="table"
            aria-label={label}
            aria-busy={busy || undefined}
            className="w-full border-separate border-spacing-0 text-sm max-md:block"
          >
            <thead role="rowgroup" className="bg-night-1 text-left max-md:hidden">
              <tr role="row">
                {selection && (
                  <th role="columnheader" scope="col" className={cn(selectCell, 'bg-night-1')}>
                    {selectable.length === 0 && (
                      <span className="sr-only">{selection.allLabel}</span>
                    )}
                    {selectable.length > 0 && (
                      <Checkbox
                        className="align-middle"
                        aria-label={selection.allLabel}
                        checked={allChosen ? true : chosen.length > 0 ? 'indeterminate' : false}
                        onCheckedChange={(on) =>
                          selection.onChange(
                            on === true ? new Set(selectable.map(rowKey)) : new Set(),
                          )
                        }
                      />
                    )}
                  </th>
                )}
                {columns.map((column, i) => (
                  <th
                    key={column.id}
                    role="columnheader"
                    scope="col"
                    aria-sort={
                      column.sortValue
                        ? sort?.column === column.id
                          ? ARIA_SORT[sort.direction]
                          : 'none'
                        : undefined
                    }
                    className={cn(
                      'type-data px-4 py-2.5 align-middle text-xs font-medium tracking-wide whitespace-nowrap text-text-3 uppercase',
                      i === 0 && cn('sticky z-[1] bg-night-1', firstLeft),
                      column.align === 'end' && 'text-right',
                      column.className,
                    )}
                  >
                    {column.sortValue ? (
                      <button
                        type="button"
                        onClick={() => toggle(column.id)}
                        className={cn(
                          'inline-flex cursor-pointer items-center gap-1 rounded-control uppercase hover:text-text',
                          sort?.column === column.id && 'text-text',
                        )}
                      >
                        {column.header}
                        {sort?.column === column.id ? (
                          sort.direction === 'asc' ? (
                            <ArrowUpIcon aria-hidden className="size-3.5" />
                          ) : (
                            <ArrowDownIcon aria-hidden className="size-3.5" />
                          )
                        ) : (
                          <ChevronsUpDownIcon aria-hidden className="size-3.5 opacity-60" />
                        )}
                      </button>
                    ) : (
                      column.header
                    )}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody
              role="rowgroup"
              // Row dividers belong to the cells, in the separate border model (see the table):
              // collapsed borders on a <tr> are painted by the table beneath the sticky first
              // cell, whose background covered them once a taller row left the following rows
              // on fractional pixel offsets (the gap below the paused Lidarr row on Services).
              className="max-md:flex max-md:flex-col max-md:gap-3 md:[&>tr+tr>td]:border-t md:[&>tr+tr>td]:border-line"
            >
              {busy
                ? Array.from({ length: 3 }, (_, r) => (
                    <tr key={r} role="row" className="max-md:block">
                      {selection && (
                        <td role="cell" className={cn(selectCell, 'bg-night-0 max-md:hidden')} />
                      )}
                      {columns.map((column) => (
                        <td key={column.id} role="cell" className="px-4 py-3 max-md:block">
                          <Skeleton className="h-4 w-3/4" />
                        </td>
                      ))}
                    </tr>
                  ))
                : shown.map((row) => (
                    <tr
                      key={rowKey(row)}
                      role="row"
                      className={cn(
                        'max-md:flex max-md:flex-col max-md:gap-2 max-md:rounded-card max-md:border max-md:border-line max-md:bg-night-1 max-md:p-4',
                        // With selection: checkbox | title on the first line, every other cell
                        // across the card below.
                        selection &&
                          'max-md:grid max-md:grid-cols-[auto_minmax(0,1fr)] max-md:gap-x-3 max-md:[&>td:nth-child(n+3)]:col-span-2 max-md:[&>td:empty]:hidden max-md:[&>td:empty+td]:col-span-2',
                      )}
                    >
                      {selection && (
                        <td
                          role="cell"
                          className={cn(
                            selectCell,
                            'bg-night-0 max-md:static max-md:flex max-md:w-auto max-md:min-w-0 max-md:items-center max-md:bg-transparent max-md:p-0',
                          )}
                        >
                          {checkbox(row)}
                        </td>
                      )}
                      {columns.map((column, i) => (
                        <td
                          key={column.id}
                          role="cell"
                          data-label={column.header}
                          className={cn(
                            'px-4 py-2.5 align-middle',
                            i === 0 &&
                              cn(
                                'sticky z-[1] bg-night-0 max-md:static max-md:bg-transparent',
                                firstLeft,
                              ),
                            column.align === 'end' && 'md:text-right',
                            // Phone cards: name above each value, the first cell is the title.
                            // A column's desktop width never squeezes its card line (slice 12).
                            'max-md:flex max-md:w-auto max-md:max-w-none max-md:min-w-0 max-md:items-baseline max-md:justify-between max-md:gap-4 max-md:p-0',
                            i > 0 &&
                              'max-md:before:type-data max-md:before:text-xs max-md:before:text-text-3 max-md:before:content-[attr(data-label)]',
                            i === 0 && 'max-md:text-base max-md:font-medium',
                            // Actions span the card's width on phones.
                            // (The selection checkbox keeps its size; its hit area grows by itself.)
                            'max-md:[&_a]:min-h-11 max-md:[&_button:not([role=checkbox])]:min-h-11',
                            column.className,
                          )}
                        >
                          {column.cell(row)}
                        </td>
                      ))}
                    </tr>
                  ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
