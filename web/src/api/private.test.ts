import { describe, expect, it } from 'vitest'

import { PRIVATE_PREFIX, privateKey } from './private'
import source from './private.ts?raw'

describe('private query keys', () => {
  it('all start with the private prefix', () => {
    expect(privateKey('scene', 'x')).toEqual([PRIVATE_PREFIX, 'scene', 'x'])
    expect(privateKey()).toEqual([PRIVATE_PREFIX])
  })

  it('are built only through privateKey, so closing the gate can drop all of them', () => {
    // Spec 0008 F2: one helper builds every key of this file.
    const keys = [...source.matchAll(/queryKey:\s*([^,\n}]+)/g)].map((m) => m[1]?.trim())
    expect(keys.length).toBeGreaterThan(5)
    for (const key of keys) expect(key).toMatch(/^privateKey\(/)
  })
})
