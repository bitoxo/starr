import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { AcquisitionStateChip, RequestStateChip } from './RequestStatus'

describe('download stage chips', () => {
  it('names the wait from the admin’s side', () => {
    render(
      <AcquisitionStateChip
        state="downloading"
        stage="arriving"
        source="Radarr"
        arrivingIn="Jellyfin"
      />,
    )
    expect(screen.getByText('Arriving in Jellyfin')).toBeTruthy()
  })

  it('falls back to generic names and to the plain state', () => {
    const { rerender } = render(<AcquisitionStateChip state="downloading" stage="arriving" />)
    expect(screen.getByText('Almost ready')).toBeTruthy()
    rerender(<AcquisitionStateChip state="downloading" stage="importing" />)
    expect(screen.getByText('Importing')).toBeTruthy()
    // A stage outside downloading is stale: the state wins.
    rerender(<AcquisitionStateChip state="available" stage="arriving" />)
    expect(screen.getByText('Available')).toBeTruthy()
    rerender(<RequestStateChip state="in_progress" stage="arriving" arrivingIn="Kino" admin />)
    expect(screen.getByText('Arriving in Kino')).toBeTruthy()
    rerender(<RequestStateChip state="in_progress" stage="arriving" arrivingIn="Kino" />)
    expect(screen.getByText('Arriving in Kino')).toBeTruthy()
    rerender(<RequestStateChip state="in_progress" />)
    expect(screen.getByText('Downloading')).toBeTruthy()
  })
})
