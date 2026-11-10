import type { AdminRequest, AdminRequestDetail, Acquisition } from '@/api/requests'
import type { HeldTitle } from '@/api/services'
import i18n from '@/i18n'

/**
 * Neutral rows (spec 0007 Q9 B): the API leaves out everything that names a private title
 * while the admin's session has not unlocked the private category (`redacted`). Every view
 * then shows "Private category · request #12" in place of the title, no cover and no member
 * name, without each table knowing about it.
 */
export function neutralRequest(r: AdminRequest): AdminRequest {
  if (!r.redacted) return r
  return {
    ...r,
    title: i18n.t('privacy.request', { id: r.id }),
    poster: null,
    year: null,
    member: { ...r.member, displayName: i18n.t('privacy.member'), username: '' },
  }
}

export function neutralRequests(list: AdminRequest[]): AdminRequest[] {
  return list.map(neutralRequest)
}

export function neutralDetail(d: AdminRequestDetail): AdminRequestDetail {
  return { ...d, request: neutralRequest(d.request) }
}

export function neutralAcquisitions(list: Acquisition[]): Acquisition[] {
  return list.map((a) =>
    a.redacted
      ? { ...a, title: i18n.t('privacy.acquisition', { id: a.id }), poster: null, year: null }
      : a,
  )
}

export function neutralHeld(list: HeldTitle[]): HeldTitle[] {
  return list.map((h) => (h.redacted ? { ...h, title: i18n.t('privacy.title') } : h))
}
