import { Dialog as DialogPrimitive } from 'radix-ui'
import { type ReactNode, useLayoutEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'

import type { TitleRef } from '@/api/discover'
import { useApiErrorMessage } from '@/api/errors'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { Button } from '@/components/ui/button'
import { isNotFound } from '@/lib/detail'
import { useDetailClose } from '@/lib/detailStage'
import { COVER_MORPH, takeCoverMorph } from '@/lib/useTitleSheet'
import { cn } from '@/lib/utils'

/**
 * The detail cover (or its skeleton): when a poster lent it the shared view-transition name
 * (`useTitleSheet().open`), it takes the name over while the new view renders, so the browser
 * morphs the poster into it (A7), and gives it up once the morph is done.
 */
export function MorphTarget({ children, className }: { children: ReactNode; className?: string }) {
  const node = useRef<HTMLSpanElement>(null)
  useLayoutEffect(() => {
    const el = node.current
    if (!el || !takeCoverMorph()) return
    el.style.setProperty('view-transition-name', COVER_MORPH)
    const timer = window.setTimeout(() => el.style.removeProperty('view-transition-name'), 1000)
    return () => window.clearTimeout(timer)
  }, [])
  return (
    <span ref={node} className={cn('block', className)}>
      {children}
    </span>
  )
}

/**
 * What a view shows before its data is there: the skeleton in the view's shape, "not found"
 * at once for a title the catalog does not know (MEM-32), or the error with a retry.
 */
export function DetailLoad({
  kind,
  error,
  onRetry,
}: {
  kind: TitleRef['kind']
  error: unknown
  onRetry: () => void
}) {
  const { t } = useTranslation('member')
  const errorMessage = useApiErrorMessage()
  if (error != null && isNotFound(error)) return <NotFound />
  if (error != null) {
    return (
      <div className="flex flex-col gap-5 px-5 pt-20 pb-10 sm:px-10">
        <DialogPrimitive.Title className="type-display text-2xl font-bold text-text">
          {t('detail.loadError')}
        </DialogPrimitive.Title>
        <QueryError message={errorMessage(error)} onRetry={onRetry} />
      </div>
    )
  }
  return <DetailSkeleton kind={kind} />
}

/** "Not found", shown at once for an unknown or malformed `?title=` (MEM-32). */
export function NotFound() {
  const { t } = useTranslation(['member', 'common'])
  const close = useDetailClose()
  return (
    <div className="flex min-h-[50vh] flex-col items-start justify-center gap-3 px-5 py-20 sm:px-10">
      <DialogPrimitive.Title className="type-display text-3xl font-extrabold text-text">
        {t('detail.notFoundTitle')}
      </DialogPrimitive.Title>
      <p className="max-w-prose text-base text-text-2">{t('detail.notFoundText')}</p>
      <Button className="mt-2" onClick={close}>
        {t('common:actions.close')}
      </Button>
    </div>
  )
}

/** The overlay's shape while the title loads (F10): backdrop, cover, title lines, tabs. */
function DetailSkeleton({ kind }: { kind: TitleRef['kind'] }) {
  const { t } = useTranslation('member')
  const music = kind === 'album' || kind === 'artist'
  return (
    <div aria-busy className="flex flex-col">
      <DialogPrimitive.Title className="sr-only">{t('detail.loading')}</DialogPrimitive.Title>
      {music ? (
        <div className="h-16 sm:h-20" />
      ) : (
        <Skeleton className="aspect-[21/8] min-h-56 w-full rounded-none" />
      )}
      <div
        className={cn(
          'grid items-end gap-4 px-5 sm:gap-6 sm:px-10',
          music
            ? 'grid-cols-[7rem_minmax(0,1fr)] sm:grid-cols-[11rem_minmax(0,1fr)]'
            : '-mt-20 grid-cols-[6rem_minmax(0,1fr)] sm:-mt-28 sm:grid-cols-[9.5rem_minmax(0,1fr)]',
        )}
      >
        <MorphTarget>
          <Skeleton
            className={cn(
              'w-full',
              kind === 'artist'
                ? 'aspect-square rounded-full'
                : music
                  ? 'aspect-square rounded-card'
                  : 'aspect-2/3 rounded-card',
            )}
          />
        </MorphTarget>
        <div className="flex flex-col gap-3 pb-1">
          <Skeleton className="h-4 w-24" />
          <Skeleton className="h-9 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
        </div>
      </div>
      <div className="flex gap-3 px-5 pt-8 sm:px-10">
        <Skeleton className="h-9 w-24" />
        <Skeleton className="h-9 w-24" />
        <Skeleton className="h-9 w-24" />
      </div>
      <div className="flex flex-col gap-2 px-5 pt-6 pb-10 sm:px-10">
        <Skeleton className="h-4 w-full max-w-2xl" />
        <Skeleton className="h-4 w-full max-w-xl" />
        <Skeleton className="h-4 w-full max-w-lg" />
      </div>
    </div>
  )
}
