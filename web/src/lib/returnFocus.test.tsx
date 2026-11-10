import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'

import { Dialog, DialogContent, DialogTitle } from '@/components/ui/dialog'
import { focusAfterRemoval, useFocusNextOnRemoval, useReturnFocus } from '@/lib/returnFocus'
import { expectFocusNotOnBody } from '@/test/focus'

/** A dialog opened from code (no Radix trigger), like most of Starr's. */
function Page({ removeOpener = false }: { removeOpener?: boolean }) {
  const [open, setOpen] = useState(false)
  return (
    <main>
      <h1>Members</h1>
      {!(removeOpener && open) && (
        <button type="button" onClick={() => setOpen(true)}>
          Open
        </button>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent aria-describedby={undefined}>
          <DialogTitle>Link</DialogTitle>
          <button type="button" onClick={() => setOpen(false)}>
            Done
          </button>
        </DialogContent>
      </Dialog>
    </main>
  )
}

describe('dialogs give focus back (MEM-03)', () => {
  it('to the opener after Esc', async () => {
    render(<Page />)
    const opener = screen.getByRole('button', { name: 'Open' })
    act(() => opener.focus())
    fireEvent.click(opener)
    const dialog = await screen.findByRole('dialog')
    fireEvent.keyDown(dialog, { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(opener)
  })

  it('to the opener after a button in the dialog closes it', async () => {
    render(<Page />)
    const opener = screen.getByRole('button', { name: 'Open' })
    act(() => opener.focus())
    fireEvent.click(opener)
    fireEvent.click(await screen.findByRole('button', { name: 'Done' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(opener)
  })

  it('to the page heading when the opener is gone', async () => {
    render(<Page removeOpener />)
    const opener = screen.getByRole('button', { name: 'Open' })
    act(() => opener.focus())
    fireEvent.click(opener)
    fireEvent.keyDown(await screen.findByRole('dialog'), { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Members' }))
    expectFocusNotOnBody()
  })
})

describe('Esc closes every dialog (ADM-37)', () => {
  // A dialog that swallows Esc (the one-time link dialogs did) is the exception the audit
  // found; none may prevent it. Popovers may still look at the key without preventing it.
  const sources = import.meta.glob<string>(['/src/**/*.tsx', '!/src/**/*.test.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
  })

  it('no layer prevents Esc', () => {
    const blocking = Object.entries(sources)
      .filter(([, source]) => /onEscapeKeyDown=\{[^}]*preventDefault/s.test(source))
      .map(([path]) => path)
    expect(blocking).toEqual([])
  })
})

describe('useReturnFocus outside Radix', () => {
  function Layer() {
    const [open, setOpen] = useState(false)
    const focus = useReturnFocus()
    return (
      <main>
        <h1>Search</h1>
        <button
          type="button"
          onClick={() => {
            focus.capture()
            setOpen(true)
          }}
        >
          Filters
        </button>
        {open && (
          <button
            type="button"
            onClick={() => {
              setOpen(false)
              focus.restore()
            }}
          >
            Close filters
          </button>
        )}
      </main>
    )
  }

  it('restores to what had focus on capture', () => {
    render(<Layer />)
    const opener = screen.getByRole('button', { name: 'Filters' })
    act(() => opener.focus())
    fireEvent.click(opener)
    const close = screen.getByRole('button', { name: 'Close filters' })
    act(() => close.focus())
    fireEvent.click(close)
    expect(document.activeElement).toBe(opener)
  })
})

describe('useFocusNextOnRemoval', () => {
  function Row({ name }: { name: string }) {
    const { ref, arm } = useFocusNextOnRemoval<HTMLLIElement>()
    return (
      <li ref={ref}>
        <button type="button" onClick={arm}>
          Hide {name}
        </button>
      </li>
    )
  }
  function List({ rows }: { rows: string[] }) {
    return (
      <main>
        <h1>Requests</h1>
        <ul>
          {rows.map((r) => (
            <Row key={r} name={r} />
          ))}
        </ul>
        <button type="button">Show hidden</button>
      </main>
    )
  }

  it('moves focus to the next row when the armed row goes', () => {
    const { rerender } = render(<List rows={['a', 'b']} />)
    const hide = screen.getByRole('button', { name: 'Hide a' })
    act(() => hide.focus())
    fireEvent.click(hide)
    rerender(<List rows={['b']} />)
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Hide b' }))
  })

  it('leaves focus alone once the user moved on', () => {
    const { rerender } = render(<List rows={['a', 'b']} />)
    fireEvent.click(screen.getByRole('button', { name: 'Hide a' }))
    const elsewhere = screen.getByRole('button', { name: 'Show hidden' })
    act(() => elsewhere.focus())
    rerender(<List rows={['b']} />)
    expect(document.activeElement).toBe(elsewhere)
  })
})

describe('focusAfterRemoval', () => {
  function rows() {
    render(
      <main>
        <h2 tabIndex={-1}>Requests</h2>
        <button type="button">Row 0</button>
        <button type="button">Row 1</button>
        <button type="button">Row 2</button>
      </main>,
    )
    return screen.getAllByRole('button')
  }

  it('moves to the next row, else the previous, else the heading', () => {
    const items = rows()
    focusAfterRemoval(items, 1, screen.getByRole('heading'))
    expect(document.activeElement).toBe(items[2])
    focusAfterRemoval(items, 2, screen.getByRole('heading'))
    expect(document.activeElement).toBe(items[1])
    focusAfterRemoval([items[0]!], 0, screen.getByRole('heading'))
    expect(document.activeElement).toBe(screen.getByRole('heading'))
  })
})
