import { useTranslation } from 'react-i18next'

import type { components } from './schema'

/** A failed API call with the machine-readable code from the server (ADR 0007). */
export class ApiError extends Error {
  readonly code: string
  readonly status: number
  constructor(code: string, status: number) {
    super(code)
    this.code = code
    this.status = status
  }
}

export type Effect = components['schemas']['Effect']

/**
 * The action has consequences the admin must agree to first (409 `*.confirm_required`, spec
 * 0008 F8): nothing changed; send it again with `confirm` after naming the `effects`.
 */
export class ConfirmRequired extends ApiError {
  readonly effects: Effect[]
  constructor(code: string, effects: Effect[]) {
    super(code, 409)
    this.effects = effects
  }
}

interface FetchResult<T> {
  data?: T
  error?: unknown
  response: Response
}

function codeOf(error: unknown): string {
  if (error && typeof error === 'object' && 'code' in error && typeof error.code === 'string') {
    return error.code
  }
  return 'unavailable'
}

function effectsOf(error: unknown): Effect[] | null {
  if (error && typeof error === 'object' && 'effects' in error && Array.isArray(error.effects)) {
    return error.effects as Effect[]
  }
  return null
}

/** The data of a successful openapi-fetch call; throws [`ApiError`] otherwise. */
export async function unwrap<T>(call: Promise<FetchResult<T>>): Promise<T> {
  const { data, error, response } = await call
  if (!response.ok) {
    const effects = response.status === 409 ? effectsOf(error) : null
    if (effects) throw new ConfirmRequired(codeOf(error), effects)
    throw new ApiError(codeOf(error), response.status)
  }
  return data as T
}

/** The error code of anything thrown by an API call. */
export function errorCode(error: unknown): string {
  return error instanceof ApiError ? error.code : 'unavailable'
}

/** Translate an API error into a sentence (`common:apiErrors.<code>`). */
export function useApiErrorMessage() {
  const { t, i18n } = useTranslation()
  return (error: unknown): string => {
    const key = `apiErrors.${errorCode(error)}`
    return i18n.exists(key) ? t(key as 'apiErrors.unavailable') : t('apiErrors.unavailable')
  }
}
