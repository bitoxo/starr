import { Suspense } from 'react'
import { useTranslation } from 'react-i18next'
import { NavLink, Outlet } from 'react-router'

import { usePrivateArea } from '@/api/private'
import { useSession } from '@/auth/session'
import { PrivateArea } from '@/components/PrivateArea'
import { SceneSkeletons, SCENE_GRID } from '@/components/private/SceneGrid'
import { Skeleton } from '@/components/Skeleton'
import { PRIVATE_REQUESTS } from '@/lib/privateNav'
import { cn } from '@/lib/utils'
import { NotFound } from '@/pages/ErrorPage'

/**
 * Layout of every private route: the gate decides whether the area exists at all. While the
 * session has not unlocked it, nothing is asked and the page is the shell's own "not found",
 * byte for byte (spec 0008 PRIV-03): no loading state or request tells the area apart.
 */
export function PrivateLayout() {
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  const area = usePrivateArea(unlocked)
  if (!unlocked) return <NotFound />
  if (area.isPending) return <PrivatePageSkeleton />
  if (area.isError) return <NotFound />
  return (
    <PrivateArea>
      <PrivateSubnav />
      {/* The pages load as their own chunk, only once the area is open (slice 12). */}
      <Suspense fallback={<PrivatePageSkeleton />}>
        <Outlet />
      </Suspense>
    </PrivateArea>
  )
}

export function PrivatePageSkeleton() {
  return (
    <div aria-busy className="flex flex-col gap-8">
      <Skeleton className="h-10 w-48 rounded-full" />
      <ul className={SCENE_GRID}>
        <SceneSkeletons count={6} />
      </ul>
    </div>
  )
}

/** Below 1024 px the sidebar's sub-items live here: Browse · Your requests · Following. */
function PrivateSubnav() {
  const { t } = useTranslation()
  const items = [
    { to: '/private', label: t('nav.privateBrowse'), end: true },
    { to: PRIVATE_REQUESTS, label: t('nav.privateRequests'), end: false },
    { to: '/private/following', label: t('nav.privateFollowing'), end: false },
  ]
  return (
    <nav
      aria-label={t('nav.private')}
      className="-mx-4 -mt-4 mb-6 flex gap-1 overflow-x-auto border-b border-line px-4 [scrollbar-width:none] lg:hidden"
    >
      {items.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.end}
          className={({ isActive }) =>
            cn(
              'relative flex h-11 shrink-0 items-center px-3 text-sm whitespace-nowrap',
              isActive
                ? 'text-text after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-text'
                : 'text-text-2 hover:text-text',
            )
          }
        >
          {item.label}
        </NavLink>
      ))}
    </nav>
  )
}
