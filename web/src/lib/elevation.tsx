import { useState, type ReactNode } from 'react'

import { useQueryClient } from '@tanstack/react-query'

import { errorCode } from '@/api/errors'
import { ME_KEY, useSession } from '@/auth/session'
import { ElevationDialog } from '@/components/ElevationDialog'

/** The admin closed the re-authentication dialog: the action did not run. */
export class ElevationCancelled extends Error {
  constructor() {
    super('elevation cancelled')
  }
}

interface Pending {
  retry: () => Promise<void>
  cancel: () => void
}

/**
 * Whether the session is elevated for at least `marginMs` more (`elevatedUntil` from
 * `/auth/me`); the margin leaves time to use the dialog and absorbs clock drift.
 */
export function isElevated(
  elevatedUntil: string | null | undefined,
  marginMs = 0,
  now = Date.now(),
): boolean {
  return elevatedUntil != null && Date.parse(elevatedUntil) - now > marginMs
}

/** Time left that lets an action's dialog open without asking first (ADM-34). */
const ENSURE_MARGIN_MS = 60_000

/**
 * Re-authenticate before opening a dialog whose action needs it (spec 0008 ADM-34), so the
 * re-authentication never stacks over the action's own dialog. `ensure(open)` opens at once
 * while the session is elevated, else after the admin confirmed. Render `dialog` once.
 */
export function useEnsureElevated(): {
  ensure: (open: () => void) => void
  elevatedUntil: string | null
  dialog: ReactNode
} {
  const client = useQueryClient()
  const elevatedUntil = useSession().me?.elevatedUntil ?? null
  const [then, setThen] = useState<(() => void) | null>(null)
  const ensure = (open: () => void) => {
    if (isElevated(elevatedUntil, ENSURE_MARGIN_MS)) open()
    else setThen(() => open)
  }
  const dialog = (
    <ElevationDialog
      open={then !== null}
      onCancel={() => setThen(null)}
      onElevated={async () => {
        await client.invalidateQueries({ queryKey: ME_KEY })
        const open = then
        setThen(null)
        open?.()
      }}
    />
  )
  return { ensure, elevatedUntil, dialog }
}

/**
 * Actions that need a fresh re-authentication (spec 0005 M10 2: adopting an
 * account, switching account writes live, resuming, approving). `run(action)`
 * calls the action; when the server answers `auth.elevation_required`, it asks
 * for the admin's password (and code with TOTP), then runs the action again.
 * Render `dialog` once in the component. Cancelling rejects with
 * {@link ElevationCancelled}.
 */
export function useElevation(): {
  run: <T>(action: () => Promise<T>) => Promise<T>
  dialog: ReactNode
} {
  const [pending, setPending] = useState<Pending | null>(null)
  const run = async <T,>(action: () => Promise<T>): Promise<T> => {
    try {
      return await action()
    } catch (error) {
      if (errorCode(error) !== 'auth.elevation_required') throw error
      return new Promise<T>((resolve, reject) => {
        setPending({
          retry: async () => {
            setPending(null)
            try {
              resolve(await action())
            } catch (again) {
              reject(again)
            }
          },
          cancel: () => {
            setPending(null)
            reject(new ElevationCancelled())
          },
        })
      })
    }
  }
  const dialog = (
    <ElevationDialog
      open={pending !== null}
      onCancel={() => pending?.cancel()}
      onElevated={() => pending?.retry()}
    />
  )
  return { run, dialog }
}
