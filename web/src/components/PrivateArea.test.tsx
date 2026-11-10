import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Cover } from './Poster'
import { PrivateArea } from './PrivateArea'

describe('private covers', () => {
  it('are refused outside the private area: no image, no title, not even blurred', () => {
    const { container } = render(
      <Cover file="scene-a.jpg" title="Scene A" sizes="80px" category="adult" />,
    )
    expect(container.querySelector('img')).toBeNull()
    expect(container.textContent).not.toContain('Scene A')
    expect(container.querySelector('[data-private-cover]')).not.toBeNull()
  })

  it('show inside it in 16:9, only through the private image source', () => {
    const { container } = render(
      <PrivateArea>
        <Cover
          file="1a6e0000-0000-4000-8000-000000000002"
          title="Scene A"
          sizes="80px"
          category="adult"
        />
      </PrivateArea>,
    )
    const img = container.querySelector('img')
    expect(img?.getAttribute('src')).toBe(
      '/api/v1/images/stashdb/600/1a6e0000-0000-4000-8000-000000000002',
    )
    expect(img?.getAttribute('srcset')).not.toContain('/tmdb/')
    expect(img?.className).toContain('aspect-video')
  })

  it('show their title inside it while there is no image', () => {
    const { container } = render(
      <PrivateArea>
        <Cover file={null} title="Scene A" sizes="80px" category="adult" />
      </PrivateArea>,
    )
    expect(container.querySelector('img')).toBeNull()
    expect(container.textContent).toContain('Scene A')
    expect(container.innerHTML).toContain('aspect-video')
  })

  it('leave other categories alone', () => {
    const { container } = render(
      <Cover file="heat.jpg" title="Heat" sizes="80px" category="movies" />,
    )
    expect(container.querySelector('img')?.getAttribute('src')).toBe(
      '/api/v1/images/tmdb/w342/heat.jpg',
    )
  })
})
