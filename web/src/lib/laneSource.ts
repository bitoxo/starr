import { isMusic } from '@/api/categories'
import { type TitleCard, uniqueCards, type useDiscoverList } from '@/api/discover'

/**
 * What a lane shows and how far its loading got: a catalog list page by page
 * (`listSource`), or a fixed set of cards (`fixedSource`).
 */
export interface LaneSource {
  cards: TitleCard[]
  /** Pages loaded so far (re-arms the endless loading). */
  pages: number
  isPending: boolean
  isFetching: boolean
  /** The first load failed: nothing to show. */
  isError: boolean
  refetch: () => void
  hasNextPage: boolean
  isFetchingNextPage: boolean
  isFetchNextPageError: boolean
  fetchNextPage: () => void
}

/** A catalog list as a lane source, each title once. */
export function listSource(query: ReturnType<typeof useDiscoverList>): LaneSource {
  const pages = query.data?.pages ?? []
  return {
    cards: uniqueCards(pages),
    pages: pages.length,
    isPending: query.isPending,
    isFetching: query.isFetching,
    isError: query.isError && !query.data,
    refetch: () => void query.refetch(),
    hasNextPage: query.hasNextPage,
    isFetchingNextPage: query.isFetchingNextPage,
    isFetchNextPageError: query.isFetchNextPageError,
    fetchNextPage: () => void query.fetchNextPage(),
  }
}

/** A fixed set of cards (a slice of a list, a discography) as a lane source. */
export function fixedSource(
  cards: TitleCard[],
  state: { isPending: boolean; isError: boolean; isFetching?: boolean; refetch: () => void },
): LaneSource {
  return {
    cards,
    pages: 1,
    isPending: state.isPending,
    isFetching: state.isFetching ?? false,
    isError: state.isError,
    refetch: state.refetch,
    hasNextPage: false,
    isFetchingNextPage: false,
    isFetchNextPageError: false,
    fetchNextPage: () => {},
  }
}

/** A dated card's width in its lane: 16:9 for movies and series, a square cover for albums. */
export function datedWidth(card: Pick<TitleCard, 'kind'>): string {
  return isMusic(card.kind) ? 'w-32 sm:w-36 lg:w-44' : 'w-64 sm:w-72'
}
