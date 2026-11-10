import { screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { AccountCell, PlanStep } from '@/api/accounts'
import { renderWithProviders } from '@/test/render'

import { AccountNameNotes, StepList } from './AccountPlan'

function step(partial: Partial<PlanStep>): PlanStep {
  return {
    action: 'enable',
    reason: 'entitled',
    destructive: false,
    drift: false,
    username: null,
    add: [],
    remove: [],
    fromAll: false,
    ...partial,
  }
}

function cell(partial: Partial<AccountCell>): AccountCell {
  return {
    serviceId: 1,
    decision: 'linked',
    account: { remoteId: 'r1', username: 'alex' },
    origin: 'adopted',
    password: 'deferred',
    linkedUsername: 'alex',
    missingSince: null,
    decidedAt: null,
    decidedBy: null,
    suggestion: null,
    state: 'managed',
    conflictAccount: null,
    steps: [],
    lockedByStarr: false,
    ...partial,
  }
}

describe('account plan', () => {
  it('names deletions of due deletion orders and changes made in the server', () => {
    renderWithProviders(
      <StepList
        steps={[
          step({ action: 'delete', reason: 'deletionDue', destructive: true }),
          step({ action: 'enable', reason: 'entitled', drift: true }),
        ]}
      />,
    )
    expect(screen.getByText('Delete the account')).toBeTruthy()
    expect(screen.getByText(/the member's deletion grace period is over/)).toBeTruthy()
    expect(screen.getByText(/takes access away/)).toBeTruthy()
    expect(screen.getByText("changed in the server since Starr's last write")).toBeTruthy()
  })

  it('shows an account renamed in the server and one no longer listed', () => {
    renderWithProviders(
      <AccountNameNotes
        cell={cell({
          account: { remoteId: 'r1', username: 'Alexander' },
          missingSince: '2026-11-01T10:00:00Z',
        })}
        username="alex"
        className="text-xs"
      />,
    )
    expect(screen.getByText('renamed to “Alexander” there')).toBeTruthy()
    expect(screen.getByText(/not listed since/)).toBeTruthy()
  })

  it('says nothing when the name there is the member’s', () => {
    const { container } = renderWithProviders(
      <AccountNameNotes cell={cell({})} username="alex" className="text-xs" />,
    )
    expect(container.textContent).toBe('')
  })
})
