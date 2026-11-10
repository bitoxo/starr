import type { DiscographyAlbum, TitleCard } from '@/api/discover'

/** The groups an artist's discography is shown in, in this order. */
export const GROUPS = ['album', 'ep', 'single', 'other', 'secondary'] as const
export type DiscographyGroup = (typeof GROUPS)[number]

/** Live albums, compilations and the like go together, whatever their primary type. */
export function groupOf(album: DiscographyAlbum): DiscographyGroup {
  if (album.secondaryTypes.length > 0) return 'secondary'
  if (album.releaseType === 'album' || album.releaseType === 'ep') return album.releaseType
  if (album.releaseType === 'single') return 'single'
  return 'other'
}

/** The discography by group: only the category's release types unless `all`. */
export function discographyGroups(
  albums: DiscographyAlbum[],
  all: boolean,
): [DiscographyGroup, DiscographyAlbum[]][] {
  const shown = all ? albums : albums.filter((a) => a.inDefaultView)
  return GROUPS.map((group): [DiscographyGroup, DiscographyAlbum[]] => [
    group,
    shown.filter((a) => groupOf(a) === group),
  ]).filter(([, items]) => items.length > 0)
}

export type SearchGroup = 'video' | 'artist' | 'album'

/**
 * Search results by kind (spec 0006 Q11): movies and series together, then artists, then
 * albums. Empty groups are left out.
 */
export function searchGroups(cards: TitleCard[]): [SearchGroup, TitleCard[]][] {
  const groupOf = (card: TitleCard): SearchGroup =>
    card.kind === 'artist' || card.kind === 'album' ? card.kind : 'video'
  return (['video', 'artist', 'album'] as const)
    .map((g): [SearchGroup, TitleCard[]] => [g, cards.filter((c) => groupOf(c) === g)])
    .filter(([, items]) => items.length > 0)
}
