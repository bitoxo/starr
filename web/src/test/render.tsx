import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'

import { TooltipProvider } from '@/components/ui/tooltip'

/** Render `ui` with a fresh query client and a memory router; `me` is the signed-in account. */
export function renderWithProviders(
  ui: ReactNode,
  { path = '/', me, router = true }: { path?: string; me?: unknown; router?: boolean } = {},
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  if (me !== undefined) client.setQueryData(['auth', 'me'], me)
  // The app's root providers (main.tsx), so components can rely on them as in the app.
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        {router ? <MemoryRouter initialEntries={[path]}>{ui}</MemoryRouter> : ui}
      </TooltipProvider>
    </QueryClientProvider>,
  )
}

/** A member whose session has unlocked the private category. */
export const UNLOCKED_MEMBER = {
  kind: 'member',
  username: 'cara',
  displayName: 'Cara',
  privateCategory: { unlocked: true, unlockedUntil: '2026-11-06T21:14:00Z' },
}

/** The same member before unlocking. */
export const LOCKED_MEMBER = {
  ...UNLOCKED_MEMBER,
  privateCategory: { unlocked: false, unlockedUntil: null },
}

/** A journey without steps, for fixtures that do not look at it. */
export const NO_JOURNEY = { steps: [] }

/** An openapi-fetch style result. */
export function ok<T>(data: T, status = 200) {
  return { data, error: undefined, response: new Response(null, { status }) }
}

export function fail(code: string, status: number) {
  return { data: undefined, error: { code }, response: new Response(null, { status }) }
}
