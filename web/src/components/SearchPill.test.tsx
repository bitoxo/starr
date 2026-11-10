import { act, fireEvent, screen } from '@testing-library/react'
import { useLocation } from 'react-router'
import { describe, expect, it } from 'vitest'

import { renderWithProviders } from '@/test/render'

import { SearchPill } from './SearchPill'

describe('SearchPill', () => {
  it('keeps focus in the search after clearing it (MEM-03)', () => {
    renderWithProviders(<SearchPill />, { path: '/search?q=dune' })
    const clear = screen.getByRole('button', { name: 'Clear search' })
    act(() => clear.focus())
    fireEvent.click(clear)
    expect(screen.queryByRole('button', { name: 'Clear search' })).toBeNull()
    expect(document.activeElement).toBe(screen.getByRole('searchbox'))
  })

  it('keeps the category filter of the results while the text changes (F6)', () => {
    renderWithProviders(
      <>
        <SearchPill />
        <Where />
      </>,
      { path: '/search?q=dune&category=movies' },
    )
    const field = screen.getByRole('searchbox')
    fireEvent.change(field, { target: { value: 'dune part two' } })
    fireEvent.submit(field.closest('form') as HTMLFormElement)
    expect(screen.getByTestId('where').textContent).toBe(
      '/search?q=dune%20part%20two&category=movies',
    )
  })
})

function Where() {
  const { pathname, search } = useLocation()
  return <output data-testid="where">{pathname + search}</output>
}
