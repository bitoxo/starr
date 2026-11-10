import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from './client'
import { unwrap } from './errors'
import type { components } from './schema'

export type AccountInventory = components['schemas']['AccountInventory']
export type ForeignAccount = components['schemas']['ForeignAccount']
export type AccountMatrix = components['schemas']['AccountMatrix']
export type MatrixMember = components['schemas']['MatrixMember']
export type MatrixService = components['schemas']['MatrixService']
export type AccountCell = components['schemas']['AccountCell']
export type AccountState = components['schemas']['AccountState']
export type AccountDecision = components['schemas']['AccountDecision']
export type AccountPlan = components['schemas']['AccountPlan']
export type PlanStep = components['schemas']['PlanStep']
export type PlanEntry = components['schemas']['PlanEntryDto']
export type MemberAccountPlan = components['schemas']['MemberAccountPlan']
export type PresetAccount = components['schemas']['PresetAccountDto']

const accountsKey = (serviceId: number) => ['services', serviceId, 'accounts'] as const
const planKey = (serviceId: number) => ['services', serviceId, 'account-plan'] as const
const matrixKey = ['accounts', 'matrix'] as const
const memberPlanKey = (memberId: number) => ['members', memberId, 'account-plan'] as const
const attentionKey = ['accounts', 'attention'] as const

/** The accounts Starr found in one media server, with the last scan. */
export function useAccounts(serviceId: number | null) {
  return useQuery({
    queryKey: accountsKey(serviceId ?? 0),
    enabled: serviceId !== null,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/accounts', {
          params: { path: { id: serviceId ?? 0 } },
        }),
      ),
  })
}

/** Queue `accounts.scan` (all media servers with accounts); returns the job id. */
export function useScanAccounts() {
  return useMutation({
    mutationFn: async () => {
      const data = await unwrap(
        api.POST('/api/v1/jobs/{kind}/trigger', {
          params: { path: { kind: 'accounts.scan' } },
        }),
      )
      return data.jobId
    },
  })
}

/** Everything that shows decisions, ownership or plans. */
function invalidateAccounts(client: ReturnType<typeof useQueryClient>) {
  void client.invalidateQueries({ queryKey: matrixKey })
  void client.invalidateQueries({ queryKey: ['services'] })
  void client.invalidateQueries({ queryKey: ['members'] })
}

/** Mark an account ignored or not (Starr-side only, audited). */
export function useSetAccountIgnored(serviceId: number) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ remoteId, ignored }: { remoteId: string; ignored: boolean }) =>
      unwrap(
        api.PUT('/api/v1/services/{id}/accounts/{remoteId}/ignored', {
          params: { path: { id: serviceId, remoteId } },
          body: { ignored },
        }),
      ),
    onSuccess: () => invalidateAccounts(client),
  })
}

/** Members × media servers with decisions, suggestions and planned states. */
export function useAccountMatrix() {
  return useQuery({
    queryKey: matrixKey,
    queryFn: () => unwrap(api.GET('/api/v1/accounts/matrix')),
  })
}

/** The stored plan of one media server (plan only). */
export function useAccountPlan(serviceId: number | null) {
  return useQuery({
    queryKey: planKey(serviceId ?? 0),
    enabled: serviceId !== null,
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/services/{id}/account-plan', {
          params: { path: { id: serviceId ?? 0 } },
        }),
      ),
  })
}

/** One member's decisions and planned steps per media server. */
export function useMemberAccountPlan(memberId: number) {
  return useQuery({
    queryKey: memberPlanKey(memberId),
    queryFn: () =>
      unwrap(
        api.GET('/api/v1/members/{id}/account-plan', {
          params: { path: { id: memberId } },
        }),
      ),
  })
}

/** Decide for a member on a media server (Starr-side only, audited). */
export function useDecideAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      memberId,
      serviceId,
      decision,
      remoteId,
    }: {
      memberId: number
      serviceId: number
      decision: AccountDecision
      remoteId?: string
    }) =>
      unwrap(
        api.PUT('/api/v1/members/{id}/service-accounts/{serviceId}', {
          params: { path: { id: memberId, serviceId } },
          body: { decision, remoteId: remoteId ?? null },
        }),
      ),
    onSuccess: () => invalidateAccounts(client),
  })
}

/** Link every suggested account of a member's row. */
export function useConfirmSuggestions() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (memberId: number) =>
      unwrap(
        api.POST('/api/v1/members/{id}/service-accounts/confirm-suggestions', {
          params: { path: { id: memberId } },
        }),
      ),
    onSuccess: () => invalidateAccounts(client),
  })
}

export type AccountWrites = components['schemas']['AccountWrites']
export type AccountAttention = components['schemas']['AccountAttention']

/**
 * Confirm a media server's plan exactly as shown (its fingerprint): switch
 * account writes live, resume paused writes, or approve a held plan. Needs a
 * fresh elevation (wrap the call in `useElevation().run`).
 */
export function useConfirmAccountPlan(serviceId: number) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (fingerprint: string) =>
      unwrap(
        api.POST('/api/v1/services/{id}/account-plan/confirm', {
          params: { path: { id: serviceId } },
          body: { fingerprint },
        }),
      ),
    onSuccess: (plan) => {
      client.setQueryData(planKey(serviceId), plan)
      invalidateAccounts(client)
      void client.invalidateQueries({ queryKey: attentionKey })
    },
  })
}

/** Pause the account writes of a live media server. */
export function usePauseAccountWrites(serviceId: number) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST('/api/v1/services/{id}/account-writes/pause', {
          params: { path: { id: serviceId } },
        }),
      ),
    onSuccess: (plan) => {
      client.setQueryData(planKey(serviceId), plan)
      void client.invalidateQueries({ queryKey: attentionKey })
    },
  })
}
