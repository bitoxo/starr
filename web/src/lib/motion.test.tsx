import { act, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { coverLift, prefersReducedMotion, scrollBehavior, useReducedMotion } from '@/lib/motion'
import { mockReducedMotion } from '@/test/motion'

function Probe() {
  return <p>{useReducedMotion() ? 'reduced' : 'full'}</p>
}

afterEach(() => vi.unstubAllGlobals())

describe('useReducedMotion', () => {
  it('is off without matchMedia (old engines, jsdom)', () => {
    vi.stubGlobal('matchMedia', undefined)
    expect(prefersReducedMotion()).toBe(false)
    render(<Probe />)
    expect(screen.getByText('full')).toBeTruthy()
  })

  it('follows the system setting, also when it changes', () => {
    const setting = mockReducedMotion(true)
    render(<Probe />)
    expect(screen.getByText('reduced')).toBeTruthy()
    expect(scrollBehavior()).toBe('auto')
    act(() => setting.set(false))
    expect(screen.getByText('full')).toBeTruthy()
    expect(scrollBehavior()).toBe('smooth')
  })
})

describe('cover lift (MEM-15)', () => {
  it.each(['group', 'peer'] as const)(
    'moves and scales only with motion allowed (%s)',
    (trigger) => {
      const classes = coverLift(trigger).split(' ')
      const moving = classes.filter((c) => /translate|scale|transition|duration/.test(c))
      expect(moving.length).toBeGreaterThan(4)
      for (const c of moving) expect(c.startsWith('motion-safe:'), c).toBe(true)
    },
  )
})
