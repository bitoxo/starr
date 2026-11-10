import { fireEvent, render, screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { type Column, DataTable } from './DataTable'

type Member = { id: number; name: string; requests: number }

const ROWS: Member[] = [
  { id: 1, name: 'Ben', requests: 12 },
  { id: 2, name: 'anna', requests: 3 },
  { id: 3, name: 'Cara', requests: 7 },
]

const COLUMNS: Column<Member>[] = [
  { id: 'name', header: 'Name', cell: (m) => m.name, sortValue: (m) => m.name },
  {
    id: 'requests',
    header: 'Requests',
    cell: (m) => m.requests,
    sortValue: (m) => m.requests,
    align: 'end',
  },
  { id: 'actions', header: 'Actions', cell: (m) => <button type="button">Open {m.name}</button> },
]

function table(props: Partial<Parameters<typeof DataTable<Member>>[0]> = {}) {
  return render(
    <DataTable<Member>
      label="Members"
      columns={COLUMNS}
      rows={ROWS}
      rowKey={(m) => m.id}
      {...props}
    />,
  )
}

const names = () =>
  screen
    .getAllByRole('row')
    .slice(1)
    .map((r) => within(r).getAllByRole('cell')[0]?.textContent)

describe('DataTable (F5)', () => {
  it('is a labelled table with a count', () => {
    table()
    expect(screen.getByRole('table', { name: 'Members' })).toBeTruthy()
    expect(screen.getByText('3 entries')).toBeTruthy()
  })

  it('sorts by a header with aria-sort, in the UI language’s order', () => {
    table()
    const name = screen.getByRole('columnheader', { name: 'Name' })
    expect(name.getAttribute('aria-sort')).toBe('none')
    fireEvent.click(within(name).getByRole('button', { name: 'Name' }))
    expect(name.getAttribute('aria-sort')).toBe('ascending')
    expect(names()).toEqual(['anna', 'Ben', 'Cara'])
    fireEvent.click(within(name).getByRole('button', { name: 'Name' }))
    expect(name.getAttribute('aria-sort')).toBe('descending')
    expect(names()).toEqual(['Cara', 'Ben', 'anna'])
    // Columns without a sort value have no sort control.
    const actions = screen.getByRole('columnheader', { name: 'Actions' })
    expect(within(actions).queryByRole('button')).toBeNull()
  })

  it('sorts numbers by value', () => {
    table({ sort: { column: 'requests', direction: 'desc' } })
    expect(names()).toEqual(['Ben', 'Cara', 'anna'])
  })

  it('keeps missing values last in both directions', () => {
    const rows = [...ROWS, { id: 4, name: '', requests: 1 }]
    const { rerender } = table({ rows, sort: { column: 'name', direction: 'desc' } })
    expect(names()).toEqual(['Cara', 'Ben', 'anna', ''])
    rerender(
      <DataTable<Member>
        label="Members"
        columns={COLUMNS}
        rows={rows}
        rowKey={(m) => m.id}
        sort={{ column: 'name', direction: 'asc' }}
      />,
    )
    expect(names()).toEqual(['anna', 'Ben', 'Cara', ''])
  })

  it('reports sort changes when controlled (URL state)', () => {
    const onSortChange = vi.fn()
    table({ sort: null, onSortChange })
    fireEvent.click(screen.getByRole('button', { name: 'Requests' }))
    expect(onSortChange).toHaveBeenCalledWith({ column: 'requests', direction: 'asc' })
  })

  it('filters with the search and counts what is shown', () => {
    const onChange = vi.fn()
    table({
      search: {
        label: 'Search members',
        value: 'a',
        onChange,
        matches: (m, q) => m.name.toLowerCase().includes(q.toLowerCase()),
      },
    })
    expect(names()).toEqual(['anna', 'Cara'])
    expect(screen.getByText('2 of 3 entries')).toBeTruthy()
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search members' }), {
      target: { value: 'be' },
    })
    expect(onChange).toHaveBeenCalledWith('be')
  })

  it('labels every cell for the stacked card layout on phones', () => {
    table()
    const first = screen.getAllByRole('row')[1] as HTMLElement
    const cells = within(first).getAllByRole('cell')
    expect(cells.map((c) => c.getAttribute('data-label'))).toEqual(['Name', 'Requests', 'Actions'])
    expect(screen.getByRole('table').className).toContain('max-md:block')
  })

  it('shows skeleton rows while loading', () => {
    table({ rows: undefined, loading: true })
    expect(screen.getByRole('table').getAttribute('aria-busy')).toBe('true')
    expect(document.querySelectorAll('[data-skeleton]').length).toBeGreaterThan(0)
    expect(screen.queryByText(/entries/)).toBeNull()
  })

  it('shows an error with retry instead of rows', () => {
    const onRetry = vi.fn()
    table({ rows: undefined, error: 'The members could not be loaded.', onRetry })
    expect(screen.getByRole('alert').textContent).toContain('The members could not be loaded.')
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(onRetry).toHaveBeenCalled()
    expect(screen.queryByRole('table')).toBeNull()
  })

  it('says when nothing matches', () => {
    table({ rows: [], empty: 'No members yet.' })
    expect(screen.getByText('No members yet.')).toBeTruthy()
  })

  it('selects rows for a sticky bar whose actions get only the chosen, shown rows (ADM-31)', () => {
    const onChange = vi.fn()
    const bar = vi.fn((chosen: Member[]) => <button type="button">Act on {chosen.length}</button>)
    const selection = {
      selected: new Set<string | number>([1, 3]),
      onChange,
      isSelectable: (m: Member) => m.id !== 2,
      rowLabel: (m: Member) => `Select ${m.name}`,
      allLabel: 'Select all',
      bar,
    }
    const chosen = (call: number) => Array.from((onChange.mock.calls[call] ?? [new Set()])[0])
    const { rerender } = table({ selection })
    expect(screen.queryByRole('checkbox', { name: 'Select anna' })).toBeNull()
    const region = screen.getByRole('region', { name: '2 selected' })
    expect(region.className).toContain('sticky')
    expect(within(region).getByRole('button', { name: 'Act on 2' })).toBeTruthy()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select Ben' }))
    expect(chosen(0)).toEqual([3])
    // A search hides Cara: the bar acts on Ben only.
    rerender(
      <DataTable<Member>
        label="Members"
        columns={COLUMNS}
        rows={ROWS}
        rowKey={(m) => m.id}
        selection={selection}
        search={{
          label: 'Search',
          value: 'Ben',
          onChange: () => {},
          matches: (m, q) => m.name.includes(q),
        }}
      />,
    )
    expect(screen.getByRole('button', { name: 'Act on 1' })).toBeTruthy()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all' }))
    expect(chosen(1)).toEqual([])
  })

  it('gives selection its own narrow column, apart from the first column’s header and cells', () => {
    const selection = {
      selected: new Set<string | number>(),
      onChange: () => {},
      isSelectable: (m: Member) => m.id !== 2,
      rowLabel: (m: Member) => `Select ${m.name}`,
      allLabel: 'Select all',
      bar: () => null,
    }
    const { rerender } = table({ selection })
    const headers = screen.getAllByRole('columnheader')
    expect(headers).toHaveLength(COLUMNS.length + 1)
    expect(within(headers[0]!).getByRole('checkbox', { name: 'Select all' })).toBeTruthy()
    expect(headers[0]!.textContent).toBe('')
    expect(within(headers[1]!).queryByRole('checkbox')).toBeNull()
    expect(within(headers[1]!).getByRole('button', { name: 'Name' })).toBeTruthy()
    for (const row of screen.getAllByRole('row').slice(1)) {
      const cells = within(row).getAllByRole('cell')
      expect(cells).toHaveLength(COLUMNS.length + 1)
      expect(cells[0]!.textContent).toBe('')
      expect(within(cells[1]!).queryByRole('checkbox')).toBeNull()
    }
    const ben = screen.getByRole('checkbox', { name: 'Select Ben' })
    expect(ben.closest('td')?.nextElementSibling?.textContent).toBe('Ben')
    // Loading rows keep the same columns.
    rerender(
      <DataTable<Member>
        label="Members"
        columns={COLUMNS}
        rows={undefined}
        loading
        rowKey={(m) => m.id}
        selection={selection}
      />,
    )
    for (const row of screen.getAllByRole('row').slice(1)) {
      expect(within(row).getAllByRole('cell')).toHaveLength(COLUMNS.length + 1)
    }
  })

  it('draws row dividers on the cells, so a sticky first cell never hides them', () => {
    table()
    const tableEl = screen.getByRole('table', { name: 'Members' })
    expect(tableEl.className).toContain('border-separate')
    expect(tableEl.className).not.toContain('border-collapse')
    const body = screen.getAllByRole('rowgroup')[1]!
    expect(body.className).toContain('[&>tr+tr>td]:border-t')
    expect(body.className).not.toContain('divide-y')
  })
})
