import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation } from 'react-router'

import { titleWithCount } from '@/lib/attention'

/** Path prefix → `common` key of its title; the first match wins. */
const TITLES: [RegExp, string][] = [
  [/^\/$/, 'nav.discover'],
  [/^\/discover\/(movies|series|music)(\/|$)/, ''],
  [/^\/search$/, 'nav.search'],
  [/^\/requests$/, 'nav.requests'],
  [/^\/services$/, 'nav.myServices'],
  [/^\/profile(\/|$)/, 'nav.profile'],
  [/^\/admin\/inbox$/, 'nav.inbox'],
  [/^\/admin\/requests$/, 'nav.adminRequests'],
  [/^\/admin\/downloads$/, 'nav.downloads'],
  [/^\/admin\/people\/members(\/[^/]+)?$/, 'nav.members'],
  [/^\/admin\/people\/invitations$/, 'nav.invitations'],
  [/^\/admin\/people\/admins$/, 'nav.admins'],
  [/^\/admin\/media\/services$/, 'nav.services'],
  [/^\/admin\/media\/categories(\/[^/]+)?$/, 'nav.categories'],
  [/^\/admin\/media\/accounts$/, 'nav.accounts'],
  [/^\/admin\/system\/runs$/, 'nav.runs'],
  [/^\/admin\/system\/notifications$/, 'nav.notifications'],
  [/^\/admin\/system\/security$/, 'nav.security'],
  [/^\/admin\/system\/about$/, 'nav.about'],
]

/**
 * The `common` i18n key that names a route, or `null` for "just Starr". The private area never
 * gets a title of its own, so neither the tab, the history nor a shared screen says what is open
 * there (spec 0008 MEM-18); unknown paths get none either, so a closed private URL and any other
 * missing page stay indistinguishable (PRIV-03).
 */
export function routeTitleKey(pathname: string): string | null {
  for (const [pattern, key] of TITLES) {
    const match = pattern.exec(pathname)
    if (match) return key || `categories.${match[1]}`
  }
  return null
}

/**
 * Keeps `document.title` in step with the route: "Requests · Starr", or "Starr". Admin pages
 * lead with the Inbox count, "(3) Requests · Starr" (spec 0008 F9: the same number as the badge).
 */
export function useRouteTitle(count = 0) {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const key = routeTitleKey(pathname)
  const app = t('app.name')
  const title = titleWithCount(
    key ? t('app.pageTitle', { page: t(key as 'nav.discover'), app }) : app,
    count,
  )
  useEffect(() => {
    document.title = title
  }, [title])
  // Leaving the shell (sign-out, login page) leaves no page name behind.
  useEffect(
    () => () => {
      document.title = app
    },
    [app],
  )
}
