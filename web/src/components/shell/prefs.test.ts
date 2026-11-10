import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  isShortcutTarget,
  readSidebarMode,
  setShortcutsEnabled,
  useShortcutsEnabled,
  writeSidebarMode,
} from './prefs'

afterEach(() => {
  localStorage.clear()
  vi.restoreAllMocks()
})

describe('sidebar mode (per browser, spec 0008 "Sidebar behaviour")', () => {
  it('is absent until chosen, then remembered', () => {
    expect(readSidebarMode()).toBeNull()
    writeSidebarMode('rail')
    expect(readSidebarMode()).toBe('rail')
    writeSidebarMode('expanded')
    expect(readSidebarMode()).toBe('expanded')
  })

  it('ignores garbage and a storage that throws', () => {
    localStorage.setItem('starr.sidebar', 'sideways')
    expect(readSidebarMode()).toBeNull()
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked')
    })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('blocked')
    })
    expect(readSidebarMode()).toBeNull()
    expect(() => writeSidebarMode('rail')).not.toThrow()
  })
})

describe('single-key shortcuts (WCAG 2.1.4, F-1)', () => {
  it('are on by default and can be switched off and on again', () => {
    const { result } = renderHook(() => useShortcutsEnabled())
    expect(result.current).toBe(true)
    act(() => setShortcutsEnabled(false))
    expect(result.current).toBe(false)
    expect(localStorage.getItem('starr.shortcuts')).toBe('off')
    act(() => setShortcutsEnabled(true))
    expect(result.current).toBe(true)
    expect(localStorage.getItem('starr.shortcuts')).toBeNull()
  })

  it('follow a change made in another tab', () => {
    const { result } = renderHook(() => useShortcutsEnabled())
    act(() => {
      localStorage.setItem('starr.shortcuts', 'off')
      window.dispatchEvent(new StorageEvent('storage', { key: 'starr.shortcuts' }))
    })
    expect(result.current).toBe(false)
  })

  it('never fire while typing or with a modifier', () => {
    const input = document.createElement('input')
    const textarea = document.createElement('textarea')
    const editable = document.createElement('div')
    editable.setAttribute('contenteditable', '')
    const button = document.createElement('button')
    const key = (target: EventTarget, init: KeyboardEventInit = {}) => {
      const event = new KeyboardEvent('keydown', { key: '[', ...init })
      Object.defineProperty(event, 'target', { value: target })
      return event
    }
    expect(isShortcutTarget(key(input))).toBe(false)
    expect(isShortcutTarget(key(textarea))).toBe(false)
    expect(isShortcutTarget(key(editable))).toBe(false)
    expect(isShortcutTarget(key(button, { ctrlKey: true }))).toBe(false)
    expect(isShortcutTarget(key(button, { metaKey: true }))).toBe(false)
    // Option+5 on a German Mac layout types `[`.
    expect(isShortcutTarget(key(button, { altKey: true }))).toBe(true)
    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    const inDialog = document.createElement('button')
    dialog.append(inDialog)
    expect(isShortcutTarget(key(inDialog))).toBe(false)
    const modal = document.createElement('div')
    modal.setAttribute('aria-modal', 'true')
    document.body.append(modal)
    expect(isShortcutTarget(key(button))).toBe(false)
    modal.remove()
    expect(isShortcutTarget(key(button))).toBe(true)
    expect(isShortcutTarget(key(document.body))).toBe(true)
  })
})
