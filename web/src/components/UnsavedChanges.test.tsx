import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { createMemoryRouter, Link, RouterProvider } from 'react-router'
import { describe, expect, it } from 'vitest'

import { UnsavedChangesGuard } from './UnsavedChanges'

function Form() {
  const [name, setName] = useState('Kino')
  const dirty = name !== 'Kino'
  return (
    <main>
      <h1>Service</h1>
      <label>
        Name
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <button type="button" disabled={!dirty}>
        Save
      </button>
      <Link to="/elsewhere">Elsewhere</Link>
      <UnsavedChangesGuard when={dirty} />
    </main>
  )
}

function app() {
  const router = createMemoryRouter(
    [
      { path: '/', element: <Form /> },
      { path: '/elsewhere', element: <h1>Elsewhere page</h1> },
    ],
    { initialEntries: ['/'] },
  )
  render(<RouterProvider router={router} />)
  return router
}

describe('unsaved changes (F8)', () => {
  it('lets a clean form go without asking', async () => {
    app()
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', true)
    fireEvent.click(screen.getByRole('link', { name: 'Elsewhere' }))
    expect(await screen.findByRole('heading', { name: 'Elsewhere page' })).toBeTruthy()
  })

  it('asks before leaving a dirty form; Cancel stays', async () => {
    const router = app()
    fireEvent.change(screen.getByRole('textbox', { name: 'Name' }), { target: { value: 'Plex' } })
    fireEvent.click(screen.getByRole('link', { name: 'Elsewhere' }))
    const dialog = await screen.findByRole('alertdialog', { name: 'Leave without saving?' })
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(dialog.isConnected).toBe(false))
    expect(router.state.location.pathname).toBe('/')
    expect(screen.getByRole('textbox', { name: 'Name' })).toHaveProperty('value', 'Plex')
  })

  it('leaves when confirmed', async () => {
    app()
    fireEvent.change(screen.getByRole('textbox', { name: 'Name' }), { target: { value: 'Plex' } })
    fireEvent.click(screen.getByRole('link', { name: 'Elsewhere' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Leave' }))
    expect(await screen.findByRole('heading', { name: 'Elsewhere page' })).toBeTruthy()
  })

  it('asks the browser before the tab closes while dirty', () => {
    app()
    const clean = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(clean)
    expect(clean.defaultPrevented).toBe(false)
    fireEvent.change(screen.getByRole('textbox', { name: 'Name' }), { target: { value: 'Plex' } })
    const dirty = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(dirty)
    expect(dirty.defaultPrevented).toBe(true)
  })
})
