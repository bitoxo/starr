import { RepeatIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'

import { useApiErrorMessage } from '@/api/errors'
import {
  type PrivateFollow,
  type PrivateFollowKind,
  useFollowPrivate,
  usePrivateFollows,
  useRestorePrivateFollow,
  useUnfollowPrivate,
} from '@/api/private'
import { EntityTileBody } from '@/components/private/EntityTile'
import { QueryError } from '@/components/QueryError'
import { Skeleton } from '@/components/Skeleton'
import { StatusChip } from '@/components/StatusChip'
import { Button } from '@/components/ui/button'
import { formatDate, formatDay, formatNumber } from '@/lib/format'
import { privateToast } from '@/lib/privateGate'
import { entityPath, privateUndoToast } from '@/lib/privateNav'
import { useFocusNextOnRemoval } from '@/lib/returnFocus'
import { cn } from '@/lib/utils'

/*
 * Performer and studio follows of the private category (spec 0007 slice 5, Q4; spec 0008
 * PRIV-09, PRIV-17). Render only inside `PrivateArea`: they name private performers and studios
 * and show their images. Unfollowing acts at once and offers Undo, which brings the follow back
 * with its original cutoff (not today's).
 */

/** "Up to 10 new scenes per week (7 left this week)": the weekly cap in numbers (PRIV-09). */
function CapLine({ follow, className }: { follow: PrivateFollow; className?: string }) {
  const { t } = useTranslation('member')
  return (
    <span className={className}>
      {t('privateFollows.cap', {
        cap: formatNumber(follow.weeklyCap),
        left: formatNumber(follow.remainingThisWeek),
      })}
    </span>
  )
}

/** Unfollow now, with an undo that restores the follow exactly (PRIV-17). */
function useUnfollowWithUndo() {
  const { t } = useTranslation('member')
  const remove = useUnfollowPrivate()
  const restore = useRestorePrivateFollow()
  const errorMessage = useApiErrorMessage()
  const onError = (error: unknown) => privateToast(errorMessage(error), {}, 'error')
  return {
    pending: remove.isPending,
    // The card that unfollowed is gone once the list refetches, and `mutate` callbacks of an
    // unmounted card never run: both calls are awaited instead.
    unfollow: (id: number, options: { onError?: () => void } = {}) => {
      remove
        .mutateAsync(id)
        .then(() =>
          privateUndoToast(t('privateFollows.toasts.unfollowed'), () => {
            restore
              .mutateAsync(id)
              .then(() => privateToast(t('privateFollows.toasts.restored')))
              .catch(onError)
          }),
        )
        .catch((error: unknown) => {
          options.onError?.()
          onError(error)
        })
    },
  }
}

/** "Weekly limit reached · 2 newer scenes not requested · Show them". */
function CappedHint({ follow, link }: { follow: PrivateFollow; link: boolean }) {
  const { t } = useTranslation('member')
  const count = follow.cappedScenes ?? 0
  const path = follow.entityId ? entityPath(follow.kind, follow.entityId) : null
  return (
    <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-text-2">
      <StatusChip tone="warn">{t('privateFollows.limitReached')}</StatusChip>
      <span>{t('privateFollows.capped', { count })}</span>
      {link && path && (
        <Link to={path} className="rounded-control text-text underline underline-offset-4">
          {t('privateFollows.showThem')}
        </Link>
      )}
    </span>
  )
}

/**
 * Follow or unfollow a performer or studio from its page: new scenes are requested in the
 * member's name, at most the weekly cap; the rest show as a hint here.
 */
export function PrivateFollowControl({
  kind,
  id,
  follow,
  canFollow,
}: {
  kind: PrivateFollowKind
  id: string
  follow: PrivateFollow | null | undefined
  /** The category takes requests (new scenes would become requests). */
  canFollow: boolean
}) {
  const { t } = useTranslation('member')
  const create = useFollowPrivate()
  const remove = useUnfollowWithUndo()
  const errorMessage = useApiErrorMessage()
  const busy = create.isPending || remove.pending
  if (!follow && !canFollow) return null

  const toggle = () =>
    follow
      ? remove.unfollow(follow.id)
      : create.mutate(
          { kind, id },
          {
            onSuccess: () => privateToast(t('privateFollows.toasts.followed')),
            onError: (error) => privateToast(errorMessage(error), {}, 'error'),
          },
        )

  return (
    <section
      aria-labelledby="private-follow-heading"
      className="flex flex-col gap-3 rounded-card border border-line bg-night-1 px-4 py-4 sm:flex-row sm:items-center sm:justify-between sm:gap-6"
    >
      <div className="flex gap-3">
        <RepeatIcon
          aria-hidden
          strokeWidth={1.75}
          className={cn('mt-1 size-4 shrink-0', follow ? 'text-text' : 'text-text-3')}
        />
        <div className="flex flex-col gap-1">
          <h2 id="private-follow-heading" className="text-base font-medium text-text">
            {follow
              ? t(`privateFollows.followingTitle_${kind}`)
              : t(`privateFollows.followTitle_${kind}`)}
          </h2>
          <p className="max-w-prose text-sm text-text-3">
            {follow?.knownThrough
              ? t('privateFollows.followingHint', { date: formatDay(follow.knownThrough) })
              : t('privateFollows.followHint')}
          </p>
          {follow && <CapLine follow={follow} className="text-sm text-text-2" />}
          {follow?.cappedScenes != null && <CappedHint follow={follow} link={false} />}
        </div>
      </div>
      <Button
        size="sm"
        variant={follow ? 'ghost' : 'secondary'}
        className="shrink-0 self-start"
        disabled={busy}
        aria-pressed={follow != null}
        onClick={toggle}
      >
        {follow ? t('follows.unfollow') : t('follows.follow')}
      </Button>
    </section>
  )
}

function FollowCard({ follow }: { follow: PrivateFollow }) {
  const { t } = useTranslation('member')
  const remove = useUnfollowWithUndo()
  const { ref, arm, disarm } = useFocusNextOnRemoval<HTMLLIElement>()
  const path = follow.entityId ? entityPath(follow.kind, follow.entityId) : null
  const round = follow.kind === 'performer'
  const body = (
    <EntityTileBody kind={follow.kind} image={follow.image} name={follow.name} following={false} />
  )
  return (
    <li
      ref={ref}
      className={cn('flex min-w-0 flex-col gap-2', round && 'items-center text-center')}
    >
      {path ? (
        <Link to={path} className="group w-full rounded-card focus-visible:outline-none">
          {body}
        </Link>
      ) : (
        body
      )}
      <CapLine follow={follow} className="text-sm text-text-2" />
      {follow.cappedScenes != null && <CappedHint follow={follow} link />}
      <span className="text-xs text-text-3">
        {follow.knownThrough
          ? t('privateFollows.after', { date: formatDay(follow.knownThrough) })
          : t('follows.since', { date: formatDate(follow.createdAt) })}
      </span>
      <Button
        size="sm"
        variant="ghost"
        className={round ? 'self-center' : '-ml-3 self-start'}
        disabled={remove.pending}
        aria-label={t('follows.unfollowLabel', { title: follow.name })}
        onClick={() => {
          arm()
          remove.unfollow(follow.id, { onError: disarm })
        }}
      >
        {t('follows.unfollow')}
      </Button>
    </li>
  )
}

function FollowGroup({ id, follows }: { id: 'performers' | 'studios'; follows: PrivateFollow[] }) {
  const { t } = useTranslation('member')
  if (follows.length === 0) return null
  return (
    <section aria-labelledby={`following-${id}`} className="flex flex-col gap-4">
      <h2
        id={`following-${id}`}
        tabIndex={-1}
        className="type-display flex items-baseline gap-2.5 text-xl font-bold text-text outline-none"
      >
        {t(`private.groups.${id}`)}{' '}
        <span className="type-data text-sm font-normal text-text-3">
          {formatNumber(follows.length)}
        </span>
      </h2>
      <ul
        className={cn(
          'grid gap-x-5 gap-y-8',
          id === 'performers'
            ? 'grid-cols-2 min-[480px]:grid-cols-3 md:grid-cols-4 xl:grid-cols-6'
            : 'grid-cols-1 min-[480px]:grid-cols-2 md:grid-cols-3 xl:grid-cols-4',
        )}
      >
        {follows.map((follow) => (
          <FollowCard key={follow.id} follow={follow} />
        ))}
      </ul>
    </section>
  )
}

/** The member's performer and studio follows (`/private/following`). */
export function PrivateFollowsSection() {
  const { t } = useTranslation('member')
  const follows = usePrivateFollows()
  const errorMessage = useApiErrorMessage()
  if (follows.isPending) {
    return (
      <div aria-busy className="grid grid-cols-2 gap-5 md:grid-cols-4">
        {Array.from({ length: 4 }, (_, i) => (
          <Skeleton key={i} className="aspect-square w-full rounded-full" />
        ))}
      </div>
    )
  }
  if (follows.isError) {
    return (
      <QueryError message={errorMessage(follows.error)} onRetry={() => void follows.refetch()} />
    )
  }
  if (follows.data.length === 0) {
    return (
      <div className="flex flex-col items-start gap-4 rounded-card border border-dashed border-line px-5 py-8">
        <p className="text-base text-text-2">{t('privateFollows.empty')}</p>
        <Button asChild variant="ghost" size="sm">
          <Link to="/private">{t('private.backToBrowsing')}</Link>
        </Button>
      </div>
    )
  }
  return (
    <div className="flex flex-col gap-12">
      <FollowGroup id="performers" follows={follows.data.filter((f) => f.kind === 'performer')} />
      <FollowGroup id="studios" follows={follows.data.filter((f) => f.kind === 'studio')} />
    </div>
  )
}
