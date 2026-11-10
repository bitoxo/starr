import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  averageColor,
  clampTint,
  hexToOklch,
  mixOklch,
  oklchToSrgb,
  sampleCoverTint,
  TINT_MAX_C,
  TINT_MAX_L,
} from '@/lib/tint'

// Design tokens (docs/design/README.md).
const NIGHT_1 = '#121821'
const TEXT = { text: '#E9EDF2', 'text-2': '#9AA6B8', 'text-3': '#7E8BA1' }

function luminance([r, g, b]: [number, number, number]): number {
  const lin = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

function hexRgb(hex: string): [number, number, number] {
  const n = Number.parseInt(hex.slice(1), 16)
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255]
}

function contrast(a: number, b: number): number {
  const [hi, lo] = a > b ? [a, b] : [b, a]
  return (hi + 0.05) / (lo + 0.05)
}

function pixels(rgb: [number, number, number], count = 64): Uint8ClampedArray {
  const data = new Uint8ClampedArray(count * 4)
  for (let i = 0; i < count; i += 1) data.set([...rgb, 255], i * 4)
  return data
}

afterEach(() => vi.restoreAllMocks())

describe('cover tint (A1)', () => {
  it('clamps lightness to 0.30 and chroma to 0.08, keeping the hue', () => {
    expect(clampTint({ l: 0.9, c: 0.3, h: 40 })).toEqual({ l: TINT_MAX_L, c: TINT_MAX_C, h: 40 })
    expect(clampTint({ l: 0.1, c: 0.02, h: 200 })).toEqual({ l: 0.1, c: 0.02, h: 200 })
  })

  it('averages pixels in OKLab and skips transparent ones', () => {
    const red = averageColor(pixels([255, 0, 0]))
    expect(red?.l).toBeCloseTo(0.628, 2)
    expect(red?.h).toBeCloseTo(29.2, 0)
    const half = new Uint8ClampedArray([255, 0, 0, 255, 0, 0, 255, 0])
    expect(averageColor(half)?.h).toBeCloseTo(29.2, 0)
    expect(averageColor(new Uint8ClampedArray([0, 0, 0, 0]))).toBeNull()
  })

  it('keeps every text token at 4.5:1 or more against the clamped extremes', () => {
    const base = hexToOklch(NIGHT_1)
    let worst = Number.POSITIVE_INFINITY
    for (let h = 0; h < 360; h += 5) {
      for (const l of [0, 0.1, 0.2, TINT_MAX_L]) {
        for (const c of [0, 0.04, TINT_MAX_C]) {
          const surface = luminance(oklchToSrgb(mixOklch({ l, c, h }, base)))
          for (const hex of Object.values(TEXT)) {
            worst = Math.min(worst, contrast(luminance(hexRgb(hex)), surface))
          }
        }
      }
    }
    expect(worst).toBeGreaterThanOrEqual(4.5)
  })

  it('samples a loaded cover through an 8×8 canvas, in memory only', () => {
    const getImageData = vi.fn(() => ({ data: pixels([40, 120, 200]) }))
    const canvas = {
      width: 0,
      height: 0,
      getContext: () => ({ drawImage: vi.fn(), getImageData }),
    }
    const create = document.createElement.bind(document)
    vi.spyOn(document, 'createElement').mockImplementation((tag: string) =>
      tag === 'canvas' ? (canvas as unknown as HTMLCanvasElement) : create(tag),
    )
    const setItem = vi.spyOn(Storage.prototype, 'setItem')
    const img = {
      src: '/api/v1/images/caa/250/a',
      currentSrc: '',
      complete: true,
      naturalWidth: 250,
    }
    const tint = sampleCoverTint(img as HTMLImageElement)
    expect(tint).toMatch(/^oklch\(0\.300 0\.080 \d+(\.\d)?\)$/)
    expect(canvas.width).toBe(8)
    // Nothing is kept: no storage write, and an image not loaded yet is read again later.
    expect(getImageData).toHaveBeenCalledOnce()
    expect(setItem).not.toHaveBeenCalled()
    const loading = { ...img, src: '/api/v1/images/caa/250/b', complete: false, naturalWidth: 0 }
    expect(sampleCoverTint(loading as HTMLImageElement)).toBeNull()
    expect(
      sampleCoverTint({ ...loading, complete: true, naturalWidth: 250 } as HTMLImageElement),
    ).toBe(tint)
  })

  it('gives up quietly on an unreadable (cross-origin) image', () => {
    const canvas = {
      getContext: () => ({
        drawImage: vi.fn(),
        getImageData: () => {
          throw new DOMException('tainted', 'SecurityError')
        },
      }),
    }
    const create = document.createElement.bind(document)
    vi.spyOn(document, 'createElement').mockImplementation((tag: string) =>
      tag === 'canvas' ? (canvas as unknown as HTMLCanvasElement) : create(tag),
    )
    const img = { src: 'https://elsewhere.example/x.jpg', complete: true, naturalWidth: 10 }
    expect(sampleCoverTint(img as HTMLImageElement)).toBeNull()
  })
})
