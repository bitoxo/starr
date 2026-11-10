import {
  Component,
  lazy,
  Suspense,
  useEffect,
  useState,
  type CSSProperties,
  type ReactNode,
} from 'react'
import { useTranslation } from 'react-i18next'
import { Outlet, useLocation, useNavigate, useNavigationType, useSearchParams } from 'react-router'

import { useAttention } from '@/api/attention'
import { useDiscoverCategories } from '@/api/discover'
import { homePath, useSession } from '@/auth/session'
import { NotificationBell } from '@/components/NotificationBell'
import {
  PrivateGateAnnouncer,
  PrivateGateIndicator,
  PrivateGateNotice,
} from '@/components/PrivateGate'
import { PrivateSearchPill } from '@/components/private/PrivateSearchPill'
import { SearchPill } from '@/components/SearchPill'
import { AccountMenu, type ShellVariant } from '@/components/shell/AccountMenu'
import { MemberViewStrip } from '@/components/shell/MemberViewStrip'
import { AdminTabBar, MemberTabBar, MobileTopBar } from '@/components/shell/MobileNav'
import {
  ADMIN_INBOX,
  ADMIN_NAV,
  FOCUS_SEARCH,
  isBrowseCategory,
  memberNav,
} from '@/components/shell/nav'
import { isShortcutTarget, useShortcutsEnabled } from '@/components/shell/prefs'
import { useRouteTitle } from '@/components/shell/routeTitle'
import { Sidebar, SidebarNav } from '@/components/shell/Sidebar'
import { useSidebar, type SidebarState } from '@/components/shell/useSidebar'
import { usePrivateGate } from '@/lib/privateGate'
import { cn } from '@/lib/utils'

export type { ShellVariant } from '@/components/shell/AccountMenu'

/*
 * The detail overlay is its own chunk (spec 0008 slice 12): fetched when `?title=` first appears,
 * or quietly once the page is idle. It is mounted (closed) as soon as the chunk is there, so the
 * first open does not suspend (React holds a revealed Suspense boundary back for ~300 ms), and
 * stays mounted so it can close and give focus back.
 */
const loadDetailView = () => import('@/components/DetailView')
const DetailViewHost = lazy(() => loadDetailView().then((m) => ({ default: m.DetailViewHost })))

/** A chunk that failed to load (offline, or gone after an update) never takes the page down. */
class QuietBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false }
  static getDerivedStateFromError() {
    return { failed: true }
  }
  render() {
    return this.state.failed ? null : this.props.children
  }
}

function LazyDetailView() {
  const [params] = useSearchParams()
  const wanted = params.has('title')
  const [mounted, setMounted] = useState(wanted)
  if (wanted && !mounted) setMounted(true)
  useEffect(() => {
    let live = true
    const warm = () =>
      void loadDetailView().then(
        () => live && setMounted(true),
        () => undefined,
      )
    if (typeof window.requestIdleCallback === 'function') {
      const id = window.requestIdleCallback(warm, { timeout: 4000 })
      return () => {
        live = false
        window.cancelIdleCallback(id)
      }
    }
    const id = window.setTimeout(warm, 2000)
    return () => {
      live = false
      window.clearTimeout(id)
    }
  }, [])
  if (!mounted) return null
  return (
    <QuietBoundary>
      <Suspense fallback={null}>
        <DetailViewHost />
      </Suspense>
    </QuietBoundary>
  )
}

/** Marks the admin area so it never gets confused with the member view. */
function AdminTag() {
  const { t } = useTranslation()
  return (
    <span className="type-data rounded-full border border-line px-2 py-0.5 text-xs text-text-2">
      {t('account.roleAdmin')}
    </span>
  )
}

/**
 * What waits for an admin (spec 0008 F9): one count from the attention list for the sidebar
 * badge, the tab bar, the Inbox heading and counters, the bell and `document.title`.
 */
function useInboxCount(): number {
  return useAttention().data?.total ?? 0
}

function searchField(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>('#main form[role="search"] input')
}

/**
 * Single-key shortcuts (spec 0008 F-1): `[` toggles the sidebar, `/` jumps to the search field.
 * Ignored while typing, with modifiers, and when switched off in this browser (WCAG 2.1.4).
 */
function useShellShortcuts(sidebar: SidebarState, variant: ShellVariant) {
  const enabled = useShortcutsEnabled()
  const navigate = useNavigate()
  const searchTo = useSearchTarget()
  const { desktop, toggle } = sidebar
  useEffect(() => {
    if (!enabled) return
    const onKey = (event: KeyboardEvent) => {
      if (!isShortcutTarget(event)) return
      if (event.key === '[' && desktop) {
        event.preventDefault()
        toggle()
      } else if (event.key === '/' && variant === 'member') {
        event.preventDefault()
        const field = searchField()
        if (field && field.offsetParent !== null) field.focus()
        else void navigate(searchTo, { state: FOCUS_SEARCH })
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [enabled, desktop, toggle, variant, navigate, searchTo])
}

/** Inside the unlocked private area (`/private…`) the header search is the area's own. */
function usePrivateSearchHere(): boolean {
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  const { pathname } = useLocation()
  return unlocked && (pathname === '/private' || pathname.startsWith('/private/'))
}

/** Where the search button and `/` go when no search field is on screen. */
function useSearchTarget(): string {
  return usePrivateSearchHere() ? '/private/search' : '/search'
}

/** Arriving with `FOCUS_SEARCH` (top bar search button, `/`) puts focus into the field. */
function useFocusSearchOnArrival() {
  const location = useLocation()
  // Only on the way there, not when Back returns to that entry (no surprise keyboard).
  const pushed = useNavigationType() === 'PUSH'
  const wanted =
    pushed && (location.state as { focusSearch?: boolean } | null)?.focusSearch === true
  useEffect(() => {
    if (wanted) searchField()?.focus()
  }, [wanted, location.key])
}

function MemberSidebarNav({ rail }: { rail: boolean }) {
  const { t } = useTranslation()
  // The private area is in the menu only while this session has unlocked it (spec 0007 Q7):
  // not rendered at all otherwise, not hidden.
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  const categories = (useDiscoverCategories().data ?? [])
    .filter((c) => c.discoverReady)
    .map((c) => c.category as string)
    .filter(isBrowseCategory)
  return <SidebarNav label={t('nav.member')} groups={memberNav(categories, unlocked)} rail={rail} />
}

function AdminSidebarNav({ rail, inbox }: { rail: boolean; inbox: number }) {
  const { t } = useTranslation()
  return (
    <SidebarNav
      label={t('nav.admin')}
      groups={ADMIN_NAV}
      rail={rail}
      badges={{ [ADMIN_INBOX]: { count: inbox, label: t('nav.inboxBadge', { count: inbox }) } }}
    />
  )
}

export function AppShell({ variant }: { variant: ShellVariant }) {
  return variant === 'admin' ? <AdminShell /> : <MemberShell />
}

function MemberShell() {
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  const { pathname } = useLocation()
  const privateSearch = usePrivateSearchHere()
  const searchTo = useSearchTarget()
  // Below 1024 px the search lives behind the top bar's button and on its own page.
  const onSearch = pathname === '/search' || (privateSearch && pathname === '/private/search')
  return (
    <Frame
      variant="member"
      searchTo={searchTo}
      nav={(rail) => <MemberSidebarNav rail={rail} />}
      tabBar={<MemberTabBar privateOpen={unlocked} />}
      header={
        // Above the Discover hero, which reaches up under this row.
        <div
          className={cn(
            'relative z-10 mb-8 flex items-center gap-4 lg:mb-10',
            !onSearch && 'max-lg:hidden',
          )}
        >
          <div className="min-w-0 flex-1">
            {/* Separate components: a private term never reaches the global pill. */}
            {privateSearch ? <PrivateSearchPill key="private" /> : <SearchPill key="all" />}
          </div>
          <div className="max-lg:hidden">
            <PrivateGateIndicator layout="bar" />
          </div>
        </div>
      }
    />
  )
}

function AdminShell() {
  const inbox = useInboxCount()
  return (
    <Frame
      variant="admin"
      nav={(rail) => <AdminSidebarNav rail={rail} inbox={inbox} />}
      tabBar={<AdminTabBar inbox={inbox} />}
      header={<AdminGateBar />}
      titleCount={inbox}
    />
  )
}

/**
 * The shell (spec 0008 IA, ADR 0015): collapsible sidebar at ≥ 1024 px, top bar and bottom tab
 * bar below, the private gate's indicator and notice, the member-view strip, and per-route
 * `document.title`.
 */
function Frame({
  variant,
  nav,
  tabBar,
  header,
  titleCount = 0,
  searchTo = '/search',
}: {
  variant: ShellVariant
  /** Where the top bar's search button goes. */
  searchTo?: string
  nav: (rail: boolean) => ReactNode
  tabBar: ReactNode
  header: ReactNode
  /** Admins: the Inbox count leads the tab title. */
  titleCount?: number
}) {
  const { t } = useTranslation()
  const sidebar = useSidebar()
  const home = homePath(variant)
  const homeLabel = variant === 'admin' ? t('app.adminHome') : t('app.home')
  usePrivateGate(home)
  useRouteTitle(titleCount)
  useShellShortcuts(sidebar, variant)
  useFocusSearchOnArrival()

  // The grid column follows the sidebar; the 1024–1279 px overlay keeps the rail's column.
  const column = sidebar.expanded && !sidebar.overlay ? '240px' : '72px'
  return (
    <div
      style={{ '--sidebar-width': column } as CSSProperties}
      className="flex min-h-dvh flex-col lg:grid lg:grid-cols-[var(--sidebar-width)_minmax(0,1fr)] lg:motion-safe:transition-[grid-template-columns] lg:motion-safe:duration-[var(--dur-3,320ms)] lg:motion-safe:ease-[cubic-bezier(0.65,0,0.35,1)]"
    >
      <a
        href="#main"
        className="sr-only z-50 rounded-full bg-polaris px-4 py-2 text-polaris-ink focus:not-sr-only focus:fixed focus:top-3 focus:left-3"
      >
        {t('nav.skipToContent')}
      </a>

      <Sidebar
        state={sidebar}
        home={home}
        homeLabel={homeLabel}
        tag={variant === 'admin' ? <AdminTag /> : undefined}
        nav={nav}
        footer={(rail) => (
          <>
            <NotificationBell scope={variant} layout={rail ? 'rail' : 'row'} />
            <AccountMenu variant={variant} layout={rail ? 'rail' : 'row'} />
          </>
        )}
      />

      <div className="flex min-w-0 flex-1 flex-col">
        {/* One sticky stack, so the strips never overlap each other or the top bar. */}
        <div className="sticky top-0 z-40">
          {variant === 'member' && <MemberViewStrip />}
          <MobileTopBar
            variant={variant}
            home={home}
            homeLabel={homeLabel}
            tag={variant === 'admin' ? <AdminTag /> : undefined}
            searchTo={searchTo}
          />
          <PrivateGateIndicator layout="strip" />
        </div>
        <PrivateGateAnnouncer />

        <main id="main" tabIndex={-1} className="min-w-0 flex-1 outline-none">
          <div
            className={cn(
              'mx-auto w-full',
              variant === 'admin'
                ? 'max-w-7xl px-4 py-6 sm:px-8'
                : 'max-w-[96rem] px-4 py-8 sm:px-10 lg:py-14',
            )}
          >
            {header}
            <PrivateGateNotice />
            <Outlet />
          </div>
        </main>
        {tabBar}
      </div>
      {variant === 'member' && <LazyDetailView />}
    </div>
  )
}

/** The admin shell has no search pill: the indicator gets its own line while unlocked. */
function AdminGateBar() {
  const unlocked = useSession().me?.privateCategory?.unlocked === true
  if (!unlocked) return null
  return (
    <div className="mb-4 flex justify-end max-lg:hidden">
      <PrivateGateIndicator layout="bar" />
    </div>
  )
}

/** Page title block. Member pages get the large display size, admin pages stay compact. */
export function PageHeader({
  variant,
  title,
  description,
  actions,
}: {
  variant: ShellVariant
  title: string
  description?: string
  actions?: ReactNode
}) {
  return (
    <div
      className={cn(
        'flex flex-wrap items-end justify-between gap-4',
        variant === 'admin' ? 'mb-6' : 'mb-10',
      )}
    >
      <div className="flex flex-col gap-1.5">
        <h1
          className={cn(
            'type-display font-extrabold text-text',
            variant === 'admin' ? 'text-2xl' : 'text-3xl sm:text-4xl',
          )}
        >
          {title}
        </h1>
        {description && <p className="text-base text-text-2">{description}</p>}
      </div>
      {actions}
    </div>
  )
}
