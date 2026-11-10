import { act, renderHook } from '@testing-library/react'
import type { FocusEvent, PointerEvent } from 'react'
import { afterEach, describe, expect, it } from 'vitest'

import { useCarousel } from './carousel'

const pointer = (pointerType: string) => ({ pointerType }) as PointerEvent
/** A focus event from a button that has keyboard focus (`:focus-visible` in jsdom). */
function focusEvent() {
  const button = document.createElement('button')
  document.body.append(button)
  button.focus()
  return { target: button } as unknown as FocusEvent
}
const blur = (relatedTarget: Element | null, currentTarget: Element) =>
  ({ relatedTarget, currentTarget }) as unknown as FocusEvent

function setHidden(hidden: boolean) {
  Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden })
  document.dispatchEvent(new Event('visibilitychange'))
}

describe('useCarousel (spec 0008 A5)', () => {
  afterEach(() => setHidden(false))

  it('plays from the first slide and advances, wrapping after the last', () => {
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    expect(result.current.index).toBe(0)
    expect(result.current.playing).toBe(true)
    act(() => result.current.advance())
    act(() => result.current.advance())
    act(() => result.current.advance())
    expect(result.current.index).toBe(0)
    // Auto-advance stays quiet for screen readers.
    expect(result.current.live).toBe('off')
  })

  it('restarts the dwell on any move, wrapping backwards too', () => {
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    const before = result.current.cycle
    act(() => result.current.go(-1))
    expect(result.current.index).toBe(2)
    expect(result.current.cycle).not.toBe(before)
  })

  it('announces slides only while it does not move by itself', () => {
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    expect(result.current.live).toBe('off')
    act(() => result.current.regionProps.onFocus(focusEvent()))
    expect(result.current.live).toBe('polite')
  })

  it('pauses while hovered by a mouse, not on touch', () => {
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    act(() => result.current.regionProps.onPointerEnter(pointer('touch')))
    expect(result.current.playing).toBe(true)
    act(() => result.current.regionProps.onPointerEnter(pointer('mouse')))
    expect(result.current.playing).toBe(false)
    act(() => result.current.regionProps.onPointerLeave())
    expect(result.current.playing).toBe(true)
  })

  it('pauses while focus is inside and resumes once it leaves', () => {
    const region = document.createElement('div')
    const inside = document.createElement('button')
    region.append(inside)
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    act(() => result.current.regionProps.onFocus(focusEvent()))
    expect(result.current.playing).toBe(false)
    act(() => result.current.regionProps.onBlur(blur(inside, region)))
    expect(result.current.playing).toBe(false)
    act(() => result.current.regionProps.onBlur(blur(document.body, region)))
    expect(result.current.playing).toBe(true)
  })

  it('pauses while the tab is hidden', () => {
    const { result } = renderHook(() => useCarousel(3, { reduced: false }))
    act(() => setHidden(true))
    expect(result.current.playing).toBe(false)
    act(() => setHidden(false))
    expect(result.current.playing).toBe(true)
  })

  it('pauses and plays with the button, and while held (detail open)', () => {
    const { result, rerender } = renderHook(
      ({ held }) => useCarousel(3, { reduced: false, held }),
      { initialProps: { held: false } },
    )
    act(() => result.current.toggle())
    expect(result.current.userPaused).toBe(true)
    expect(result.current.playing).toBe(false)
    expect(result.current.live).toBe('polite')
    act(() => result.current.toggle())
    expect(result.current.playing).toBe(true)
    rerender({ held: true })
    expect(result.current.playing).toBe(false)
  })

  it('never plays under reduced motion or with a single slide', () => {
    expect(renderHook(() => useCarousel(3, { reduced: true })).result.current.playing).toBe(false)
    expect(renderHook(() => useCarousel(1, { reduced: false })).result.current.playing).toBe(false)
  })

  it('keeps the index inside the slides when there are fewer of them', () => {
    const { result, rerender } = renderHook(({ n }) => useCarousel(n, { reduced: false }), {
      initialProps: { n: 5 },
    })
    act(() => result.current.go(4))
    rerender({ n: 2 })
    expect(result.current.index).toBe(1)
  })
})
