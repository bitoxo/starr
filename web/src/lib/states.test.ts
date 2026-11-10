import i18n from 'i18next'
import { describe, expect, it } from 'vitest'

import {
  acquisitionState,
  cardState,
  journeyState,
  notificationState,
  requestState,
  type Journey,
  type JourneyStep,
  STATES,
  stateLabel,
  unitsState,
} from '@/lib/states'

function journey(steps: Partial<JourneyStep>[], names: Partial<Journey> = {}): Journey {
  return {
    ...names,
    steps: steps.map((s) => ({ step: 'requested', status: 'done', ...s }) as JourneyStep),
  }
}

const downloading = journey([
  { step: 'requested', status: 'done' },
  { step: 'approved', status: 'done' },
  { step: 'downloading', status: 'current', progress: 0.35 },
  { step: 'ready', status: 'upcoming' },
])

describe('the state of a journey is its current step (MEM-01)', () => {
  it('downloading carries its percentage', () => {
    const view = journeyState(downloading)
    expect(view).toEqual({ key: 'downloading', percent: 0.35 })
    expect(stateLabel(view!)).toBe('Downloading 35%')
  })

  it('a warning at import is "Stuck at import", never "Downloading 100%"', () => {
    const stuck = journey([
      { step: 'downloading', status: 'done', progress: 1 },
      { step: 'importing', status: 'warning' },
    ])
    expect(stateLabel(journeyState(stuck)!)).toBe('Stuck at import')
    expect(STATES.stuckAtImport.tone).toBe('warn')
  })

  it('another warning needs attention', () => {
    const warn = journey([{ step: 'downloading', status: 'warning', progress: 0.4 }])
    expect(stateLabel(journeyState(warn)!)).toBe('Needs attention')
  })

  it('arriving names the media server', () => {
    const arriving = journey([{ step: 'arriving', status: 'current' }], { library: 'Jellyfin' })
    expect(stateLabel(journeyState(arriving)!)).toBe('Arriving in Jellyfin')
  })

  it('held is "Held – <source> paused" for admins and "Approved" for members', () => {
    const held = journey([{ step: 'held', status: 'current' }], { source: 'Radarr' })
    expect(stateLabel(journeyState(held, { admin: true })!)).toBe('Held – Radarr paused')
    expect(stateLabel(journeyState(held)!)).toBe('Approved')
  })

  it('declined, withdrawn and revoked use one word each (MEM-02)', () => {
    const end = (step: JourneyStep['step'], status: JourneyStep['status']) =>
      stateLabel(journeyState(journey([{ step, status }]))!)
    expect(end('declined', 'failed')).toBe('Declined')
    expect(end('withdrawn', 'ended')).toBe('Withdrawn')
    expect(end('cancelled', 'ended')).toBe('Withdrawn')
    expect(end('revoked', 'failed')).toBe('Revoked')
    expect(end('removed', 'ended')).toBe('No longer in the library')
    expect(end('searching', 'failed')).toBe('Failed')
  })

  it('ready is available, partly ready is partly available', () => {
    expect(journeyState(journey([{ step: 'ready', status: 'done' }]))?.key).toBe('available')
    expect(journeyState(journey([{ step: 'ready', status: 'current' }]))?.key).toBe(
      'partlyAvailable',
    )
  })

  it('a journey without steps has no state', () => {
    expect(journeyState({ steps: [] })).toBeNull()
  })
})

describe('several units aggregate to one state', () => {
  const unit = (j: Journey, season: number) => ({ season, journey: j })
  const ready = journey([{ step: 'ready', status: 'done' }])
  const waiting = journey([{ step: 'awaiting_approval', status: 'current' }])
  const declined = journey([{ step: 'declined', status: 'failed' }])

  it('something going on wins over what is done', () => {
    expect(unitsState([unit(ready, 1), unit(downloading, 2)])?.key).toBe('downloading')
  })

  it('the earliest step in progress wins', () => {
    expect(unitsState([unit(downloading, 1), unit(waiting, 2)])?.key).toBe('awaitingApproval')
  })

  it('all ready is available; some ready is partly available, counted', () => {
    expect(unitsState([unit(ready, 1), unit(ready, 2)])?.key).toBe('available')
    const partly = unitsState([unit(ready, 1), unit(declined, 2)])
    expect(stateLabel(partly!)).toBe('1 of 2 seasons')
    const albums = unitsState([
      { album: 'a', journey: ready },
      { album: 'b', journey: ready },
      { album: 'c', journey: declined },
    ])
    expect(stateLabel(albums!)).toBe('2 of 3 albums')
  })

  it('nothing is null', () => {
    expect(unitsState([])).toBeNull()
  })
})

describe('cards', () => {
  it('a request by someone else is not the member’s own (MEM-24)', () => {
    const view = cardState({ status: 'requested', requestedByMe: false, journeys: [] })
    expect(stateLabel(view!)).toBe('Requested by someone')
    expect(STATES.requestedByOthers.tone).toBe('neutral')
  })

  it('the member’s own request shows its current step, not "Requested" (MEM-01)', () => {
    const view = cardState({
      status: 'requested',
      requestedByMe: true,
      journeys: [{ journey: downloading }],
    })
    expect(stateLabel(view!)).toBe('Downloading 35%')
  })

  it('new titles have no chip', () => {
    expect(cardState({ status: 'new', requestedByMe: false, journeys: [] })).toBeNull()
  })

  it('a finished journey does not hide partial availability', () => {
    const view = cardState({
      status: 'partially_available',
      requestedByMe: true,
      journeys: [{ journey: journey([{ step: 'ready', status: 'done' }]) }],
    })
    expect(view?.key).toBe('partlyAvailable')
  })

  it('partly available can count its units', () => {
    expect(stateLabel({ key: 'partlyAvailable', have: 1, total: 2, unit: 'seasons' })).toBe(
      '1 of 2 seasons',
    )
    expect(stateLabel({ key: 'partlyAvailable', have: 8, total: 12, unit: 'tracks' })).toBe(
      '8 of 12 tracks',
    )
    expect(STATES.partlyAvailable.outline).toBe(true)
  })
})

describe('request and acquisition states map onto the same table (MEM-02)', () => {
  it('requests', () => {
    expect(stateLabel(requestState('pending_approval'))).toBe('Waiting for approval')
    expect(stateLabel(requestState('rejected'))).toBe('Declined')
    expect(stateLabel(requestState('cancelled'))).toBe('Withdrawn')
    expect(stateLabel(requestState('submitted'))).toBe('Searching')
    expect(stateLabel(requestState('in_progress', 'importing'))).toBe('Importing')
    expect(stateLabel(requestState('in_progress', 'arriving', 'Plex'))).toBe('Arriving in Plex')
    expect(STATES[requestState('rejected').key].tone).toBe('bad')
  })

  it('acquisitions', () => {
    expect(stateLabel(acquisitionState('pending', { held: true, source: 'Sonarr' }))).toBe(
      'Held – Sonarr paused',
    )
    expect(stateLabel(acquisitionState('submitted'))).toBe('Searching')
    expect(stateLabel(acquisitionState('cancelled'))).toBe('Withdrawn')
    // Not sent to the source yet: admins see it waiting, not "Approved" (slice 12).
    expect(stateLabel(acquisitionState('pending'))).toBe('Queued')
    expect(STATES[acquisitionState('pending').key].tone).toBe('info')
  })

  it('notifications take their tone from the table', () => {
    expect(notificationState('request_rejected')).toBe('declined')
    expect(notificationState('request_available')).toBe('available')
    expect(notificationState('account_locked')).toBeNull()
  })

  it('every state has a label, a tone and an icon', () => {
    for (const [key, entry] of Object.entries(STATES)) {
      expect(i18n.exists(`common:state.${key}`), key).toBe(true)
      expect(entry.icon).toBeTruthy()
    }
  })
})
