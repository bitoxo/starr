import { type KeyboardEvent, type ReactNode, useRef, useState } from 'react'

import { coverImage, tmdbImage } from '@/api/categories'
import { cn } from '@/lib/utils'

export interface SwitcherOption {
  id: string
  name: string
  /** A provider logo or an artist's round picture, before the name. */
  image?: ReactNode
}

/**
 * The switcher of a category page lane (spec 0008 Q6 A, Q13 B; `ProviderSwitcher` in the design
 * README): "On \<provider\>" chooses the provider, "From artists you follow" the artist. A tab
 * list over the lane it switches, so it is one tab stop: arrows, Home and End move and choose
 * at once, as the Discover list pills do (F4). The row scrolls sideways when it is wider than
 * its room. The choice lives in the URL (F6), which the caller keeps. Without a `panel` (the
 * "See all" grid's filters) it is a radio group instead, with the same keys.
 */
export function ProviderSwitcher({
  label,
  options,
  chosen,
  panel,
  onChoose,
}: {
  label: string
  options: SwitcherOption[]
  chosen: string
  /** Id of the lane it switches; absent: a filter (radio group). */
  panel?: string
  onChoose: (id: string) => void
}) {
  const filter = panel === undefined
  const tabs = useRef<(HTMLButtonElement | null)[]>([])
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return
    const from = options.findIndex((o) => o.id === chosen)
    const to: number | undefined = {
      ArrowRight: Math.min(options.length - 1, from + 1),
      ArrowLeft: Math.max(0, from - 1),
      Home: 0,
      End: options.length - 1,
    }[event.key]
    if (to === undefined) return
    event.preventDefault()
    const next = options[to]
    if (!next) return
    onChoose(next.id)
    tabs.current[to]?.focus()
  }
  return (
    <div
      role={filter ? 'radiogroup' : 'tablist'}
      aria-label={label}
      onKeyDown={onKeyDown}
      className="-mx-1 flex min-w-0 gap-1.5 overflow-x-auto px-1 py-1 [scrollbar-width:none]"
    >
      {options.map((option, i) => {
        const selected = option.id === chosen
        return (
          <button
            key={option.id}
            ref={(el) => {
              tabs.current[i] = el
            }}
            type="button"
            role={filter ? 'radio' : 'tab'}
            aria-selected={filter ? undefined : selected}
            aria-checked={filter ? selected : undefined}
            aria-controls={panel}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChoose(option.id)}
            className={cn(
              'inline-flex h-9 shrink-0 cursor-pointer items-center gap-2 rounded-full pr-3.5 text-sm font-medium whitespace-nowrap ring-1 transition-colors duration-(--dur-1) ring-inset max-lg:h-11',
              option.image ? 'pl-1.5' : 'pl-3.5',
              selected
                ? 'bg-night-2 text-text ring-text-3'
                : 'text-text-2 ring-line hover:text-text',
            )}
          >
            {option.image}
            {option.name}
          </button>
        )
      })}
    </div>
  )
}

/** A watch provider's logo (A6: a 1:1 rounded square), or its initial without one. */
export function ProviderLogo({
  logo,
  name,
  className,
}: {
  logo?: string | null
  name: string
  className?: string
}) {
  const shape = cn('size-6 shrink-0 rounded-control', className)
  return logo ? (
    <img
      src={tmdbImage(logo, 'w92')}
      alt=""
      decoding="async"
      className={cn(shape, 'object-cover')}
    />
  ) : (
    <span
      aria-hidden
      className={cn(
        shape,
        'type-display grid place-items-center bg-night-2 text-xs font-bold text-text-2',
      )}
    >
      {name.slice(0, 1)}
    </span>
  )
}

/**
 * An artist in a switcher (A6: round): their newest cover, or a monogram on night-2, also when
 * the archive has no cover (404).
 */
export function ArtistAvatar({ cover, name }: { cover?: string | null; name: string }) {
  const [failed, setFailed] = useState(false)
  return cover && !failed ? (
    <img
      src={coverImage(cover, '250')}
      alt=""
      decoding="async"
      onError={() => setFailed(true)}
      className="size-6 shrink-0 rounded-full bg-night-2 object-cover"
    />
  ) : (
    <span
      aria-hidden
      className="type-display grid size-6 shrink-0 place-items-center rounded-full bg-night-2 text-xs font-bold text-text-2"
    >
      {name.slice(0, 1)}
    </span>
  )
}
