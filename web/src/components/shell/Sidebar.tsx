import { PanelLeftCloseIcon, PanelLeftOpenIcon } from 'lucide-react'
import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'

import { CategoryDot } from '@/components/CategoryDot'
import { LogoMark } from '@/components/Logo'
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

import { isActive, type NavEntry, type NavGroup } from './nav'
import { useShortcutsEnabled } from './prefs'
import type { SidebarState } from './useSidebar'

/** A count shown on a nav item, with the sentence screen readers get for it. */
export interface NavBadge {
  count: number
  label: string
}

export const SIDEBAR_ID = 'sidebar'

const itemBase =
  'relative flex w-full items-center gap-3 rounded-card text-text-2 transition-colors duration-[var(--dur-1,120ms)] hover:bg-night-2/60 hover:text-text [&_svg]:size-4 [&_svg]:shrink-0'

/** Arrow keys move focus between items of the same nav (Tab still works as usual). */
function onNavKeyDown(event: KeyboardEvent<HTMLElement>) {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
  const items = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('[data-nav-item]'))
  const index = items.indexOf(document.activeElement as HTMLElement)
  if (index === -1) return
  event.preventDefault()
  const next = event.key === 'ArrowDown' ? index + 1 : index - 1
  items[(next + items.length) % items.length]?.focus()
}

/** In the rail every control names itself in a tooltip on hover and on focus. */
export function RailTip({
  rail,
  label,
  children,
}: {
  rail: boolean
  label: string
  children: ReactNode
}) {
  if (!rail) return children
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side="right" sideOffset={10}>
        {label}
      </TooltipContent>
    </Tooltip>
  )
}

function NavItem({
  entry,
  rail,
  badge,
  onNavigate,
}: {
  entry: NavEntry
  rail: boolean
  badge?: NavBadge
  onNavigate?: () => void
}) {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const descriptionId = useId()
  const active = isActive(entry, pathname)
  const label = t(entry.label as 'nav.discover')
  const Icon = entry.icon
  const count = badge && badge.count > 0 ? badge : null
  const shown = count ? (count.count > 99 ? '99+' : String(count.count)) : null

  return (
    <RailTip rail={rail} label={label}>
      <Link
        to={entry.to}
        data-nav-item
        aria-current={active ? 'page' : undefined}
        // The count is described, not part of the name (ADM-23): "Inbox", "3 items need you".
        aria-describedby={count ? descriptionId : undefined}
        onClick={onNavigate}
        className={cn(
          itemBase,
          'h-10 text-base',
          rail ? 'justify-center px-0' : entry.sub ? 'pr-3 pl-10 text-sm' : 'px-3',
          entry.sub && !rail && 'h-9',
          active && 'bg-night-2 font-medium text-text hover:bg-night-2',
        )}
      >
        <span className="relative grid size-4 shrink-0 place-items-center">
          {entry.dot && !rail ? (
            <CategoryDot category={entry.dot} />
          ) : (
            <Icon aria-hidden strokeWidth={1.75} />
          )}
          {entry.dot && rail && (
            <CategoryDot category={entry.dot} className="absolute -top-1 -right-1.5 size-1.5" />
          )}
          {shown && rail && (
            <span
              aria-hidden
              className="type-data absolute -top-2.5 left-2.5 min-w-4 rounded-full bg-line px-1 text-center text-[10px] leading-4 font-semibold text-text"
            >
              {shown}
            </span>
          )}
        </span>
        <span className={rail ? 'sr-only' : 'min-w-0 flex-1 truncate'}>{label}</span>
        {shown && !rail && (
          <span
            aria-hidden
            className="chip type-data rounded-full px-2 text-xs font-medium [--chip:var(--color-warn)]"
          >
            {shown}
          </span>
        )}
        {count && (
          <span id={descriptionId} hidden>
            {count.label}
          </span>
        )}
      </Link>
    </RailTip>
  )
}

/** Groups with headings when expanded, hairlines between them in the rail. */
export function SidebarNav({
  label,
  groups,
  rail,
  badges = {},
  onNavigate,
}: {
  label: string
  groups: NavGroup[]
  rail: boolean
  badges?: Record<string, NavBadge>
  onNavigate?: () => void
}) {
  const { t } = useTranslation()
  return (
    <nav aria-label={label} onKeyDown={onNavKeyDown} className="flex flex-col">
      {groups.map((group, index) => {
        const headingId = `nav-group-${group.id}`
        const items = group.items.map((entry) => (
          <NavItem
            key={entry.to}
            entry={entry}
            rail={rail}
            badge={badges[entry.to]}
            onNavigate={onNavigate}
          />
        ))
        return (
          <div key={group.id} className="flex flex-col gap-0.5">
            {index > 0 && (rail || !group.heading) && <div className="mx-2 my-2.5 h-px bg-line" />}
            {group.heading ? (
              <div role="group" aria-labelledby={headingId} className="flex flex-col gap-0.5">
                <p
                  id={headingId}
                  className={cn(
                    rail
                      ? 'sr-only'
                      : 'type-data px-3 pt-4 pb-1.5 text-xs tracking-wide text-text-3 uppercase',
                  )}
                >
                  {t(group.heading as 'nav.discover')}
                </p>
                {items}
              </div>
            ) : (
              items
            )}
          </div>
        )
      })}
    </nav>
  )
}

/**
 * The collapsible sidebar (spec 0008 "Sidebar behaviour"): 240 px expanded or a 72 px rail; at
 * 1024–1279 px the expanded state lies over the content and Esc or a click outside closes it.
 * Hidden below 1024 px, where the top bar and the tab bar take over.
 */
export function Sidebar({
  state,
  home,
  homeLabel,
  tag,
  nav,
  footer,
}: {
  state: SidebarState
  home: string
  homeLabel: string
  tag?: ReactNode
  nav: (rail: boolean) => ReactNode
  footer: (rail: boolean) => ReactNode
}) {
  const { t } = useTranslation()
  const shortcuts = useShortcutsEnabled()
  const panel = useRef<HTMLDivElement>(null)
  const toggleRef = useRef<HTMLButtonElement>(null)
  const rail = !state.expanded
  const { overlay, close } = state

  // The overlay goes away on Esc (focus back to the toggle) or a click elsewhere.
  useEffect(() => {
    if (!overlay) return
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return
      close()
      toggleRef.current?.focus()
    }
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Element
      // Menus and popovers opened from the panel live in portals: clicks there stay inside.
      if (
        panel.current?.contains(target) ||
        target.closest?.('[data-radix-popper-content-wrapper]')
      )
        return
      close()
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('pointerdown', onPointer)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('pointerdown', onPointer)
    }
  }, [overlay, close])

  const toggleLabel = state.expanded ? t('nav.collapseSidebar') : t('nav.expandSidebar')
  return (
    <TooltipProvider>
      {/* The aside stretches with the page, so its ground and border reach the bottom at
          any length; the panel inside stays in view while scrolling. */}
      <aside className="hidden border-r border-line bg-night-1 lg:block">
        <div
          ref={panel}
          id={SIDEBAR_ID}
          className={cn(
            'sticky top-0 flex h-dvh flex-col py-4',
            'px-3',
            overlay && 'fixed top-0 left-0 z-50 w-[240px] border-r border-line bg-night-1',
          )}
        >
          <div
            className={cn(
              'mb-5 flex items-center',
              rail ? 'flex-col gap-3' : 'h-10 justify-between gap-2 pl-1.5',
            )}
          >
            <div className="flex min-w-0 items-center gap-2">
              <Link
                to={home}
                aria-label={homeLabel}
                className="inline-flex items-center gap-2.5 rounded-control text-text"
              >
                <LogoMark className="size-7" />
                {!rail && (
                  <span className="wordmark text-2xl leading-none">{t('app.wordmark')}</span>
                )}
              </Link>
              {!rail && tag}
            </div>
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  ref={toggleRef}
                  type="button"
                  onClick={state.toggle}
                  aria-expanded={state.expanded}
                  aria-controls={SIDEBAR_ID}
                  aria-label={toggleLabel}
                  aria-keyshortcuts={shortcuts ? '[' : undefined}
                  className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-control text-text-3 transition-colors hover:bg-night-2 hover:text-text"
                >
                  {state.expanded ? (
                    <PanelLeftCloseIcon aria-hidden className="size-4" strokeWidth={1.75} />
                  ) : (
                    <PanelLeftOpenIcon aria-hidden className="size-4" strokeWidth={1.75} />
                  )}
                </button>
              </TooltipTrigger>
              <TooltipContent side="right" sideOffset={10}>
                {toggleLabel}
                {shortcuts && (
                  <kbd className="type-data ml-2 rounded-[4px] border border-line px-1 text-xs text-text-2">
                    [
                  </kbd>
                )}
              </TooltipContent>
            </Tooltip>
          </div>
          <div className="-mx-3 min-h-0 flex-1 overflow-y-auto px-3 pb-3">{nav(rail)}</div>
          <div className="flex flex-col gap-0.5 border-t border-line pt-3">{footer(rail)}</div>
        </div>
      </aside>
    </TooltipProvider>
  )
}
