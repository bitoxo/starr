import {
  CompassIcon,
  EllipsisIcon,
  InboxIcon,
  LockOpenIcon,
  SearchIcon,
  UserIcon,
  UsersIcon,
  ClapperboardIcon,
  type LucideIcon,
} from 'lucide-react'
import { useId, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation, useNavigate } from 'react-router'

import { Logo } from '@/components/Logo'
import { NotificationBell } from '@/components/NotificationBell'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { cn } from '@/lib/utils'

import { AccountMenu, type ShellVariant } from './AccountMenu'
import { ADMIN_NAV, FOCUS_SEARCH, isActive, type NavEntry } from './nav'

/** Below 1024 px: wordmark, search (members), bell and the account menu. No drawer. */
export function MobileTopBar({
  variant,
  home,
  homeLabel,
  tag,
  searchTo = '/search',
}: {
  variant: ShellVariant
  home: string
  homeLabel: string
  tag?: ReactNode
  /** The search page the button opens (the private area's own inside it). */
  searchTo?: string
}) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  return (
    <header className="flex h-14 items-center gap-1 border-b border-line bg-night-0 pr-2 pl-4 lg:hidden">
      <Link to={home} aria-label={homeLabel} className="flex h-11 items-center rounded-control">
        <Logo markClassName="size-6" className="[&>span:last-child]:text-xl" />
      </Link>
      {tag}
      <div className="ml-auto flex items-center">
        {variant === 'member' && (
          <button
            type="button"
            onClick={() => void navigate(searchTo, { state: FOCUS_SEARCH })}
            aria-label={t('nav.openSearch')}
            className="grid size-11 cursor-pointer place-items-center rounded-full text-text-2 transition-colors hover:bg-night-2 hover:text-text"
          >
            <SearchIcon aria-hidden className="size-5" strokeWidth={1.75} />
          </button>
        )}
        <NotificationBell scope={variant} />
        <AccountMenu variant={variant} layout="icon" />
      </div>
    </header>
  )
}

interface Tab {
  to: string
  icon: LucideIcon
  label: string
  active: boolean
  badge?: string | null
  /** What the count means, as the link's description (the name stays the label). */
  badgeLabel?: string
}

function TabLink({ tab }: { tab: Tab }) {
  const Icon = tab.icon
  const descriptionId = useId()
  return (
    <Link
      to={tab.to}
      aria-current={tab.active ? 'page' : undefined}
      aria-describedby={tab.badge ? descriptionId : undefined}
      className={cn(
        'relative flex min-h-12 flex-1 flex-col items-center justify-center gap-0.5 rounded-card text-xs transition-colors',
        tab.active ? 'text-text' : 'text-text-3 hover:text-text',
      )}
    >
      <span
        className={cn(
          'relative grid h-7 w-12 place-items-center rounded-full transition-colors',
          tab.active && 'bg-night-2',
        )}
      >
        <Icon aria-hidden className="size-5" strokeWidth={1.75} />
        {tab.badge && (
          <span
            aria-hidden
            className="chip type-data absolute -top-1 left-7 min-w-4 rounded-full px-1 text-center text-[10px] leading-4 font-semibold [--chip:var(--color-warn)]"
          >
            {tab.badge}
          </span>
        )}
      </span>
      {tab.label}
      {tab.badge && tab.badgeLabel && (
        <span id={descriptionId} hidden>
          {tab.badgeLabel}
        </span>
      )}
    </Link>
  )
}

function TabBar({ label, children }: { label: string; children: ReactNode }) {
  return (
    <nav
      aria-label={label}
      className="sticky bottom-0 z-40 flex gap-1 border-t border-line bg-night-1 px-2 pt-1 pb-[max(0.25rem,env(safe-area-inset-bottom))] lg:hidden"
    >
      {children}
    </nav>
  )
}

/**
 * The member tab bar below 1024 px (spec 0008 "Member › Mobile", F-6): Discover (also on the
 * category pages), Search, Requests, Profile (with My services), and Private only while the
 * session has unlocked it — absent otherwise.
 */
export function MemberTabBar({ privateOpen }: { privateOpen: boolean }) {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const tabs: Tab[] = [
    {
      to: '/',
      icon: CompassIcon,
      label: t('nav.discover'),
      active: pathname === '/' || pathname.startsWith('/discover/'),
    },
    { to: '/search', icon: SearchIcon, label: t('nav.search'), active: pathname === '/search' },
    {
      to: '/requests',
      icon: InboxIcon,
      label: t('nav.requests'),
      active: pathname === '/requests',
    },
    {
      to: '/profile',
      icon: UserIcon,
      label: t('nav.profile'),
      active: pathname.startsWith('/profile') || pathname === '/services',
    },
  ]
  if (privateOpen) {
    tabs.push({
      to: '/private',
      icon: LockOpenIcon,
      label: t('nav.private'),
      active: pathname === '/private' || pathname.startsWith('/private/'),
    })
  }
  return (
    <TabBar label={t('nav.member')}>
      {tabs.map((tab) => (
        <TabLink key={tab.to} tab={tab} />
      ))}
    </TabBar>
  )
}

const MORE: NavEntry[] = ADMIN_NAV.flatMap((group) => group.items).filter(
  (entry) => !['/admin/inbox', '/admin/requests', '/admin/people/members'].includes(entry.to),
)

/**
 * The admin tab bar below 1024 px: Inbox with its count, Requests, People, and More, a menu
 * with every other admin page.
 */
export function AdminTabBar({ inbox }: { inbox: number }) {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const tabs: Tab[] = [
    {
      to: '/admin/inbox',
      icon: InboxIcon,
      label: t('nav.inbox'),
      active: pathname === '/admin/inbox',
      badge: inbox > 0 ? (inbox > 9 ? '9+' : String(inbox)) : null,
      badgeLabel: t('nav.inboxBadge', { count: inbox }),
    },
    {
      to: '/admin/requests',
      icon: ClapperboardIcon,
      label: t('nav.adminRequests'),
      active: pathname === '/admin/requests',
    },
    {
      to: '/admin/people/members',
      icon: UsersIcon,
      label: t('nav.people'),
      active: pathname.startsWith('/admin/people/members'),
    },
  ]
  const moreActive = MORE.some((entry) => isActive(entry, pathname))
  return (
    <TabBar label={t('nav.admin')}>
      {tabs.map((tab) => (
        <TabLink key={tab.to} tab={tab} />
      ))}
      <DropdownMenu>
        <DropdownMenuTrigger
          aria-label={t('nav.moreLabel')}
          className={cn(
            'flex min-h-12 flex-1 cursor-pointer flex-col items-center justify-center gap-0.5 rounded-card text-xs transition-colors data-[state=open]:text-text',
            moreActive ? 'text-text' : 'text-text-3 hover:text-text',
          )}
        >
          <span
            className={cn(
              'grid h-7 w-12 place-items-center rounded-full transition-colors',
              moreActive && 'bg-night-2',
            )}
          >
            <EllipsisIcon aria-hidden className="size-5" strokeWidth={1.75} />
          </span>
          <span aria-hidden>{t('nav.more')}</span>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="top" align="end" className="w-60">
          {ADMIN_NAV.filter((group) => group.items.some((entry) => MORE.includes(entry))).map(
            (group, index) => (
              <div key={group.id}>
                {index > 0 && <DropdownMenuSeparator />}
                {group.heading && (
                  <DropdownMenuLabel>{t(group.heading as 'nav.about')}</DropdownMenuLabel>
                )}
                {group.items
                  .filter((entry) => MORE.includes(entry))
                  .map((entry) => {
                    const Icon = entry.icon
                    return (
                      <DropdownMenuItem key={entry.to} asChild>
                        <Link
                          to={entry.to}
                          aria-current={isActive(entry, pathname) ? 'page' : undefined}
                          className="min-h-11"
                        >
                          <Icon aria-hidden />
                          {t(entry.label as 'nav.about')}
                        </Link>
                      </DropdownMenuItem>
                    )
                  })}
              </div>
            ),
          )}
        </DropdownMenuContent>
      </DropdownMenu>
    </TabBar>
  )
}
