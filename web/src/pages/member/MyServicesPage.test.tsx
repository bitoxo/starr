import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { expectFocusNotOnBody } from '@/test/focus'
import { fail, ok, renderWithProviders } from '@/test/render'

import MyServicesPage from './MyServicesPage'

const api = vi.hoisted(() => ({ POST: vi.fn(), GET: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

function service(overrides: Record<string, unknown>) {
  return {
    serviceId: 1,
    name: 'Kino',
    state: 'ready',
    username: null,
    serverAddress: 'https://kino.example.org',
    usesStarrPassword: true,
    passwordChangeSignsOut: true,
    acceptsPassword: true,
    lockedByStarr: false,
    ...overrides,
  }
}

const SERVICES = {
  services: [
    service({}),
    service({ serviceId: 2, name: 'Emby', state: 'activate', serverAddress: null }),
    service({
      serviceId: 3,
      name: 'Garten',
      state: 'oldPassword',
      username: 'Alex',
      usesStarrPassword: false,
    }),
  ],
}

describe('My services', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.GET.mockResolvedValue(ok(SERVICES))
  })

  it('shows each service in member words with the server address', async () => {
    renderWithProviders(<MyServicesPage />)
    const kino = (await screen.findByRole('heading', { name: 'Kino' })).closest('li') as HTMLElement
    expect(within(kino).getByText('Ready')).toBeTruthy()
    // The address is a link, and can be copied (MEM-22).
    const link = within(kino).getByRole('link', { name: /https:\/\/kino\.example\.org/ })
    expect(link.getAttribute('href')).toBe('https://kino.example.org/')
    expect(link.getAttribute('target')).toBe('_blank')
    expect(
      within(kino).getByRole('button', { name: 'Copy the server address of Kino' }),
    ).toBeTruthy()
    const emby = screen.getByRole('heading', { name: 'Emby' }).closest('li') as HTMLElement
    expect(within(emby).getByText('Needs your password')).toBeTruthy()
    expect(within(emby).getByText('Ask your admin for the server address.')).toBeTruthy()
    const garten = screen.getByRole('heading', { name: 'Garten' }).closest('li') as HTMLElement
    expect(within(garten).getByText('Uses your old password')).toBeTruthy()
    expect(within(garten).getByText('Alex')).toBeTruthy()
    expect(
      within(garten).getByRole('button', { name: 'Copy your username in Garten' }),
    ).toBeTruthy()
    // One intro that fits every card: here, activating comes first.
    expect(screen.getByText(/to finish your accounts/)).toBeTruthy()
    expect(screen.queryByText(/Sign in to these apps with your Starr username/)).toBeNull()
  })

  it('copies the server address and confirms it', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })
    renderWithProviders(<MyServicesPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Copy the server address of Kino' }))
    await waitFor(() => expect(writeText).toHaveBeenCalledWith('https://kino.example.org'))
    expect(await screen.findByText('Copied.')).toBeTruthy()
  })

  it('says what a service waits for and lists undecided ones (MEM-22)', async () => {
    api.GET.mockResolvedValue(
      ok({
        services: [
          service({ name: 'Kino', state: 'waiting', waitingFor: 'nameTaken' }),
          service({ serviceId: 2, name: 'Emby', state: 'waiting', waitingFor: 'writesOff' }),
          service({ serviceId: 3, name: 'Navidrome', state: 'undecided', serverAddress: null }),
          service({ serviceId: 4, name: 'Garten', state: 'settingUp' }),
        ],
      }),
    )
    renderWithProviders(<MyServicesPage />)
    const kino = (await screen.findByRole('heading', { name: 'Kino' })).closest('li') as HTMLElement
    expect(within(kino).getByText('Waiting for your admin')).toBeTruthy()
    expect(within(kino).getByText(/Your username is already taken there/)).toBeTruthy()
    const emby = screen.getByRole('heading', { name: 'Emby' }).closest('li') as HTMLElement
    expect(within(emby).getByText(/once your admin turns on account changes/)).toBeTruthy()
    const nd = screen.getByRole('heading', { name: 'Navidrome' }).closest('li') as HTMLElement
    expect(within(nd).getByText('Not decided yet')).toBeTruthy()
    expect(within(nd).getByText(/hasn't decided about an account/)).toBeTruthy()
    const garten = screen.getByRole('heading', { name: 'Garten' }).closest('li') as HTMLElement
    expect(within(garten).getByText('Being set up')).toBeTruthy()
    expect(within(garten).getByText(/Starr is setting up this account right now/)).toBeTruthy()
    expect(screen.getByText(/Some accounts aren't ready yet/)).toBeTruthy()
  })

  it('activates with the current password and says when it is wrong', async () => {
    api.POST.mockResolvedValueOnce(
      fail('auth.current_password_invalid', 422),
    ).mockResolvedValueOnce(ok(SERVICES))
    renderWithProviders(<MyServicesPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Activate with your password' }))
    const dialog = await screen.findByRole('dialog', { name: 'Activate your accounts' })
    fireEvent.change(within(dialog).getByLabelText('Your Starr password'), {
      target: { value: 'wrong one' },
    })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Activate with your password' }))
    // The error names the field it belongs to, in its words, and focus stays in the form.
    expect(
      await within(dialog).findByText("That isn't your Starr password. Try again."),
    ).toBeTruthy()
    expect(within(dialog).queryByText('Current password is wrong.')).toBeNull()
    expectFocusNotOnBody()
    fireEvent.change(within(dialog).getByLabelText('Your Starr password'), {
      target: { value: 'the right one' },
    })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Activate with your password' }))
    await waitFor(() => expect(api.POST).toHaveBeenCalledTimes(2))
    expect(api.POST).toHaveBeenLastCalledWith('/api/v1/me/services/activate', {
      body: { password: 'the right one' },
    })
  })

  it('switches an adopted account only after saying its devices sign out', async () => {
    api.POST.mockResolvedValue(ok(SERVICES))
    renderWithProviders(<MyServicesPage />)
    fireEvent.click(await screen.findByRole('button', { name: 'Use my Starr password here' }))
    const dialog = await screen.findByRole('dialog', {
      name: 'Use your Starr password in Garten?',
    })
    expect(
      within(dialog).getByText(
        'All your devices signed in to Garten will be signed out. Sign in again with your Starr password.',
      ),
    ).toBeTruthy()
    fireEvent.change(within(dialog).getByLabelText('Your Starr password'), {
      target: { value: 'my password' },
    })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Use my Starr password here' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/me/services/{serviceId}/use-starr-password', {
        params: { path: { serviceId: 3 } },
        body: { password: 'my password' },
      }),
    )
  })

  it('gives focus back to the opener when the dialog closes', async () => {
    renderWithProviders(<MyServicesPage />)
    const opener = await screen.findByRole('button', { name: 'Use my Starr password here' })
    opener.focus()
    fireEvent.click(opener)
    const dialog = await screen.findByRole('dialog')
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull())
    await waitFor(() => expect(document.activeElement).toBe(opener))
  })

  it('offers no switch where Starr does not write yet', async () => {
    api.GET.mockResolvedValue(
      ok({ services: [service({ state: 'oldPassword', acceptsPassword: false })] }),
    )
    renderWithProviders(<MyServicesPage />)
    expect(await screen.findByText(/Switching becomes possible/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Use my Starr password here' })).toBeNull()
  })

  it('explains a Navidrome account Starr locked with a random password', async () => {
    api.GET.mockResolvedValue(
      ok({
        services: [
          service({ name: 'Navidrome', state: 'activate', lockedByStarr: true }),
          service({ serviceId: 2, name: 'Plattenkiste', state: 'suspended', lockedByStarr: true }),
        ],
      }),
    )
    renderWithProviders(<MyServicesPage />)
    const nd = (await screen.findByRole('heading', { name: 'Navidrome' })).closest(
      'li',
    ) as HTMLElement
    expect(within(nd).getByText(/Enter your password once to unlock it/)).toBeTruthy()
    const other = screen.getByRole('heading', { name: 'Plattenkiste' }).closest('li') as HTMLElement
    expect(within(other).getByText(/your Starr password unlocks it/)).toBeTruthy()
    // One activation unlocks it.
    expect(screen.getByRole('button', { name: 'Activate with your password' })).toBeTruthy()
  })
})
