import { CheckIcon, ChevronDownIcon, SearchIcon, XIcon } from 'lucide-react'
import { Popover } from 'radix-ui'
import { type ReactNode, useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { cn } from '@/lib/utils'

export interface MultiOption<V extends string | number> {
  value: V
  label: string
  /** Shown after the label in mono (codes, ids). */
  meta?: string
  /** Shown before the label (logos). */
  icon?: ReactNode
}

/**
 * Searchable multi-select: selected values as removable chips, options in a
 * popover with a filter. Order of `value` is kept; new picks go last.
 */
export function MultiSelect<V extends string | number>({
  label,
  options,
  value,
  onChange,
  placeholder,
  disabled,
  hint,
}: {
  label: string
  options: MultiOption<V>[]
  value: V[]
  onChange: (next: V[]) => void
  placeholder: string
  disabled?: boolean
  hint?: ReactNode
}) {
  const { t } = useTranslation('admin')
  const id = useId()
  const [open, setOpen] = useState(false)
  const [filter, setFilter] = useState('')
  const byValue = useMemo(() => new Map(options.map((o) => [o.value, o])), [options])
  const selected = value.map((v) => byValue.get(v) ?? { value: v, label: String(v) })
  const needle = filter.trim().toLowerCase()
  const shown = needle
    ? options.filter(
        (o) => o.label.toLowerCase().includes(needle) || o.meta?.toLowerCase().includes(needle),
      )
    : options
  const toggle = (v: V) =>
    onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v])

  return (
    <div className="flex flex-col gap-1.5">
      <span id={`${id}-label`} className="text-sm font-medium text-text">
        {label}
      </span>
      <div className="flex min-h-10 flex-wrap items-center gap-1.5 rounded-control border border-line bg-night-0 p-1.5">
        {selected.length === 0 && <span className="px-1.5 text-text-3">{placeholder}</span>}
        {selected.map((o) => (
          <span
            key={String(o.value)}
            className="inline-flex items-center gap-1.5 rounded-full border border-line bg-night-2 py-0.5 pr-1 pl-2 text-sm text-text"
          >
            {o.icon}
            {o.label}
            <button
              type="button"
              disabled={disabled}
              onClick={() => toggle(o.value)}
              aria-label={t('categories.multi.remove', { name: o.label })}
              className="inline-flex size-5 cursor-pointer items-center justify-center rounded-full text-text-3 hover:bg-night-1 hover:text-text"
            >
              <XIcon aria-hidden className="size-3.5" />
            </button>
          </span>
        ))}
        <Popover.Root open={open} onOpenChange={setOpen}>
          <Popover.Trigger asChild>
            <button
              type="button"
              disabled={disabled}
              aria-labelledby={`${id}-label`}
              className="ml-auto inline-flex h-7 cursor-pointer items-center gap-1 rounded-full px-2.5 text-sm text-text-2 hover:bg-night-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-50"
            >
              {t('categories.multi.choose')}
              <ChevronDownIcon aria-hidden className="size-4" />
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content
              align="end"
              sideOffset={6}
              className="z-50 w-72 rounded-card border border-line bg-night-2 p-1.5 text-text shadow-lg"
            >
              <label className="flex items-center gap-2 rounded-control border border-line bg-night-0 px-2.5">
                <SearchIcon aria-hidden className="size-4 text-text-3" />
                <input
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  placeholder={t('categories.multi.filter')}
                  aria-label={t('categories.multi.filterLabel', { label })}
                  className="h-9 w-full bg-transparent text-base text-text outline-hidden placeholder:text-text-3"
                />
              </label>
              <ul
                role="listbox"
                aria-multiselectable
                aria-label={label}
                className="mt-1.5 max-h-72 overflow-y-auto"
              >
                {shown.length === 0 && (
                  <li className="px-2.5 py-2 text-sm text-text-3">{t('categories.multi.none')}</li>
                )}
                {shown.map((o) => {
                  const isSelected = value.includes(o.value)
                  return (
                    <li
                      key={String(o.value)}
                      role="option"
                      aria-selected={isSelected}
                      tabIndex={0}
                      onClick={() => toggle(o.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault()
                          toggle(o.value)
                        }
                      }}
                      className={cn(
                        'flex cursor-pointer items-center gap-2 rounded-control px-2.5 py-1.5 text-sm outline-hidden',
                        'hover:bg-night-1 focus-visible:bg-night-1 focus-visible:ring-2 focus-visible:ring-polaris/50',
                      )}
                    >
                      <span
                        aria-hidden
                        className={cn(
                          'inline-flex size-4 shrink-0 items-center justify-center rounded-[4px] border',
                          isSelected ? 'border-text-2 bg-text-2 text-night-0' : 'border-line',
                        )}
                      >
                        {isSelected && <CheckIcon className="size-3" />}
                      </span>
                      {o.icon}
                      <span className="truncate">{o.label}</span>
                      {o.meta && (
                        <span className="type-data ml-auto text-xs text-text-3">{o.meta}</span>
                      )}
                    </li>
                  )
                })}
              </ul>
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
      </div>
      {hint && <div className="text-sm text-text-3">{hint}</div>}
    </div>
  )
}

/** Native select styled to the tokens, with a label. */
export function SelectField({
  label,
  value,
  onChange,
  children,
  disabled,
  hint,
  hideLabel = false,
}: {
  label: string
  value: string
  onChange: (value: string) => void
  children: ReactNode
  disabled?: boolean
  hint?: ReactNode
  /** Only for screen readers, where the row around the select already says what it is for. */
  hideLabel?: boolean
}) {
  const id = useId()
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className={cn('text-sm font-medium text-text', hideLabel && 'sr-only')}>
        {label}
      </label>
      <select
        id={id}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className={cn(
          'h-10 w-full cursor-pointer rounded-control border border-line bg-night-0 px-3 text-base text-text transition-colors outline-hidden',
          'hover:border-text-3 focus-visible:border-polaris focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-0 focus-visible:outline-polaris/40',
          'disabled:cursor-not-allowed disabled:opacity-60',
        )}
      >
        {children}
      </select>
      {hint && <div className="text-sm text-text-3">{hint}</div>}
    </div>
  )
}
