import {
  ActivityIcon,
  ClapperboardIcon,
  CompassIcon,
  ContactIcon,
  DownloadIcon,
  FilmIcon,
  HeartIcon,
  InboxIcon,
  InfoIcon,
  LayersIcon,
  ListChecksIcon,
  LockIcon,
  LockOpenIcon,
  MailIcon,
  MegaphoneIcon,
  MusicIcon,
  SearchIcon,
  ServerIcon,
  ShieldIcon,
  TvIcon,
  UsersIcon,
  type LucideIcon,
} from 'lucide-react'

import type { Category } from '@/components/CategoryDot'

/** One navigation target. `label` is a `common` i18n key. */
export interface NavEntry {
  to: string
  icon: LucideIcon
  label: string
  /** Active only on exactly `to` (default: also below it). */
  end?: boolean
  /** Active on these paths as well (e.g. Discover on category pages). */
  also?: RegExp
  /** Category items carry their dot (and stay icons in the rail). */
  dot?: Category
  /** Indented under the item before it. */
  sub?: boolean
}

/** A run of entries, optionally under a heading (`common` key); groups are separated. */
export interface NavGroup {
  id: string
  heading?: string
  items: NavEntry[]
}

export function isActive(entry: NavEntry, pathname: string): boolean {
  if (entry.also?.test(pathname)) return true
  if (pathname === entry.to) return true
  return !entry.end && pathname.startsWith(`${entry.to}/`)
}

const CATEGORY_ICON: Record<'movies' | 'series' | 'music', LucideIcon> = {
  movies: FilmIcon,
  series: TvIcon,
  music: MusicIcon,
}

export type BrowseCategory = keyof typeof CATEGORY_ICON

export function isBrowseCategory(value: string): value is BrowseCategory {
  return value in CATEGORY_ICON
}

/**
 * The member navigation (spec 0008 IA): Discover with the categories the member may browse,
 * then Search, Requests and My services; the private area only while the session has unlocked
 * it — otherwise it is not in the list at all (spec 0007: absent, not hidden).
 */
export function memberNav(categories: BrowseCategory[], privateOpen: boolean): NavGroup[] {
  const groups: NavGroup[] = [
    {
      id: 'browse',
      items: [
        { to: '/', icon: CompassIcon, label: 'nav.discover', end: true },
        ...categories.map((category) => ({
          to: `/discover/${category}`,
          icon: CATEGORY_ICON[category],
          label: `categories.${category}`,
          dot: category,
          sub: true,
        })),
      ],
    },
    {
      id: 'mine',
      items: [
        { to: '/search', icon: SearchIcon, label: 'nav.search' },
        { to: '/requests', icon: InboxIcon, label: 'nav.requests' },
        { to: '/services', icon: ServerIcon, label: 'nav.myServices' },
      ],
    },
  ]
  if (privateOpen) {
    groups.push({
      id: 'private',
      items: [
        {
          to: '/private',
          icon: LockOpenIcon,
          label: 'nav.private',
          // Browse, search and the scene, performer and studio pages.
          end: true,
          also: /^\/private\/(search|scenes|performers|studios)(\/|$)/,
        },
        { to: '/private/requests', icon: ListChecksIcon, label: 'nav.privateRequests', sub: true },
        { to: '/private/following', icon: HeartIcon, label: 'nav.privateFollowing', sub: true },
      ],
    })
  }
  return groups
}

/** The admin navigation, grouped (spec 0008 Q22). Inbox carries the count of what waits. */
export const ADMIN_NAV: NavGroup[] = [
  { id: 'inbox', items: [{ to: '/admin/inbox', icon: InboxIcon, label: 'nav.inbox' }] },
  {
    id: 'requests',
    heading: 'nav.groupRequests',
    items: [
      { to: '/admin/requests', icon: ClapperboardIcon, label: 'nav.adminRequests' },
      { to: '/admin/downloads', icon: DownloadIcon, label: 'nav.downloads' },
    ],
  },
  {
    id: 'people',
    heading: 'nav.groupPeople',
    items: [
      { to: '/admin/people/members', icon: UsersIcon, label: 'nav.members' },
      { to: '/admin/people/invitations', icon: MailIcon, label: 'nav.invitations' },
      { to: '/admin/people/admins', icon: ShieldIcon, label: 'nav.admins' },
    ],
  },
  {
    id: 'media',
    heading: 'nav.groupMedia',
    items: [
      { to: '/admin/media/services', icon: ServerIcon, label: 'nav.services' },
      { to: '/admin/media/categories', icon: LayersIcon, label: 'nav.categories' },
      { to: '/admin/media/accounts', icon: ContactIcon, label: 'nav.accounts' },
    ],
  },
  {
    id: 'system',
    heading: 'nav.groupSystem',
    items: [
      { to: '/admin/system/runs', icon: ActivityIcon, label: 'nav.runs' },
      { to: '/admin/system/notifications', icon: MegaphoneIcon, label: 'nav.notifications' },
      { to: '/admin/system/security', icon: LockIcon, label: 'nav.security' },
      { to: '/admin/system/about', icon: InfoIcon, label: 'nav.about' },
    ],
  },
]

export const ADMIN_INBOX = '/admin/inbox'

/** Navigation state that asks the shell to put focus into the search field on arrival. */
export const FOCUS_SEARCH = { focusSearch: true } as const
