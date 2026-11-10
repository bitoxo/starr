import type { CategoryName, CategorySettings, RoleKind } from '@/api/categories'
import type { MyServiceState } from '@/api/me'
import type { Service } from '@/api/services'
import type admin from '@/locales/en/admin.json'

type ServiceErrorKey = keyof (typeof admin)['services']['errors']

/** Error codes that mean "works, but look at it" rather than "broken". */
const WARNING_CODES = new Set(['version_unsupported', 'throttled'])

export type ServiceStatus = 'pending' | 'ok' | 'warning' | 'error'

/** Status of a service from its last check. */
export function serviceStatus(service: Pick<Service, 'lastCheckAt' | 'lastError'>): ServiceStatus {
  if (!service.lastCheckAt) return 'pending'
  if (!service.lastError) return 'ok'
  return WARNING_CODES.has(service.lastError) ? 'warning' : 'error'
}

/** Product name of a media server adapter type, for "Open in". */
export function serviceName(type: string): string {
  if (type === 'jellyfin') return 'Jellyfin'
  if (type === 'emby') return 'Emby'
  if (type === 'navidrome') return 'Navidrome'
  return type
}

/** Translation key for an API or check error code (dots become underscores). */
export function serviceErrorKey(code: string): `services.errors.${ServiceErrorKey}` {
  const key = code.replaceAll('.', '_') as ServiceErrorKey
  return `services.errors.${key}`
}

/**
 * The intro of My services (MEM-22): one sentence that fits every card below. What the member
 * has to do comes first (activate, switch an old password), then what is not ready yet.
 */
export type ServicesIntro = 'empty' | 'ready' | 'suspended' | 'activate' | 'oldPassword' | 'notYet'

export function servicesIntro(states: MyServiceState[]): ServicesIntro {
  if (states.length === 0) return 'empty'
  if (states.every((s) => s === 'suspended')) return 'suspended'
  if (states.includes('activate')) return 'activate'
  if (states.includes('oldPassword')) return 'oldPassword'
  if (states.every((s) => s === 'ready')) return 'ready'
  return 'notYet'
}

/**
 * An example address per service type, with its usual port (ADM-20); public services
 * (TMDB, MusicBrainz, StashDB) bring their own default from the API instead.
 */
const EXAMPLE_URL: Record<string, string> = {
  radarr: 'http://radarr:7878',
  sonarr: 'http://sonarr:8989',
  lidarr: 'http://lidarr:8686',
  whisparr: 'http://whisparr:6969',
  jellyfin: 'http://jellyfin:8096',
  emby: 'http://emby:8096',
  navidrome: 'http://navidrome:4533',
}

export function exampleUrl(typeId: string, defaultBaseUrl?: string | null): string {
  return defaultBaseUrl ?? EXAMPLE_URL[typeId] ?? 'http://service:port'
}

/** One role a service plays in a category (what removing it takes away, ADM-21). */
export type ServiceDependency = { category: CategoryName; role: RoleKind }

/** Every category role of `serviceId`, in category order. */
export function serviceDependencies(
  categories: CategorySettings[],
  serviceId: number,
): ServiceDependency[] {
  const out: ServiceDependency[] = []
  for (const c of categories) {
    if (c.catalogs.some((r) => r.serviceId === serviceId)) {
      out.push({ category: c.category, role: 'catalog' })
    }
    if (c.source?.serviceId === serviceId) out.push({ category: c.category, role: 'source' })
    if (c.consumers.some((r) => r.serviceId === serviceId)) {
      out.push({ category: c.category, role: 'consume' })
    }
  }
  return out
}

/** An admin-set address as a link target, only for `http(s)` (never `javascript:` and co). */
export function safeHttpUrl(raw: string): string | null {
  try {
    const url = new URL(raw)
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null
  } catch {
    return null
  }
}

/**
 * Whether two service URLs share scheme, host and port. A URL on another origin needs the API key
 * (or login) again, so a stored key never follows the address to another server.
 */
export function sameOrigin(a: string, b: string): boolean {
  try {
    return new URL(a.trim()).origin === new URL(b.trim()).origin
  } catch {
    return false
  }
}
