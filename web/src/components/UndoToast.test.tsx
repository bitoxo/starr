import { act, fireEvent, render, screen } from '@testing-library/react'
import { toast } from 'sonner'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { UndoToast } from './UndoToast'

describe('UndoToast (F8)', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.spyOn(toast, 'dismiss')
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  const show = (onUndo = vi.fn()) => {
    render(<UndoToast toastId="t1" message="Unfollowed Dark" onUndo={onUndo} />)
    return onUndo
  }

  it('offers undo for 8 seconds', () => {
    show()
    act(() => vi.advanceTimersByTime(7900))
    expect(toast.dismiss).not.toHaveBeenCalled()
    act(() => vi.advanceTimersByTime(100))
    expect(toast.dismiss).toHaveBeenCalledWith('t1')
  })

  it('undoes and closes', () => {
    const onUndo = show()
    fireEvent.click(screen.getByRole('button', { name: 'Undo' }))
    expect(onUndo).toHaveBeenCalledOnce()
    expect(toast.dismiss).toHaveBeenCalledWith('t1')
  })

  it('pauses while hovered and continues with the time left', () => {
    show()
    act(() => vi.advanceTimersByTime(5000))
    fireEvent.pointerEnter(screen.getByRole('status'))
    act(() => vi.advanceTimersByTime(30_000))
    expect(toast.dismiss).not.toHaveBeenCalled()
    fireEvent.pointerLeave(screen.getByRole('status'))
    act(() => vi.advanceTimersByTime(2900))
    expect(toast.dismiss).not.toHaveBeenCalled()
    act(() => vi.advanceTimersByTime(100))
    expect(toast.dismiss).toHaveBeenCalledWith('t1')
  })

  it('stays while focus is inside', () => {
    show()
    act(() => screen.getByRole('button', { name: 'Undo' }).focus())
    act(() => vi.advanceTimersByTime(60_000))
    expect(toast.dismiss).not.toHaveBeenCalled()
    act(() => screen.getByRole('button', { name: 'Undo' }).blur())
    act(() => vi.advanceTimersByTime(8000))
    expect(toast.dismiss).toHaveBeenCalledWith('t1')
  })
})
