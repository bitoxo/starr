import { describe, expect, it } from 'vitest'

import admin from '@/locales/en/admin.json'

import type { CategorySettings } from '@/api/categories'
import {
  exampleUrl,
  safeHttpUrl,
  sameOrigin,
  serviceDependencies,
  serviceErrorKey,
  servicesIntro,
  serviceStatus,
} from './services'

describe('exampleUrl (ADM-20)', () => {
  it('gives each type its own address and keeps public defaults', () => {
    expect(exampleUrl('sonarr')).toBe('http://sonarr:8989')
    expect(exampleUrl('navidrome')).toBe('http://navidrome:4533')
    expect(exampleUrl('tmdb', 'https://api.themoviedb.org')).toBe('https://api.themoviedb.org')
    expect(new Set(['radarr', 'sonarr', 'lidarr', 'whisparr'].map((t) => exampleUrl(t))).size).toBe(
      4,
    )
  })
})

describe('serviceDependencies (ADM-21)', () => {
  it('lists every role a service has, per category', () => {
    const role = (serviceId: number) => ({ serviceId })
    const categories = [
      { category: 'movies', catalogs: [role(1)], source: role(2), consumers: [role(3)] },
      { category: 'series', catalogs: [role(1)], source: null, consumers: [role(3)] },
    ] as unknown as CategorySettings[]
    expect(serviceDependencies(categories, 1)).toEqual([
      { category: 'movies', role: 'catalog' },
      { category: 'series', role: 'catalog' },
    ])
    expect(serviceDependencies(categories, 2)).toEqual([{ category: 'movies', role: 'source' }])
    expect(serviceDependencies(categories, 9)).toEqual([])
  })
})

describe('serviceStatus', () => {
  it('is pending until the first check and then follows the error class', () => {
    expect(serviceStatus({ lastCheckAt: null, lastError: null })).toBe('pending')
    const at = '2026-11-01T12:00:00Z'
    expect(serviceStatus({ lastCheckAt: at, lastError: null })).toBe('ok')
    expect(serviceStatus({ lastCheckAt: at, lastError: 'version_unsupported' })).toBe('warning')
    expect(serviceStatus({ lastCheckAt: at, lastError: 'throttled' })).toBe('warning')
    expect(serviceStatus({ lastCheckAt: at, lastError: 'credentials_invalid' })).toBe('error')
    // The service cannot be called at all until the key is re-entered.
    expect(serviceStatus({ lastCheckAt: at, lastError: 'credentials_unreadable' })).toBe('error')
  })
})

describe('serviceErrorKey', () => {
  it('has a translation for every code the API and the check can return', () => {
    const codes = [
      'transient',
      'throttled',
      'credentials_invalid',
      'not_found',
      'rejected',
      'version_unsupported',
      'unknown',
      'credentials_unreadable',
      'adapter_unknown',
      'services.name_taken',
      'services.not_found',
      'services.secrets_unavailable',
      'validation.adapter_type',
      'validation.service_name',
      'validation.base_url',
      'validation.api_key',
      'validation.credential_required_for_new_origin',
    ]
    const errors: Record<string, string> = admin.services.errors
    for (const code of codes) {
      const key = serviceErrorKey(code).replace('services.errors.', '')
      expect(errors[key], code).toBeTruthy()
    }
  })
})

describe('servicesIntro (MEM-22)', () => {
  it('names what the member has to do first, else what the cards wait for', () => {
    expect(servicesIntro([])).toBe('empty')
    expect(servicesIntro(['ready', 'ready'])).toBe('ready')
    expect(servicesIntro(['suspended', 'suspended'])).toBe('suspended')
    expect(servicesIntro(['ready', 'oldPassword', 'activate'])).toBe('activate')
    expect(servicesIntro(['ready', 'oldPassword', 'waiting'])).toBe('oldPassword')
    expect(servicesIntro(['ready', 'waiting'])).toBe('notYet')
    expect(servicesIntro(['undecided'])).toBe('notYet')
    expect(servicesIntro(['settingUp', 'ready'])).toBe('notYet')
  })
})

describe('safeHttpUrl', () => {
  it('links only web addresses', () => {
    expect(safeHttpUrl('https://kino.example.org')).toBe('https://kino.example.org/')
    expect(safeHttpUrl('http://192.168.1.4:8096')).toBe('http://192.168.1.4:8096/')
    expect(safeHttpUrl('javascript:alert(1)')).toBeNull()
    expect(safeHttpUrl('kino.local')).toBeNull()
  })
})

describe('sameOrigin (stored key never follows the URL to another server)', () => {
  it('compares scheme, host and port, not the path', () => {
    expect(sameOrigin('http://radarr:7878', 'http://radarr:7878/radarr')).toBe(true)
    expect(sameOrigin('http://radarr', 'http://radarr:80/')).toBe(true)
    expect(sameOrigin('HTTP://Radarr:7878', 'http://radarr:7878')).toBe(true)
    expect(sameOrigin('http://radarr:7878', 'http://radarr:7879')).toBe(false)
    expect(sameOrigin('http://radarr:7878', 'https://radarr:7878')).toBe(false)
    expect(sameOrigin('http://radarr:7878', 'http://evil:7878')).toBe(false)
  })

  it('treats an unparsable URL as another origin', () => {
    expect(sameOrigin('http://radarr:7878', 'radarr:7878')).toBe(false)
    expect(sameOrigin('', 'http://radarr')).toBe(false)
  })
})
