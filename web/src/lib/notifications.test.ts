import { describe, expect, it } from 'vitest'

import { ADMIN_KINDS, MEMBER_KINDS } from '@/api/notifications'

import {
  badgeText,
  memberNotificationLink,
  notificationOpenIn,
  notificationTone,
  subjectLabel,
  toggleKind,
} from './notifications'

describe('notifications', () => {
  it('names the subject with year and seasons', () => {
    expect(subjectLabel({ title: 'Inception', year: 2010, seasons: [] })).toBe('Inception (2010)')
    expect(subjectLabel({ title: 'Dark', year: null, seasons: [1, 12] })).toBe('Dark · S01, S12')
  })

  it('caps the badge and hides it at zero', () => {
    expect(badgeText(0)).toBeNull()
    expect(badgeText(3)).toBe('3')
    expect(badgeText(42)).toBe('9+')
  })

  it('gives failures the bad tone and availability the ok tone', () => {
    expect(notificationTone('request_failed')).toBe('bad')
    expect(notificationTone('acquisition_failed')).toBe('bad')
    expect(notificationTone('request_available')).toBe('ok')
    expect(notificationTone('import_blocked')).toBe('warn')
    expect(notificationTone('account_privileged')).toBe('bad')
    expect(notificationTone('account_locked')).toBe('warn')
  })

  it('toggles kinds in display order', () => {
    expect(toggleKind([], 'request_available', MEMBER_KINDS)).toEqual(['request_available'])
    expect(toggleKind(['request_available'], 'request_approved', MEMBER_KINDS)).toEqual([
      'request_approved',
      'request_available',
    ])
    expect(toggleKind(['approval_pending'], 'approval_pending', ADMIN_KINDS)).toEqual([])
    expect(toggleKind(['import_blocked'], 'approval_pending', ADMIN_KINDS)).toEqual([
      'approval_pending',
      'import_blocked',
    ])
  })

  it('opens a member notification on its title over the highlighted request row (MEM-21)', () => {
    const n = { kind: 'request_available', category: 'movies', requestId: 3 } as const
    expect(memberNotificationLink(n, { kind: 'movie', titleId: '27205' })).toBe(
      '/requests?request=3&title=movie-27205',
    )
    // Without the title at hand: the row only.
    expect(memberNotificationLink(n, undefined)).toBe('/requests?request=3')
    expect(memberNotificationLink({ ...n, requestId: null }, undefined)).toBe('/requests')
    expect(
      memberNotificationLink(
        { kind: 'account_activate', category: '', requestId: null },
        undefined,
      ),
    ).toBe('/services')
  })

  it('keeps private notifications inside the private area', () => {
    const scene = { kind: 'request_approved', category: 'adult', requestId: 12 } as const
    expect(memberNotificationLink(scene, { kind: 'scene', titleId: 'abc' })).toBe(
      '/private/requests?request=12',
    )
    expect(
      memberNotificationLink({ kind: 'follow_capped', category: 'adult', requestId: null }, null),
    ).toBe('/private/following')
  })

  it('offers "Open in" only for ready notifications, each server once', () => {
    const jelly = { name: 'Jellyfin', service: 'jellyfin', url: 'https://j/1' }
    const emby = { name: 'Emby', service: 'emby', url: 'https://e/1' }
    const request = { openIn: [jelly], items: [{ openIn: [jelly, emby] }, { openIn: undefined }] }
    expect(notificationOpenIn('request_available', request)).toEqual([jelly, emby])
    expect(notificationOpenIn('request_partially_available', request)).toEqual([jelly, emby])
    expect(notificationOpenIn('request_approved', request)).toEqual([])
    expect(notificationOpenIn('request_available', undefined)).toEqual([])
  })
})
