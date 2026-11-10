import { useSyncExternalStore } from 'react'

/*
 * Per-browser shell preferences (spec 0008 "Sidebar behaviour", F-1). They live in
 * localStorage only, never on the server; every access is guarded because storage can be
 * missing or throw (private windows, blocked site data). Absent means the default.
 */

export type SidebarMode = 'expanded' | 'rail'

const SIDEBAR_KEY = 'starr.sidebar'
const SHORTCUTS_KEY = 'starr.shortcuts'

function read(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function write(key: string, value: string | null) {
  try {
    if (value === null) localStorage.removeItem(key)
    else localStorage.setItem(key, value)
  } catch {
    // Not remembered then; the choice still holds for this page.
  }
}

/** The sidebar mode chosen at ≥ 1280 px, or `null` for the default (expanded). */
export function readSidebarMode(): SidebarMode | null {
  const value = read(SIDEBAR_KEY)
  return value === 'expanded' || value === 'rail' ? value : null
}

export function writeSidebarMode(mode: SidebarMode) {
  write(SIDEBAR_KEY, mode)
}

const listeners = new Set<() => void>()

function subscribe(listener: () => void) {
  listeners.add(listener)
  const onStorage = (event: StorageEvent) => {
    if (event.key === SHORTCUTS_KEY || event.key === null) listener()
  }
  window.addEventListener('storage', onStorage)
  return () => {
    listeners.delete(listener)
    window.removeEventListener('storage', onStorage)
  }
}

const shortcutsEnabled = () => read(SHORTCUTS_KEY) !== 'off'

/** Single-key shortcuts (`[`, `/`): on unless switched off in this browser (WCAG 2.1.4). */
export function useShortcutsEnabled(): boolean {
  return useSyncExternalStore(subscribe, shortcutsEnabled, () => true)
}

export function setShortcutsEnabled(enabled: boolean) {
  write(SHORTCUTS_KEY, enabled ? null : 'off')
  for (const listener of listeners) listener()
}

/**
 * Whether a key press may act as a single-key shortcut: not while typing (text fields, selects,
 * editable content), not inside a dialog or menu (nor while a modal is open), and not with
 * Ctrl or Cmd, so browser and screen-reader keys stay untouched. Option (macOS) and AltGr stay
 * allowed: on German layouts `[` is Option+5 or AltGr+8.
 */
export function isShortcutTarget(event: KeyboardEvent): boolean {
  if (event.defaultPrevented || event.metaKey) return false
  const altGraph =
    typeof event.getModifierState === 'function' && event.getModifierState('AltGraph')
  if (event.ctrlKey && !altGraph) return false
  if (document.querySelector('[aria-modal="true"]')) return false
  const target = event.target
  if (!(target instanceof HTMLElement)) return true
  if (target.isContentEditable) return false
  return !target.closest(
    'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]',
  )
}
