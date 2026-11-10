import type { AlbumDetails, MyRequest, TitleDetails, TitleRef } from '@/api/discover'
import { ApiError } from '@/api/errors'

/*
 * The logic behind the detail overlay (spec 0008 A7): where to watch, who made it, the
 * tracklist by disc, what the member can still withdraw, and how a missing title is told apart.
 */

type Provider = TitleDetails['watchProviders'][number]
type Crew = TitleDetails['crew'][number]
type Track = AlbumDetails['tracks'][number]

export type ProviderGroup = 'stream' | 'rent' | 'buy'

const GROUP_OF: Record<Provider['kind'], ProviderGroup> = {
  flatrate: 'stream',
  free: 'stream',
  ads: 'stream',
  rent: 'rent',
  buy: 'buy',
}

/** Offers as Stream (flatrate, free, with ads) · Rent · Buy, each named provider once per group. */
export function providerGroups(providers: Provider[]): [ProviderGroup, Provider[]][] {
  return (['stream', 'rent', 'buy'] as const)
    .map((group): [ProviderGroup, Provider[]] => {
      const seen = new Set<number>()
      return [
        group,
        providers.filter((p) => {
          if (GROUP_OF[p.kind] !== group || seen.has(p.id) || !p.name.trim()) return false
          seen.add(p.id)
          return true
        }),
      ]
    })
    .filter(([, list]) => list.length > 0)
}

/** TMDB's statuses of a series that gets no new seasons. */
export function seriesEnded(status: string | null | undefined): boolean {
  return status === 'Ended' || status === 'Canceled'
}

/** Directors or creators (shown above the cast), and writers (in Details). */
export function crewGroups(crew: Crew[]): { leads: Crew[]; writers: Crew[] } {
  return {
    leads: crew.filter((c) => c.role === 'director' || c.role === 'creator'),
    writers: crew.filter((c) => c.role === 'writer'),
  }
}

export type Disc = { disc: number; title: string; tracks: Track[]; seconds: number }

/** The tracklist by disc; the total running time only when every length is known. */
export function discs(tracks: Track[]): { discs: Disc[]; totalSeconds: number | null } {
  const byDisc = new Map<number, Disc>()
  for (const track of tracks) {
    const disc = byDisc.get(track.disc) ?? {
      disc: track.disc,
      title: track.discTitle,
      tracks: [],
      seconds: 0,
    }
    disc.tracks.push(track)
    disc.seconds += track.lengthSeconds ?? 0
    byDisc.set(track.disc, disc)
  }
  const known = tracks.length > 0 && tracks.every((t) => t.lengthSeconds != null)
  return {
    discs: [...byDisc.values()].sort((a, b) => a.disc - b.disc),
    totalSeconds: known ? tracks.reduce((sum, t) => sum + (t.lengthSeconds ?? 0), 0) : null,
  }
}

/** A request of the member that can still be withdrawn, with what it asks for. */
export type Withdrawable = { id: number; seasons: number[]; albums: string[] }

/**
 * The member's requests for `target` that can be withdrawn: everything in them still waits for
 * approval (D11), as on the Requests page. An album is requested through its artist.
 */
/** The member may still withdraw it: nothing of it is approved yet (MEM-05). */
export function waitingForApproval(request: MyRequest): boolean {
  return request.canWithdraw && request.items.every((i) => i.state === 'pending_approval')
}

export function withdrawables(requests: MyRequest[], target: TitleRef): Withdrawable[] {
  const matches = (r: MyRequest) =>
    target.kind === 'album'
      ? r.kind === 'artist' && r.items.some((i) => i.album?.id === target.id)
      : r.kind === target.kind && r.titleId === target.id
  return requests
    .filter((r) => matches(r) && waitingForApproval(r))
    .map((r) => ({
      id: r.id,
      seasons: r.items.flatMap((i) => (i.season != null ? [i.season] : [])),
      albums: r.items.flatMap((i) => (i.album ? [i.album.title] : [])),
    }))
}

/** The catalog does not know the title (404): say so at once (MEM-32). */
export function isNotFound(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404
}

/** TanStack Query `retry`: a 404 never gets better, anything else gets three more tries. */
export function retryUnlessNotFound(failures: number, error: unknown): boolean {
  return !isNotFound(error) && failures < 3
}
