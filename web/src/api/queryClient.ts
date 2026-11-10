import { MutationCache, QueryClient } from '@tanstack/react-query'

import { ATTENTION_KEY } from '@/lib/attention'

/**
 * The app's single TanStack Query client (also used to reset state on sign-out). Every change an
 * admin makes may resolve something in the Inbox: its attention list is asked again after each
 * mutation, so the badge, heading and title drop live (spec 0008 F9). Members never hold it.
 */
export const queryClient: QueryClient = new QueryClient({
  mutationCache: new MutationCache({
    onSettled: () => queryClient.invalidateQueries({ queryKey: ATTENTION_KEY }),
  }),
})
