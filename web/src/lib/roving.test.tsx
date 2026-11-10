import { act, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { useRovingFocus } from '@/lib/roving'
import { mockReducedMotion } from '@/test/motion'

function List({ count, grid = false }: { count: number; grid?: boolean }) {
  const roving = useRovingFocus<HTMLButtonElement>(count, { grid })
  return (
    <>
      <button type="button">before</button>
      <ul onKeyDown={roving.onKeyDown}>
        {Array.from({ length: count }, (_, i) => (
          <li key={i}>
            <button type="button" data-i={i} {...roving.itemProps(i)}>
              item {i}
            </button>
          </li>
        ))}
      </ul>
      <button type="button">after</button>
    </>
  )
}

const item = (i: number) => screen.getByRole('button', { name: `item ${i}` })
const stops = () => screen.getAllByRole('button').filter((b) => b.tabIndex === 0)
const press = (key: string) => fireEvent.keyDown(document.activeElement ?? document.body, { key })

afterEach(() => vi.unstubAllGlobals())

describe('roving tabindex (MEM-04)', () => {
  it('is one tab stop for the whole lane', () => {
    render(<List count={5} />)
    expect(stops().map((b) => b.textContent)).toEqual(['before', 'item 0', 'after'])
  })

  it('moves with the arrow keys, Home and End, and stops at the ends', () => {
    render(<List count={4} />)
    act(() => item(0).focus())
    press('ArrowRight')
    expect(document.activeElement).toBe(item(1))
    press('End')
    expect(document.activeElement).toBe(item(3))
    press('ArrowRight')
    expect(document.activeElement).toBe(item(3))
    press('Home')
    expect(document.activeElement).toBe(item(0))
    press('ArrowLeft')
    expect(document.activeElement).toBe(item(0))
    // Up and Down are not lane keys: the page may scroll.
    press('ArrowDown')
    expect(document.activeElement).toBe(item(0))
  })

  it('remembers the last item when Tab comes back', () => {
    render(<List count={4} />)
    act(() => item(0).focus())
    press('ArrowRight')
    press('ArrowRight')
    expect(stops().map((b) => b.textContent)).toEqual(['before', 'item 2', 'after'])
  })

  it('scrolls the focused item into view, smoothly only with motion allowed', () => {
    const scroll = vi.fn()
    Element.prototype.scrollIntoView = scroll
    mockReducedMotion(true)
    render(<List count={3} />)
    act(() => item(0).focus())
    press('ArrowRight')
    expect(scroll).toHaveBeenLastCalledWith({
      block: 'nearest',
      inline: 'nearest',
      behavior: 'auto',
    })
  })

  it('moves by rows in a grid', () => {
    // Three per row, as the browser would lay them out.
    vi.spyOn(HTMLElement.prototype, 'offsetTop', 'get').mockImplementation(function (
      this: HTMLElement,
    ) {
      return Math.floor(Number(this.dataset.i ?? 0) / 3) * 100
    })
    render(<List count={7} grid />)
    act(() => item(1).focus())
    press('ArrowDown')
    expect(document.activeElement).toBe(item(4))
    press('ArrowDown')
    expect(document.activeElement).toBe(item(4))
    press('ArrowUp')
    expect(document.activeElement).toBe(item(1))
    press('ArrowRight')
    expect(document.activeElement).toBe(item(2))
    vi.restoreAllMocks()
  })
})
