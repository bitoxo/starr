import { describe, expect, it } from 'vitest'

import { routeTitleKey } from './routeTitle'

describe('routeTitleKey (MEM-18: a title per route)', () => {
  it.each([
    ['/', 'nav.discover'],
    ['/discover/movies', 'categories.movies'],
    ['/discover/music', 'categories.music'],
    ['/search', 'nav.search'],
    ['/requests', 'nav.requests'],
    ['/services', 'nav.myServices'],
    ['/profile', 'nav.profile'],
    ['/profile/preferences', 'nav.profile'],
    ['/admin/inbox', 'nav.inbox'],
    ['/admin/requests', 'nav.adminRequests'],
    ['/admin/downloads', 'nav.downloads'],
    ['/admin/people/members', 'nav.members'],
    ['/admin/people/members/3', 'nav.members'],
    ['/admin/people/invitations', 'nav.invitations'],
    ['/admin/people/admins', 'nav.admins'],
    ['/admin/media/services', 'nav.services'],
    ['/admin/media/categories', 'nav.categories'],
    ['/admin/media/categories/series', 'nav.categories'],
    ['/admin/media/accounts', 'nav.accounts'],
    ['/admin/system/runs', 'nav.runs'],
    ['/admin/system/notifications', 'nav.notifications'],
    ['/admin/system/security', 'nav.security'],
    ['/admin/system/about', 'nav.about'],
  ])('%s → %s', (path, key) => {
    expect(routeTitleKey(path)).toBe(key)
  })

  it('stays neutral for the whole private area, whatever is open there', () => {
    for (const path of ['/private', '/private/scenes/abc', '/private/performers/x', '/private/']) {
      expect(routeTitleKey(path)).toBeNull()
    }
  })

  it('gives unknown paths no title, the same as a closed private area', () => {
    expect(routeTitleKey('/nope')).toBeNull()
    expect(routeTitleKey('/discover/adult')).toBeNull()
    expect(routeTitleKey('/admin/nope')).toBeNull()
  })
})
