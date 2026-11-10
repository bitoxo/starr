import { CheckIcon } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { privateImage } from '@/api/private'
import { useInPrivateArea } from '@/lib/privateArea'
import { cn } from '@/lib/utils'

/*
 * People and organisations of the private area (spec 0008 A6, PRIV-22): a performer is round
 * (portrait or monogram on `night-2`), a studio a 16:9 logo tile on `night-1` with the logo
 * contained, not cropped. Names below; a "Following" chip when followed. Never a category
 * colour. Images only inside `PrivateArea`, through the private source.
 */

export type EntityKind = 'performer' | 'studio'

function initial(name: string): string {
  return Array.from(name.trim())[0]?.toUpperCase() ?? '?'
}

/** The round performer shape or the studio logo tile, at the size its container gives. */
export function EntityShape({
  kind,
  image,
  name,
  compact = false,
  className,
}: {
  kind: EntityKind
  image?: string | null
  name: string
  /** A small studio tile: its fallback shows the initial, as the name sits beside it. */
  compact?: boolean
  className?: string
}) {
  const inArea = useInPrivateArea()
  const [failed, setFailed] = useState<string | null>(null)
  const shown = inArea && image && failed !== image ? image : null
  const shape =
    kind === 'performer'
      ? 'aspect-square rounded-full bg-night-2'
      : 'aspect-video rounded-card bg-night-1'
  return (
    <span
      aria-hidden
      className={cn('relative block w-full overflow-hidden border border-line', shape, className)}
    >
      {shown ? (
        <img
          src={privateImage(shown, kind === 'performer' ? '300' : '600')}
          alt=""
          loading="lazy"
          decoding="async"
          onError={() => setFailed(shown)}
          className={cn(
            'size-full',
            kind === 'performer' ? 'object-cover object-top' : 'object-contain p-2',
          )}
        />
      ) : (
        <span
          title={name}
          className={cn(
            'type-display grid size-full place-items-center font-extrabold text-text-2',
            kind === 'performer'
              ? 'text-[2.2em]'
              : compact
                ? 'text-xl'
                : 'px-3 text-center text-lg leading-tight',
          )}
        >
          {kind === 'performer' || compact ? (
            initial(name)
          ) : (
            <span className="line-clamp-2">{name}</span>
          )}
        </span>
      )}
    </span>
  )
}

/** "Following" as a quiet chip (form, not colour). */
export function FollowingChip({ className }: { className?: string }) {
  const { t } = useTranslation('member')
  return (
    <span
      className={cn(
        'inline-flex h-6 items-center gap-1 rounded-full bg-night-2 px-2 text-xs font-medium text-text',
        className,
      )}
    >
      <CheckIcon aria-hidden className="size-3" strokeWidth={2.4} />
      {t('private.following')}
    </span>
  )
}

/**
 * The body of an entity tile: shape, name, and below it an optional line (a count) and the
 * "Following" chip. The caller wraps it in a link or button and gives the accessible name.
 */
export function EntityTileBody({
  kind,
  image,
  name,
  following,
  sub,
  size = 'md',
}: {
  kind: EntityKind
  image?: string | null
  name: string
  following: boolean
  sub?: ReactNode
  size?: 'sm' | 'md'
}) {
  const round = kind === 'performer'
  return (
    <span
      className={cn(
        'flex min-w-0 flex-col gap-2',
        round ? 'items-center text-center' : 'items-start',
      )}
    >
      <span
        className={cn(
          'block rounded-full ring-polaris ring-offset-2 ring-offset-night-0 group-hover:ring-2 group-focus-visible:ring-2 motion-safe:transition-transform motion-safe:duration-(--dur-2) motion-safe:group-hover:-translate-y-[3px] motion-safe:group-hover:scale-[1.03] motion-safe:group-focus-visible:-translate-y-[3px] motion-safe:group-focus-visible:scale-[1.03] group-aria-pressed:ring-2',
          round ? (size === 'sm' ? 'w-20' : 'w-24') : 'w-full rounded-card',
        )}
      >
        <EntityShape kind={kind} image={image} name={name} />
      </span>
      <span className="flex max-w-full min-w-0 flex-col gap-1">
        <span title={name} className="truncate text-sm font-semibold text-text">
          {name}
        </span>
        {(sub || following) && (
          <span
            className={cn(
              'flex flex-wrap items-center gap-2 text-xs text-text-3',
              round && 'justify-center',
            )}
          >
            {sub}
            {following && <FollowingChip />}
          </span>
        )}
      </span>
    </span>
  )
}
