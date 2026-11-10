import type { AccountPlan, AccountState, ForeignAccount, PlanStep } from '@/api/accounts'
import type { StatusTone } from '@/components/StatusChip'

export type AccountChip =
  'gone' | 'missing' | 'ignored' | 'admin' | 'locked' | 'lockedByStarr' | 'disabled' | 'enabled'

const TONE: Record<AccountChip, StatusTone> = {
  gone: 'bad',
  missing: 'warn',
  ignored: 'neutral',
  admin: 'warn',
  locked: 'bad',
  lockedByStarr: 'neutral',
  disabled: 'neutral',
  enabled: 'ok',
}

/**
 * The state chips of one account, most important first: presence (gone,
 * missing), the admin's decision (ignored), rights (admin), then the
 * server's own flag (locked, disabled or enabled). A server without an off
 * switch (Navidrome) reports every account as enabled; one Starr locked
 * with a random password shows that instead. One derivation for the whole
 * page (lesson 16).
 */
export function accountChips(
  account: Pick<
    ForeignAccount,
    | 'goneSince'
    | 'missingRuns'
    | 'ignoredAt'
    | 'privileged'
    | 'lockedByService'
    | 'lockedByStarr'
    | 'enabled'
  >,
): { chip: AccountChip; tone: StatusTone }[] {
  const chips: AccountChip[] = []
  if (account.goneSince) chips.push('gone')
  else if (account.missingRuns > 0) chips.push('missing')
  if (account.ignoredAt) chips.push('ignored')
  if (account.privileged) chips.push('admin')
  if (!account.goneSince) {
    if (account.lockedByService) chips.push('locked')
    else if (account.lockedByStarr) chips.push('lockedByStarr')
    else chips.push(account.enabled ? 'enabled' : 'disabled')
  }
  return chips.map((chip) => ({ chip, tone: TONE[chip] }))
}

const STATE_TONE: Record<AccountState, StatusTone> = {
  undecided: 'warn',
  noAccount: 'neutral',
  ignored: 'neutral',
  notNeeded: 'neutral',
  waitingForListing: 'info',
  waitingForLibraries: 'warn',
  toCreate: 'info',
  managed: 'ok',
  lockedByService: 'bad',
  nameTaken: 'bad',
  privileged: 'bad',
  missing: 'bad',
}

/** Tone of a planned account state: needs you (warn/bad), on its way (info), fine (ok). */
export function planStateTone(state: AccountState): StatusTone {
  return STATE_TONE[state]
}

/** Steps a live run executes now, in plan order (creates and passwords wait for the member's password). */
export function executableSteps(plan: AccountPlan): { username: string; step: PlanStep }[] {
  return plan.entries.flatMap((e) =>
    e.steps
      .filter((step) => step.action !== 'create' && step.action !== 'setPassword')
      .map((step) => ({ username: e.member.username, step })),
  )
}
