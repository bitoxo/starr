import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter, useLocation, useNavigationType } from 'react-router'
import { describe, expect, it } from 'vitest'
import { z } from 'zod'

import { formatSort, parseSort, useUrlSort, useUrlState } from '@/lib/urlState'

const LIST = z.enum(['trending', 'popular', 'top_rated'])

function Lane() {
  const [list, setList] = useUrlState('lane-movies', LIST, 'trending')
  const [page, setPage] = useUrlState('page', z.coerce.number().int().min(1), 1)
  const location = useLocation()
  const type = useNavigationType()
  return (
    <>
      <p>list {list}</p>
      <p>page {page}</p>
      <p>search {location.search || '(none)'}</p>
      <p>type {type}</p>
      <button type="button" onClick={() => setList('popular')}>
        Popular
      </button>
      <button type="button" onClick={() => setList('trending')}>
        Trending
      </button>
      <button type="button" onClick={() => setPage(page + 1, { push: true })}>
        Next page
      </button>
    </>
  )
}

function at(url: string) {
  render(
    <MemoryRouter initialEntries={[url]}>
      <Lane />
    </MemoryRouter>,
  )
}

describe('useUrlState (F6)', () => {
  it('reads valid values from the URL', () => {
    at('/?lane-movies=top_rated&page=3')
    expect(screen.getByText('list top_rated')).toBeTruthy()
    expect(screen.getByText('page 3')).toBeTruthy()
  })

  it('falls back silently on missing or invalid values', () => {
    at('/?lane-movies=nonsense&page=-2')
    expect(screen.getByText('list trending')).toBeTruthy()
    expect(screen.getByText('page 1')).toBeTruthy()
  })

  it('writes filter changes with replace and keeps other parameters', () => {
    at('/?title=movie-27205')
    fireEvent.click(screen.getByRole('button', { name: 'Popular' }))
    expect(screen.getByText('list popular')).toBeTruthy()
    expect(screen.getByText('search ?title=movie-27205&lane-movies=popular')).toBeTruthy()
    expect(screen.getByText('type REPLACE')).toBeTruthy()
  })

  it('drops the parameter when it is back at its default', () => {
    at('/?lane-movies=popular')
    fireEvent.click(screen.getByRole('button', { name: 'Trending' }))
    expect(screen.getByText('search (none)')).toBeTruthy()
  })

  it('pushes when asked to (opening a page)', () => {
    at('/')
    fireEvent.click(screen.getByRole('button', { name: 'Next page' }))
    expect(screen.getByText('page 2')).toBeTruthy()
    expect(screen.getByText('type PUSH')).toBeTruthy()
  })
})

function Sorted() {
  const [sort, setSort] = useUrlSort({ column: 'created', direction: 'desc' })
  const location = useLocation()
  return (
    <>
      <p>sort {sort ? `${sort.column} ${sort.direction}` : 'none'}</p>
      <p>search {location.search || '(none)'}</p>
      <button type="button" onClick={() => setSort({ column: 'name', direction: 'asc' })}>
        By name
      </button>
      <button type="button" onClick={() => setSort({ column: 'created', direction: 'desc' })}>
        Newest
      </button>
    </>
  )
}

describe('useUrlSort (F6)', () => {
  it('keeps a table sort in one parameter, the default out of the URL', () => {
    render(
      <MemoryRouter initialEntries={['/?sort=-name']}>
        <Sorted />
      </MemoryRouter>,
    )
    expect(screen.getByText('sort name desc')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'By name' }))
    expect(screen.getByText('search ?sort=name')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Newest' }))
    expect(screen.getByText('search (none)')).toBeTruthy()
    expect(screen.getByText('sort created desc')).toBeTruthy()
  })

  it('round-trips through parse and format', () => {
    expect(parseSort('-createdAt')).toEqual({ column: 'createdAt', direction: 'desc' })
    expect(formatSort({ column: 'name', direction: 'asc' })).toBe('name')
    expect(parseSort('')).toBeNull()
  })
})
