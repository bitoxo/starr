import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { JourneyChip, JourneyStatusChip, type Journey, type UnitJourney } from './StatusJourney'
import { StatusChip } from './StatusChip'

const ARRIVING: Journey = {
  library: 'Jellyfin',
  source: null,
  steps: [
    { step: 'requested', status: 'done', at: '2026-11-02T15:12:00Z' },
    { step: 'approved', status: 'done', at: '2026-11-02T16:30:00Z' },
    { step: 'getting_it', status: 'done' },
    { step: 'arriving', status: 'current' },
    { step: 'ready', status: 'upcoming' },
  ],
}

function chip(units: UnitJourney[], admin = false) {
  return render(
    <JourneyChip title="Verity · Movie" units={units} admin={admin}>
      <StatusChip tone="info">Almost ready</StatusChip>
    </JourneyChip>,
  )
}

const trigger = () => screen.getByRole('button', { name: 'Almost ready' })

/** Tab onto the chip: a key press first, so the focus counts as keyboard focus. */
function keyboardFocus() {
  fireEvent.keyDown(document.body, { key: 'Tab' })
  act(() => trigger().focus())
}

describe('JourneyChip', () => {
  afterEach(() => vi.useRealTimers())

  it('opens on keyboard focus with the steps left to right and what happens now', () => {
    chip([{ journey: ARRIVING }])
    expect(screen.queryByRole('dialog')).toBeNull()
    keyboardFocus()
    const card = screen.getByRole('dialog', { name: 'Status of Verity · Movie' })
    const steps = within(card)
      .getAllByRole('listitem')
      .map((li) => li.textContent ?? '')
    expect(steps.map((s) => s.split(' (')[0])).toEqual([
      expect.stringContaining('Requested'),
      expect.stringContaining('Approved'),
      expect.stringContaining('Downloaded'),
      expect.stringContaining('Arriving in Jellyfin'),
      expect.stringContaining('Ready'),
    ])
    expect(within(card).getAllByRole('listitem')[3]?.getAttribute('aria-current')).toBe('step')
    expect(within(card).getByText('now')).toBeTruthy()
    expect(
      within(card).getByText("It's downloaded. Jellyfin usually picks it up within minutes."),
    ).toBeTruthy()
    // Focus stays on the chip; Escape closes.
    expect(document.activeElement).toBe(trigger())
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' })
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('moves into the card on Enter after keyboard focus and hands focus back on Escape', async () => {
    const onRowClick = vi.fn()
    render(
      <div onClick={onRowClick}>
        <JourneyChip title="Verity · Movie" units={[{ journey: ARRIVING }]}>
          <StatusChip tone="info">Almost ready</StatusChip>
        </JourneyChip>
      </div>,
    )
    keyboardFocus()
    expect(screen.getByRole('dialog')).toBeTruthy()
    // Enter (a click without pointer) keeps it open and moves focus inside.
    fireEvent.click(trigger(), { detail: 0 })
    const card = screen.getByRole('dialog')
    await waitFor(() => expect(card.contains(document.activeElement)).toBe(true))
    // Clicks in the card stay out of the surrounding row.
    fireEvent.click(card)
    expect(onRowClick).not.toHaveBeenCalled()
    fireEvent.keyDown(document.activeElement ?? card, { key: 'Escape' })
    expect(screen.queryByRole('dialog')).toBeNull()
    await waitFor(() => expect(document.activeElement).toBe(trigger()))
    // Getting focus back does not reopen the card.
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(trigger().getAttribute('aria-expanded')).toBe('false')
  })

  it('opens on a tap (no hover on touch) and closes on the next', () => {
    chip([{ journey: ARRIVING }])
    fireEvent.pointerDown(trigger(), { pointerType: 'touch' })
    fireEvent.click(trigger(), { detail: 1 })
    expect(screen.getByRole('dialog')).toBeTruthy()
    expect(trigger().getAttribute('aria-expanded')).toBe('true')
    fireEvent.pointerDown(trigger(), { pointerType: 'touch' })
    fireEvent.click(trigger(), { detail: 1 })
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('opens on hover only after a short delay', () => {
    vi.useFakeTimers()
    chip([{ journey: ARRIVING }])
    fireEvent.pointerEnter(trigger(), { pointerType: 'mouse' })
    expect(screen.queryByRole('dialog')).toBeNull()
    act(() => vi.advanceTimersByTime(400))
    expect(screen.getByRole('dialog')).toBeTruthy()
    fireEvent.pointerLeave(trigger(), { pointerType: 'mouse' })
    act(() => vi.advanceTimersByTime(400))
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('stays a plain chip without a journey', () => {
    chip([])
    expect(screen.queryByRole('button')).toBeNull()
    expect(screen.getByText('Almost ready')).toBeTruthy()
  })

  it('shows a declined request ending with the reason, and the admin who declined only to admins', () => {
    const declined: Journey = {
      steps: [
        { step: 'requested', status: 'done', at: '2026-11-02T15:12:00Z' },
        { step: 'declined', status: 'failed', detail: 'Not in 4K', actor: 'root' },
      ],
    }
    chip([{ journey: declined }], true)
    keyboardFocus()
    const card = screen.getByRole('dialog')
    expect(within(card).getByText('Reason: Not in 4K')).toBeTruthy()
    expect(within(card).getByText('by root')).toBeTruthy()
  })

  it('names technical steps for admins: held, the source and the media server', () => {
    const held: Journey = {
      source: 'Radarr',
      library: 'Jellyfin',
      steps: [
        { step: 'requested', status: 'done' },
        { step: 'approved', status: 'done', actor: 'root' },
        { step: 'held', status: 'warning' },
        { step: 'getting_it', status: 'upcoming' },
        { step: 'arriving', status: 'upcoming' },
        { step: 'ready', status: 'upcoming' },
      ],
    }
    chip([{ journey: held }], true)
    keyboardFocus()
    const card = screen.getByRole('dialog')
    expect(within(card).getByText('Held – Radarr paused')).toBeTruthy()
    expect(within(card).getByText('In Radarr, waiting for Jellyfin')).toBeTruthy()
    expect(
      within(card).getByText('Writes to Radarr are paused. It goes out once they are resumed.'),
    ).toBeTruthy()
  })

  it('tells members a stuck import without the source message', () => {
    const stuck: Journey = {
      steps: [
        { step: 'requested', status: 'done' },
        { step: 'approved', status: 'done' },
        { step: 'importing', status: 'warning', note: 'stuck_at_import' },
        { step: 'arriving', status: 'upcoming' },
        { step: 'ready', status: 'upcoming' },
      ],
    }
    chip([{ journey: stuck }])
    keyboardFocus()
    expect(screen.getByText('Stuck at import.')).toBeTruthy()
  })

  it('heads each album with its title and says music is ready to listen to', () => {
    const ready: Journey = {
      steps: [
        { step: 'requested', status: 'done' },
        { step: 'getting_it', status: 'done' },
        { step: 'ready', status: 'done' },
      ],
    }
    render(
      <JourneyChip
        title="Radiohead · Artist"
        units={[
          { album: 'ok', journey: ready },
          { album: { id: 'pool' }, journey: ARRIVING },
        ]}
        albums={{ ok: 'OK Computer', pool: 'A Moon Shaped Pool' }}
      >
        <StatusChip tone="ok">Partly available</StatusChip>
      </JourneyChip>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Partly available' }))
    const card = screen.getByRole('dialog', { name: 'Status of Radiohead · Artist' })
    expect(within(card).getByRole('heading', { name: 'OK Computer' })).toBeTruthy()
    expect(within(card).getByRole('heading', { name: 'A Moon Shaped Pool' })).toBeTruthy()
    expect(within(card).getByText('Ready to listen to.')).toBeTruthy()
    expect(within(card).queryByText(/Ready to watch/)).toBeNull()
  })

  it('names the music library and opens each available album there', () => {
    const ready: Journey = {
      library: 'Navidrome',
      steps: [
        { step: 'requested', status: 'done' },
        { step: 'getting_it', status: 'done' },
        { step: 'ready', status: 'done' },
      ],
    }
    const link = (id: string) => ({
      service: 'navidrome',
      name: 'Navidrome',
      url: `https://music.example.com/app/#/album/${id}/show`,
    })
    render(
      <JourneyChip
        title="Radiohead · Artist"
        units={[
          { album: { id: 'ok' }, journey: ready, openIn: [link('ok')] },
          { album: { id: 'pool' }, journey: { ...ARRIVING, library: 'Navidrome' }, openIn: [] },
        ]}
        albums={{ ok: 'OK Computer', pool: 'A Moon Shaped Pool' }}
      >
        <StatusChip tone="ok">Partly available</StatusChip>
      </JourneyChip>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Partly available' }))
    const card = screen.getByRole('dialog', { name: 'Status of Radiohead · Artist' })
    expect(within(card).getByText('Ready to listen to in Navidrome.')).toBeTruthy()
    const links = within(card).getAllByRole('link', { name: 'Open in Navidrome' })
    expect(links.map((l) => l.getAttribute('href'))).toEqual([link('ok').url])
    // The arriving album says where it will show up.
    expect(within(card).getByText(/Navidrome usually picks it up/)).toBeTruthy()
  })

  it('shows one row per season of a series', () => {
    const waiting: Journey = {
      steps: [
        { step: 'requested', status: 'done' },
        { step: 'awaiting_approval', status: 'current', note: 'can_withdraw' },
        { step: 'getting_it', status: 'upcoming' },
        { step: 'arriving', status: 'upcoming' },
        { step: 'ready', status: 'upcoming' },
      ],
    }
    chip([
      { season: 1, journey: ARRIVING },
      { season: 2, journey: waiting },
    ])
    keyboardFocus()
    const card = screen.getByRole('dialog')
    expect(within(card).getByRole('heading', { name: 'Season 1' })).toBeTruthy()
    expect(within(card).getByRole('heading', { name: 'Season 2' })).toBeTruthy()
    expect(within(card).getByText("You can withdraw until it's approved.")).toBeTruthy()
  })
})

describe('journey endings (spec 0004)', () => {
  it('ends a revoked item in red with the reason', () => {
    chip([
      {
        journey: {
          steps: [
            { step: 'requested', status: 'done' },
            { step: 'approved', status: 'done' },
            { step: 'revoked', status: 'failed', detail: 'Not on this server' },
          ],
        },
      },
    ])
    keyboardFocus()
    const card = screen.getByRole('dialog')
    expect(within(card).getByText('An admin revoked this request.')).toBeTruthy()
    expect(within(card).getByText('Reason: Not on this server')).toBeTruthy()
    expect(within(card).queryByText('Something went wrong on the way.')).toBeNull()
  })

  it('ends a removed item quietly, saying it can be requested again', () => {
    chip([
      {
        journey: {
          steps: [
            { step: 'requested', status: 'done' },
            { step: 'approved', status: 'done' },
            { step: 'ready', status: 'done' },
            { step: 'removed', status: 'ended', note: 'request_again' },
          ],
        },
      },
    ])
    keyboardFocus()
    const card = screen.getByRole('dialog')
    expect(within(card).getByText(/No longer in the library/, { selector: 'span' })).toBeTruthy()
    expect(
      within(card).getByText('No longer in the library — you can request it again.'),
    ).toBeTruthy()
  })

  it('ends a cancelled item muted', () => {
    render(
      <JourneyStatusChip
        journey={{
          steps: [
            { step: 'requested', status: 'done' },
            { step: 'approved', status: 'done' },
            { step: 'cancelled', status: 'ended' },
          ],
        }}
      />,
    )
    expect(screen.getByText('Withdrawn')).toBeTruthy()
  })
})

describe('JourneyStatusChip', () => {
  it('names where the journey stands', () => {
    render(<JourneyStatusChip journey={ARRIVING} />)
    expect(screen.getByText('Arriving in Jellyfin')).toBeTruthy()
  })
})
