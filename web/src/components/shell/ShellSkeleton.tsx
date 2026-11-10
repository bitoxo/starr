import { useTranslation } from 'react-i18next'

import { cn } from '@/lib/utils'

import { readSidebarMode } from './prefs'
import { useMediaQuery, WIDE } from './useSidebar'

const block = 'rounded-card bg-night-2/70 motion-safe:animate-pulse'

/**
 * The shell's shape while `/auth/me` is still on its way (spec 0008 MEM-13, success criterion
 * 8): sidebar or top and tab bar, a search line and a lane of covers, so the page never starts
 * blank and nothing jumps when the real shell arrives. No router hooks: it also stands in while
 * the first route loads.
 */
export function ShellSkeleton({ variant }: { variant: 'member' | 'admin' }) {
  const { t } = useTranslation()
  const expanded = useMediaQuery(WIDE) && readSidebarMode() !== 'rail'
  const rows = variant === 'admin' ? 12 : 7
  return (
    <div
      data-shell-skeleton
      aria-busy
      className={cn(
        'flex min-h-dvh flex-col lg:grid',
        expanded ? 'lg:grid-cols-[240px_minmax(0,1fr)]' : 'lg:grid-cols-[72px_minmax(0,1fr)]',
      )}
    >
      <p role="status" className="sr-only">
        {t('shell.loading')}
      </p>
      <div aria-hidden className="hidden border-r border-line bg-night-1 lg:block">
        <div className="sticky top-0 flex h-dvh flex-col gap-1 px-3 py-4">
          <div className={cn(block, 'mb-5 h-10', expanded ? 'w-28' : 'w-10 self-center')} />
          {Array.from({ length: rows }, (_, i) => (
            <div key={i} className={cn(block, 'h-10', !expanded && 'w-10 self-center')} />
          ))}
          <div className={cn(block, 'mt-auto h-12', !expanded && 'w-10 self-center')} />
        </div>
      </div>
      <div aria-hidden className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-14 items-center gap-3 border-b border-line px-4 lg:hidden">
          <div className={cn(block, 'h-7 w-24')} />
          <div className={cn(block, 'ml-auto size-9 rounded-full')} />
          <div className={cn(block, 'size-9 rounded-full')} />
        </div>
        <div
          className={cn(
            'mx-auto w-full flex-1',
            variant === 'admin'
              ? 'max-w-7xl px-4 py-6 sm:px-8'
              : 'max-w-[96rem] px-4 py-8 sm:px-10 lg:py-14',
          )}
        >
          {variant === 'member' ? (
            <>
              <div className={cn(block, 'mb-10 hidden h-11 max-w-xl rounded-full lg:block')} />
              <div className={cn(block, 'mb-10 h-10 w-56')} />
              <div className={cn(block, 'mb-4 h-7 w-40')} />
              <div className="flex gap-4 overflow-hidden">
                {Array.from({ length: 7 }, (_, i) => (
                  <div key={i} className={cn(block, 'aspect-[2/3] w-36 shrink-0 sm:w-44')} />
                ))}
              </div>
            </>
          ) : (
            <>
              <div className={cn(block, 'mb-2 h-8 w-48')} />
              <div className={cn(block, 'mb-6 h-4 w-96 max-w-full')} />
              <div className={cn(block, 'h-72 rounded-panel')} />
            </>
          )}
        </div>
        <div className="flex h-14 items-center justify-around border-t border-line bg-night-1 lg:hidden">
          {Array.from({ length: 4 }, (_, i) => (
            <div key={i} className={cn(block, 'size-6 rounded-full')} />
          ))}
        </div>
      </div>
    </div>
  )
}

/** While the first route loads (before any router state exists), by the address alone. */
export function BootSkeleton() {
  return (
    <ShellSkeleton variant={window.location.pathname.startsWith('/admin') ? 'admin' : 'member'} />
  )
}
