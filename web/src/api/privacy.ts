import { useMutation, useQueryClient } from '@tanstack/react-query'

import { type Me, ME_KEY } from '@/auth/session'
import { announceGateChange, dropPrivateState, expectUserClose } from '@/lib/privateGate'

import { api } from './client'
import { unwrap } from './errors'

/**
 * Unlock or lock the private category for this session (spec 0007 Q7). The session's gate is
 * updated at once (indicator, navigation), the other tabs are told, and everything is fetched
 * again: lists, counts and notifications depend on the gate. Locking forgets everything private
 * first (spec 0008 F2) and shows no notice: the user closed it.
 */
export function useSetPrivateUnlock() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (unlocked: boolean) =>
      unwrap(api.PUT('/api/v1/auth/private', { body: { unlocked } })),
    onMutate: (unlocked) => expectUserClose(!unlocked),
    onError: () => expectUserClose(false),
    onSuccess: (gate, unlocked) => {
      if (!unlocked) dropPrivateState(client)
      client.setQueryData<Me | null>(ME_KEY, (me) =>
        me?.privateCategory ? { ...me, privateCategory: gate } : me,
      )
      announceGateChange()
      return client.invalidateQueries()
    },
  })
}
