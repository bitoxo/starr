/*
 * Flat tint from a cover (design README A1, spec 0008 Q16 A): the already loaded, same-origin
 * (proxied) cover is drawn into an 8×8 canvas, its pixels are averaged in OKLab (the circular
 * mean of OKLCH hue), and the colour is clamped to lightness ≤ 0.30 and chroma ≤ 0.08. The
 * surface then is `color-mix(in oklch, var(--tint) 22%, var(--night-1))`, which keeps every text
 * token at ≥ 4.5:1 (unit test). The colour lives in memory only: never stored, so nothing
 * sampled from a private cover outlives the page.
 */

export type Oklch = { l: number; c: number; h: number }

/** The clamp of A1. */
export const TINT_MAX_L = 0.3
export const TINT_MAX_C = 0.08
/** Share of the tint in the surface mix. */
export const TINT_SHARE = 0.22

function toLinear(channel: number): number {
  const c = channel / 255
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
}

function fromLinear(c: number): number {
  const v = Math.min(1, Math.max(0, c))
  return v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055
}

/** sRGB (0–255) to OKLab (Björn Ottosson's matrices). */
export function srgbToOklab(r: number, g: number, b: number): [number, number, number] {
  const [lr, lg, lb] = [toLinear(r), toLinear(g), toLinear(b)]
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb)
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb)
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb)
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ]
}

/** OKLCH to sRGB (0–1 per channel, clipped to the gamut). */
export function oklchToSrgb({ l, c, h }: Oklch): [number, number, number] {
  const a = c * Math.cos((h * Math.PI) / 180)
  const b = c * Math.sin((h * Math.PI) / 180)
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3
  return [
    fromLinear(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_),
    fromLinear(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_),
    fromLinear(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_),
  ]
}

function toOklch([l, a, b]: [number, number, number]): Oklch {
  const h = (Math.atan2(b, a) * 180) / Math.PI
  return { l, c: Math.hypot(a, b), h: (h + 360) % 360 }
}

/** A `#rrggbb` colour in OKLCH. */
export function hexToOklch(hex: string): Oklch {
  const n = Number.parseInt(hex.slice(1), 16)
  return toOklch(srgbToOklab((n >> 16) & 255, (n >> 8) & 255, n & 255))
}

/** Clamp a sampled colour to the tint range of A1. */
export function clampTint(color: Oklch): Oklch {
  return { l: Math.min(color.l, TINT_MAX_L), c: Math.min(color.c, TINT_MAX_C), h: color.h }
}

/** The mean colour of RGBA pixels (transparent ones skipped), or null if there are none. */
export function averageColor(pixels: ArrayLike<number>): Oklch | null {
  let n = 0
  const sum = [0, 0, 0]
  for (let i = 0; i + 3 < pixels.length; i += 4) {
    if ((pixels[i + 3] ?? 0) < 128) continue
    const lab = srgbToOklab(pixels[i] ?? 0, pixels[i + 1] ?? 0, pixels[i + 2] ?? 0)
    sum[0] += lab[0]
    sum[1] += lab[1]
    sum[2] += lab[2]
    n += 1
  }
  if (n === 0) return null
  return toOklch([sum[0]! / n, sum[1]! / n, sum[2]! / n])
}

/** `color-mix(in oklch, tint share%, base)`, hue the shorter way round (CSS Color 4). */
export function mixOklch(tint: Oklch, base: Oklch, share = TINT_SHARE): Oklch {
  // A grey has no hue of its own: it takes the other colour's.
  const tintHue = tint.c < 1e-4 ? base.h : tint.h
  const delta = ((((tintHue - base.h) % 360) + 540) % 360) - 180
  return {
    l: base.l + (tint.l - base.l) * share,
    c: base.c + (tint.c - base.c) * share,
    h: (base.h + delta * share + 360) % 360,
  }
}

/** A CSS value for `--tint`. */
export function tintCss({ l, c, h }: Oklch): string {
  return `oklch(${l.toFixed(3)} ${c.toFixed(3)} ${h.toFixed(1)})`
}

/**
 * The clamped tint of a loaded cover, as a CSS colour, or null when it cannot be read (not yet
 * decoded, a canvas-less engine, a cross-origin image). Not cached: reading 8×8 pixels is cheap,
 * a not-yet-loaded cover gets its tint on the next call, and nothing sampled from a private
 * cover outlives the element that shows it.
 */
export function sampleCoverTint(img: HTMLImageElement): string | null {
  let tint: string | null = null
  try {
    const canvas = document.createElement('canvas')
    canvas.width = 8
    canvas.height = 8
    const context = canvas.getContext('2d', { willReadFrequently: true })
    if (context && img.complete && img.naturalWidth > 0) {
      context.drawImage(img, 0, 0, 8, 8)
      const color = averageColor(context.getImageData(0, 0, 8, 8).data)
      tint = color ? tintCss(clampTint(color)) : null
    }
  } catch {
    // A tainted canvas (an image from elsewhere) cannot be read: no tint, the plain surface.
    tint = null
  }
  return tint
}
