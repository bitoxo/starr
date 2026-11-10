import { type PointerEvent, useCallback, useEffect, useRef, useState } from 'react'

import type { TitleCard } from '@/api/discover'
import { matchesMedia, NO_HOVER } from '@/lib/media'

/*
 * When a lane's preview card shows (spec 0008 A3, amendment 2026-11-07): after a 320 ms hover
 * with a mouse or pen, or on keyboard focus of a cover; never on touch or on a device that cannot
 * hover. Browsing is quick: within 400 ms of a card closing (or while one is open) the next cover
 * gets its card after 90 ms, and a keyboard move with a card open replaces it at once. It closes
 * when the pointer leaves the cover and the card (after a short grace), when focus leaves the
 * covers, on Esc, and at once on a scroll that moves the cover.
 */

/** Hover dwell before the card shows (A3). */
export const PREVIEW_DELAY = 320
/** Dwell while browsing: a card is open or closed less than `BROWSE_WINDOW` ago. */
export const BROWSE_DELAY = 90
export const BROWSE_WINDOW = 400
/** Grace after the pointer leaves, so it can cross from the cover onto the card. */
export const LEAVE_GRACE = 80
/**
 * The lane (and the page, bringing the info strip into view) scrolls itself to follow keyboard
 * focus; for this long after a key or such a scroll the card follows the cover instead of
 * closing.
 */
const FOLLOW_SCROLL = 700

// Browsing crosses lanes: the moment the last card (of any lane) closed.
let lastClose = Number.NEGATIVE_INFINITY

/** Tests only: forget the last close, so no test inherits another's browsing window. */
export function resetPreviewBrowsing() {
  lastClose = Number.NEGATIVE_INFINITY
}

export interface PreviewAnchor {
  /** Changes with every opening, so a reopened card starts afresh. */
  id: number
  card: TitleCard
  /** The cover's tile; its button gets focus when the card opens the detail view. */
  el: HTMLElement
  /** Opened by keyboard focus: the card's poster carries the focus ring. */
  keyboard: boolean
}

let nextId = 0

/** Hover and focus handling for the covers of one lane. */
export function usePreview() {
  const [anchor, setAnchor] = useState<PreviewAnchor | null>(null)
  const [closing, setClosing] = useState(false)
  // Counts the cover's moves under an open card (a scroll following focus).
  const [moved, setMoved] = useState(0)
  // The same, readable in handlers without waiting for a render.
  const current = useRef<{ anchor: PreviewAnchor | null; closing: boolean }>({
    anchor: null,
    closing: false,
  })
  const openTimer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const closeTimer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const keyAt = useRef(Number.NEGATIVE_INFINITY)
  const cardRef = useRef<HTMLDivElement>(null)

  const show = useCallback((next: PreviewAnchor | null, isClosing = false) => {
    current.current = { anchor: next, closing: isClosing }
    setAnchor(next)
    setClosing(isClosing)
  }, [])

  const clearTimers = () => {
    clearTimeout(openTimer.current)
    clearTimeout(closeTimer.current)
  }

  /** Close the card: animated, or `instant` (a scroll, opening the detail view). */
  const close = useCallback(
    (instant = false) => {
      clearTimeout(openTimer.current)
      clearTimeout(closeTimer.current)
      const { anchor: open, closing: already } = current.current
      if (!open) return
      if (!already) lastClose = performance.now()
      if (instant) show(null)
      else if (!already) show(open, true)
    },
    [show],
  )

  /** The close animation of card `id` ended (a card that replaced it stays). */
  const closed = useCallback(
    (id: number) => {
      if (current.current.anchor?.id === id) show(null)
    },
    [show],
  )

  useEffect(
    () => () => {
      clearTimeout(openTimer.current)
      clearTimeout(closeTimer.current)
    },
    [],
  )

  useEffect(() => {
    if (!anchor) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close()
    }
    // A scroll that moves the cover (the page's or its lane's) leaves the card behind; others
    // (the hero's swipe row rotating) do not. The lane or page following keyboard focus moves
    // the card along instead (once per frame).
    let frame = 0
    const onScroll = (event: Event) => {
      const target = event.target
      if (target !== document && !(target instanceof Node && target.contains(anchor.el))) return
      if (performance.now() - keyAt.current < FOLLOW_SCROLL) {
        if (frame === 0) {
          frame = requestAnimationFrame(() => {
            frame = 0
            setMoved((n) => n + 1)
          })
        }
        return
      }
      close(true)
    }
    document.addEventListener('scroll', onScroll, { capture: true, passive: true })
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('scroll', onScroll, { capture: true })
      document.removeEventListener('keydown', onKey)
      cancelAnimationFrame(frame)
    }
  }, [anchor, close])

  const schedule = (el: HTMLElement, card: TitleCard, keyboard: boolean) => {
    clearTimers()
    const { anchor: open, closing: isClosing } = current.current
    if (open?.el === el && !isClosing) return
    const visible = open !== null && !isClosing
    const browsing = visible || performance.now() - lastClose < BROWSE_WINDOW
    const delay = keyboard && visible ? 0 : browsing ? BROWSE_DELAY : PREVIEW_DELAY
    const open_ = () => show({ id: (nextId += 1), card, el, keyboard })
    if (delay === 0) open_()
    else openTimer.current = setTimeout(open_, delay)
  }

  /** Pointer over a cover tile: show its card after the dwell (mouse and pen only). */
  const enter = (event: PointerEvent<HTMLElement>, card: TitleCard) => {
    if (event.pointerType !== 'mouse' && event.pointerType !== 'pen') return
    if (matchesMedia(NO_HOVER)) return
    const el = event.currentTarget
    const { anchor: open, closing: isClosing } = current.current
    if (open?.el === el && !isClosing) {
      clearTimeout(closeTimer.current)
      return
    }
    schedule(el, card, false)
  }

  /** Keyboard focus on a cover tile: its card follows the focus (never on touch layouts). */
  const focus = (el: HTMLElement, card: TitleCard) => {
    if (matchesMedia(NO_HOVER)) return
    schedule(el, card, true)
  }

  /** Pointer left a cover tile or the card: close, unless it moved between the two. */
  const leave = (event: PointerEvent<HTMLElement>) => {
    clearTimeout(openTimer.current)
    const to = event.relatedTarget
    const open = current.current.anchor
    if (!open) return
    if (to instanceof Node && (cardRef.current?.contains(to) || open.el.contains(to))) return
    clearTimeout(closeTimer.current)
    closeTimer.current = setTimeout(() => close(), LEAVE_GRACE)
  }

  /** Pointer is on the card: keep it. */
  const hold = () => clearTimeout(closeTimer.current)

  /** A key moved focus in the lane, or the lane scrolls its strip into view: the scroll that
   * follows is the lane's own. */
  const keyed = useCallback(() => {
    keyAt.current = performance.now()
  }, [])

  return { anchor, closing, moved, enter, focus, leave, hold, close, closed, keyed, cardRef }
}
