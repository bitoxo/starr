import admin from './en/admin.json'
import auth from './en/auth.json'
import common from './en/common.json'
import member from './en/member.json'

// English only for now (ADR 0007). Bundled: no async loading, no flash of keys.
export const resources = { en: { common, auth, admin, member } } as const

export const namespaces = ['common', 'auth', 'admin', 'member'] as const
