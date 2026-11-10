import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { ConfirmDialog } from './ConfirmDialog'

function Remove({ onConfirm, phrase }: { onConfirm: () => void; phrase?: string }) {
  const [open, setOpen] = useState(false)
  return (
    <main>
      <h1>Services</h1>
      <button type="button" onClick={() => setOpen(true)}>
        Remove Kino
      </button>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title="Remove Kino?"
        description="Movies stop taking requests."
        confirmLabel="Remove"
        confirmPhrase={phrase}
        onConfirm={() => {
          onConfirm()
          setOpen(false)
        }}
      />
    </main>
  )
}

function openWith(phrase?: string) {
  const onConfirm = vi.fn()
  render(<Remove onConfirm={onConfirm} phrase={phrase} />)
  const opener = screen.getByRole('button', { name: 'Remove Kino' })
  act(() => opener.focus())
  fireEvent.click(opener)
  return { onConfirm, opener }
}

describe('ConfirmDialog (F8)', () => {
  it('names the action and its consequences, with Cancel focused', async () => {
    openWith()
    const dialog = await screen.findByRole('alertdialog', { name: 'Remove Kino?' })
    expect(dialog.textContent).toContain('Movies stop taking requests.')
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Cancel' }))
  })

  it('needs the name typed when something in use is affected', async () => {
    const { onConfirm } = openWith('Kino')
    const remove = await screen.findByRole('button', { name: 'Remove' })
    expect(remove).toHaveProperty('disabled', true)
    fireEvent.change(screen.getByRole('textbox', { name: 'Type Kino to confirm' }), {
      target: { value: 'Kin' },
    })
    expect(remove).toHaveProperty('disabled', true)
    fireEvent.change(screen.getByRole('textbox', { name: 'Type Kino to confirm' }), {
      target: { value: 'Kino' },
    })
    fireEvent.click(remove)
    expect(onConfirm).toHaveBeenCalledOnce()
  })

  it('starts empty again on the next opening', async () => {
    const { opener } = openWith('Kino')
    fireEvent.change(await screen.findByRole('textbox', { name: 'Type Kino to confirm' }), {
      target: { value: 'Kino' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    fireEvent.click(opener)
    expect(await screen.findByRole('textbox', { name: 'Type Kino to confirm' })).toHaveProperty(
      'value',
      '',
    )
  })

  it('closes on Esc and gives focus back to the opener', async () => {
    const { opener } = openWith()
    fireEvent.keyDown(await screen.findByRole('alertdialog'), { key: 'Escape' })
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(document.activeElement).toBe(opener)
  })
})
