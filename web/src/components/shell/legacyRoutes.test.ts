import { describe, expect, it } from 'vitest'

import { legacyAdminTarget } from './legacyRoutes'

describe('legacyAdminTarget (spec 0008 Q22: old admin URLs keep working)', () => {
  it.each([
    ['/admin/needs-you', '/admin/inbox'],
    ['/admin/members', '/admin/people/members'],
    ['/admin/members/7', '/admin/people/members/7'],
    ['/admin/invitations', '/admin/people/invitations'],
    ['/admin/settings/admins', '/admin/people/admins'],
    ['/admin/services', '/admin/media/services'],
    ['/admin/accounts', '/admin/media/accounts'],
    ['/admin/categories', '/admin/media/categories'],
    ['/admin/runs', '/admin/system/runs'],
    ['/admin/settings', '/admin/system/about'],
    ['/admin/settings/security', '/admin/system/security'],
    ['/admin/settings/notifications', '/admin/system/notifications'],
  ])('moves %s to %s', (from, to) => {
    expect(legacyAdminTarget(from, '')).toBe(to)
  })

  it('keeps ?open=, ?run= and every other parameter', () => {
    expect(legacyAdminTarget('/admin/runs', '?run=12')).toBe('/admin/system/runs?run=12')
    expect(legacyAdminTarget('/admin/needs-you', '?open=approval-3')).toBe(
      '/admin/inbox?open=approval-3',
    )
    expect(legacyAdminTarget('/admin/accounts', '?service=4')).toBe(
      '/admin/media/accounts?service=4',
    )
  })

  it('turns the category parameter into the path', () => {
    expect(legacyAdminTarget('/admin/categories', '?category=music')).toBe(
      '/admin/media/categories/music',
    )
    expect(legacyAdminTarget('/admin/categories', '?category=series&x=1')).toBe(
      '/admin/media/categories/series?x=1',
    )
  })

  it('sends the acquisitions view of Requests to Downloads', () => {
    expect(legacyAdminTarget('/admin/requests', '?view=acquisitions&open=5')).toBe(
      '/admin/downloads?open=5',
    )
  })

  it('ignores a trailing slash', () => {
    expect(legacyAdminTarget('/admin/members/', '')).toBe('/admin/people/members')
    expect(legacyAdminTarget('/admin/members/7/', '')).toBe('/admin/people/members/7')
  })

  it('leaves current routes alone', () => {
    expect(legacyAdminTarget('/admin/requests', '?state=failed')).toBeNull()
    expect(legacyAdminTarget('/admin/inbox', '')).toBeNull()
    expect(legacyAdminTarget('/admin/people/members', '')).toBeNull()
    expect(legacyAdminTarget('/admin/members/7/extra', '')).toBeNull()
  })
})
