import type { CategoryName } from '@/api/categories'
import type { components } from '@/api/schema'

type LibraryContent = components['schemas']['LibraryContent']

/**
 * Whether a media server library's content type fits a category (ADM-19): a books or music
 * library under Movies is almost certainly a mistake. "Other" (mixed or unknown) fits anything;
 * private content usually sits in a movies or mixed library.
 */
export function libraryFits(category: CategoryName, content: LibraryContent): boolean {
  if (content === 'other') return true
  switch (category) {
    case 'movies':
      return content === 'movies'
    case 'series':
      return content === 'series'
    case 'music':
      return content === 'music'
    case 'adult':
      return content === 'movies' || content === 'series'
  }
}
