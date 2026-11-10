import { render } from '@testing-library/react'
import { useRef } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { useScrollIntoDialogView } from './dialogView'

function Notice() {
  const ref = useRef<HTMLParagraphElement>(null)
  useScrollIntoDialogView(ref)
  return <p ref={ref}>Could not save.</p>
}

describe('useScrollIntoDialogView', () => {
  const scroll = vi.fn()
  afterEach(() => {
    scroll.mockReset()
    delete (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView
  })

  it('brings a notice inside a dialog body into view', () => {
    HTMLElement.prototype.scrollIntoView = scroll
    render(
      <div data-slot="dialog-body">
        <Notice />
      </div>,
    )
    expect(scroll).toHaveBeenCalledWith({ block: 'nearest' })
  })

  it('leaves the page alone outside a dialog', () => {
    HTMLElement.prototype.scrollIntoView = scroll
    render(<Notice />)
    expect(scroll).not.toHaveBeenCalled()
  })
})
