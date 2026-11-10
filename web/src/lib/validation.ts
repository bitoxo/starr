import { z } from 'zod'

import type auth from '@/locales/en/auth.json'

/** Zod messages are keys under `auth:validation.*`; `FormField` translates them. */
export type ValidationKey = keyof (typeof auth)['validation']

const msg = (key: ValidationKey) => ({ message: key })

// Mirrors the server policy (starr_core::auth::validate_password): length only.
export const PASSWORD_MIN = 8
export const PASSWORD_MAX = 256
// Mirrors starr_core::auth::DISPLAY_NAME_MAX_CHARS.
export const DISPLAY_NAME_MAX = 64

/** Mirrors starr_core::auth::parse_username: 3–32 of a-z 0-9 . _ -, starting with a letter or digit. */
export const username = z
  .string()
  .trim()
  .toLowerCase()
  .min(3, msg('usernameLength'))
  .max(32, msg('usernameLength'))
  .regex(/^[a-z0-9][a-z0-9._-]*$/, msg('usernameChars'))

/** Loose check like starr_core::members::parse_email; the server has the last word. */
export const email = z
  .string()
  .trim()
  .toLowerCase()
  .max(254, msg('emailInvalid'))
  .regex(/^[^\s@]+@[^\s@.][^\s@]*\.[^\s@.]+$/, msg('emailInvalid'))

export const displayName = z.string().trim().max(DISPLAY_NAME_MAX, msg('displayNameLength'))

export const newPassword = z
  .string()
  .min(PASSWORD_MIN, msg('passwordLength'))
  .max(PASSWORD_MAX, msg('passwordTooLong'))

export const required = z.string().trim().min(1, msg('required'))

/**
 * TOTP code (6 digits) or recovery code (12 characters, dashes optional).
 * Mirrors starr_core::auth::parse_second_factor loosely; the server decides.
 */
export const secondFactor = z
  .string()
  .trim()
  .regex(/^(\d\s*){6}$|^[A-Za-z0-9]{4}-?[A-Za-z0-9]{4}-?[A-Za-z0-9]{4}$/, msg('codeFormat'))

/** A six-digit TOTP code only (no recovery code). */
export const totpCode = z
  .string()
  .trim()
  .regex(/^(\d\s*){6}$/, msg('totpCodeFormat'))

/** Existing password: never trimmed, spaces are significant. */
export const password = z.string().min(1, msg('required'))

const passwordPair = { password: newPassword, confirm: z.string() }
const confirmMatches = (v: { password: string; confirm: string }) => v.password === v.confirm
const mismatch = { ...msg('passwordMismatch'), path: ['confirm'] }

/** `/set-password/:token`: new password plus matching confirmation. */
export const setPasswordSchema = z.object(passwordPair).refine(confirmMatches, mismatch)

/**
 * `/invite/:token`: username and email unless the invitation presets them
 * (preset fields are not shown and not validated), new password plus
 * confirmation.
 */
export function acceptInviteSchema(preset: { username: boolean; email: boolean }) {
  return z
    .object({
      username: preset.username ? z.string() : username,
      email: preset.email ? z.string() : email,
      ...passwordPair,
    })
    .refine(confirmMatches, mismatch)
}

/** Admin: create another admin account. */
export const createAdminSchema = z.object({ username, displayName })

/** Admin: create a member directly. */
export const createMemberSchema = z.object({ username, email, displayName })

/** Admin: create an invitation; empty preset fields mean "let the person choose". */
export const createInvitationSchema = z.object({
  expiresInDays: z.number().int().min(1, msg('expiryRange')).max(30, msg('expiryRange')),
  username: z.union([z.literal(''), username]),
  email: z.union([z.literal(''), email]),
})

/** Member: profile. */
export const profileSchema = z.object({ displayName })

/** Member: change password. */
export const changePasswordSchema = z
  .object({ currentPassword: password, ...passwordPair })
  .refine(confirmMatches, mismatch)
