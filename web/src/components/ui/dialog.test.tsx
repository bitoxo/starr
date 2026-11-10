import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from './dialog'

function open(withForm: boolean) {
  const inner = (
    <>
      <DialogHeader>
        <DialogTitle>Edit Radarr</DialogTitle>
        <DialogDescription>Change the connection.</DialogDescription>
      </DialogHeader>
      <DialogBody>
        <label>
          Address <input />
        </label>
      </DialogBody>
      <DialogFooter>
        <button type="submit">Save</button>
      </DialogFooter>
    </>
  )
  render(
    <Dialog open>
      <DialogContent>{withForm ? <form>{inner}</form> : inner}</DialogContent>
    </Dialog>,
  )
  return screen.getByRole('dialog')
}

describe('DialogContent', () => {
  it('is bounded by the viewport with a margin, and scrolls itself as a last resort', () => {
    const dialog = open(false)
    expect(dialog.className).toContain('max-h-[calc(100dvh-48px)]')
    expect(dialog.className).toContain('overflow-y-auto')
    expect(dialog.className).toContain('flex-col')
  })

  it('keeps header and footer in place while only the body scrolls', () => {
    const dialog = open(false)
    const body = dialog.querySelector('[data-slot="dialog-body"]')
    expect(body?.className).toContain('overflow-y-auto')
    expect(body?.className).toContain('min-h-0')
    expect(dialog.querySelector('[data-slot="dialog-header"]')?.className).toContain('shrink-0')
    expect(dialog.querySelector('[data-slot="dialog-footer"]')?.className).toContain('shrink-0')
    expect(body?.contains(screen.getByRole('textbox'))).toBe(true)
    expect(body?.contains(screen.getByRole('button', { name: 'Save' }))).toBe(false)
  })

  it('passes the bounds through a form that wraps header, body and footer', () => {
    const dialog = open(true)
    expect(dialog.className).toContain('[&>form]:min-h-0')
    expect(dialog.className).toContain('[&>form]:flex-col')
  })
})
