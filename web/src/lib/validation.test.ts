import { describe, expect, it } from 'vitest'

import {
  acceptInviteSchema,
  changePasswordSchema,
  createInvitationSchema,
  createMemberSchema,
  email,
  newPassword,
  setPasswordSchema,
  username,
} from './validation'

const firstMessage = (result: { success: boolean; error?: { issues: { message: string }[] } }) =>
  result.error?.issues[0]?.message

describe('username', () => {
  it('accepts names the server accepts and normalizes case and spaces', () => {
    expect(username.parse('  Alice.B ')).toBe('alice.b')
    expect(username.parse('a_1-b')).toBe('a_1-b')
    expect(username.parse('007')).toBe('007')
  })

  it.each([
    ['ab', 'usernameLength'],
    ['a'.repeat(33), 'usernameLength'],
    ['_alice', 'usernameChars'],
    ['.alice', 'usernameChars'],
    ['al ice', 'usernameChars'],
    ['älice', 'usernameChars'],
    ['a@b', 'usernameChars'],
  ])('rejects %j with %s', (input, key) => {
    expect(firstMessage(username.safeParse(input))).toBe(key)
  })
})

describe('passwords', () => {
  it('need at least 8 characters, length only', () => {
    expect(firstMessage(newPassword.safeParse('1234567'))).toBe('passwordLength')
    expect(newPassword.safeParse('12345678').success).toBe(true)
    expect(newPassword.safeParse('        ').success).toBe(true)
    expect(firstMessage(newPassword.safeParse('x'.repeat(257)))).toBe('passwordTooLong')
  })

  it('must match their confirmation', () => {
    const result = setPasswordSchema.safeParse({ password: 'long enough', confirm: 'different' })
    expect(result.success).toBe(false)
    expect(result.error?.issues[0]?.path).toEqual(['confirm'])
    expect(firstMessage(result)).toBe('passwordMismatch')
  })

  it('change requires the current password', () => {
    const result = changePasswordSchema.safeParse({
      currentPassword: '',
      password: 'long enough',
      confirm: 'long enough',
    })
    expect(firstMessage(result)).toBe('required')
  })
})

describe('email', () => {
  it('normalizes and loosely checks', () => {
    expect(email.parse(' Mia@Example.ORG ')).toBe('mia@example.org')
    for (const bad of ['mia', 'mia@', '@example.org', 'mia@example', 'm ia@example.org']) {
      expect(firstMessage(email.safeParse(bad))).toBe('emailInvalid')
    }
  })
})

describe('forms', () => {
  it('create member needs username and email, display name is optional', () => {
    expect(
      createMemberSchema.safeParse({ username: 'mia', email: 'mia@example.org', displayName: '' })
        .success,
    ).toBe(true)
    expect(
      createMemberSchema.safeParse({ username: 'mia', email: '', displayName: '' }).success,
    ).toBe(false)
  })

  it('invitation presets may be empty, expiry is 1 to 30 days', () => {
    expect(
      createInvitationSchema.safeParse({ expiresInDays: 7, username: '', email: '' }).success,
    ).toBe(true)
    expect(
      firstMessage(
        createInvitationSchema.safeParse({ expiresInDays: 31, username: '', email: '' }),
      ),
    ).toBe('expiryRange')
    expect(
      createInvitationSchema.safeParse({ expiresInDays: 7, username: '_x', email: '' }).success,
    ).toBe(false)
  })

  it('accepting skips fields the invitation presets', () => {
    const values = { username: '', email: '', password: 'long enough', confirm: 'long enough' }
    expect(acceptInviteSchema({ username: true, email: true }).safeParse(values).success).toBe(true)
    const open = acceptInviteSchema({ username: false, email: false }).safeParse(values)
    expect(open.success).toBe(false)
    expect(new Set(open.error?.issues.map((i) => i.path[0]))).toEqual(
      new Set(['username', 'email']),
    )
  })
})
