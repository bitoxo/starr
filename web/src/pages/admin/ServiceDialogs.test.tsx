import { fireEvent, screen, waitFor, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { Service, ServiceType } from '@/api/services'
import { fail, ok, renderWithProviders } from '@/test/render'

import { DeleteServiceDialog, ServiceFormDialog } from './ServiceDialogs'

const api = vi.hoisted(() => ({ GET: vi.fn(), POST: vi.fn(), PATCH: vi.fn() }))
vi.mock('@/api/client', () => ({ api }))

const TYPES = [
  {
    typeId: 'navidrome',
    displayName: 'Navidrome',
    capabilities: ['libraries'],
    categories: ['music'],
    testedVersions: { min: '0.58.0.0', maxExclusive: '0.65.0.0' },
    needsCredential: true,
    credential: 'login',
    defaultBaseUrl: null,
  },
] satisfies ServiceType[]

describe('Editing a source in use', () => {
  const radarr = {
    id: 3,
    adapterType: 'radarr',
    name: 'Radarr',
    baseUrl: 'http://radarr:7878',
    publicUrl: null,
    mode: 'full',
    capabilities: ['requests'],
    createdAt: '2026-11-03T10:00:00Z',
    updatedAt: '2026-11-03T10:00:00Z',
  }
  const types = [
    {
      typeId: 'radarr',
      displayName: 'Radarr',
      capabilities: ['requests'],
      categories: ['movies'],
      testedVersions: { min: '5.0.0.0', maxExclusive: '7.0.0.0' },
      needsCredential: true,
      credential: 'api_key',
      defaultBaseUrl: null,
    },
  ] satisfies ServiceType[]

  beforeEach(() => {
    api.GET.mockReset()
    api.PATCH.mockReset()
    api.POST.mockReset()
    api.GET.mockResolvedValue(ok(types))
  })

  it('names what a new URL does to the categories and saves only when confirmed (ADM-01)', async () => {
    api.PATCH.mockImplementation((_path: string, init: { body: { confirm?: boolean } }) =>
      Promise.resolve(
        init.body.confirm
          ? ok({ service: { ...radarr, baseUrl: 'http://radarr.lan:7878' }, jobId: null })
          : {
              data: undefined,
              error: {
                code: 'services.confirm_required',
                effects: [{ kind: 'sourceSettingsRechecked', category: 'movies' }],
              },
              response: new Response(null, { status: 409 }),
            },
      ),
    )
    const onOpenChange = vi.fn()
    renderWithProviders(
      <ServiceFormDialog open onOpenChange={onOpenChange} service={radarr as never} />,
    )
    fireEvent.change(await screen.findByLabelText('URL'), {
      target: { value: 'http://radarr.lan:7878' },
    })
    fireEvent.change(screen.getByLabelText('API key'), { target: { value: 'k' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByText(/Movies keeps its quality profile/)).toBeTruthy()
    expect(api.PATCH).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByRole('button', { name: 'Save the new URL' }))
    await waitFor(() =>
      expect(api.PATCH).toHaveBeenLastCalledWith('/api/v1/services/{id}', {
        params: { path: { id: 3 } },
        body: expect.objectContaining({ baseUrl: 'http://radarr.lan:7878', confirm: true }),
      }),
    )
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
  })

  it('asks for the API key again when the URL points to another server', async () => {
    renderWithProviders(<ServiceFormDialog open onOpenChange={vi.fn()} service={radarr as never} />)
    fireEvent.change(await screen.findByLabelText('URL'), {
      target: { value: 'http://elsewhere:7878' },
    })
    expect(
      await screen.findByText(/points to another server: enter the API key again/),
    ).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(screen.getByLabelText('API key').getAttribute('aria-invalid')).toBe('true'),
    )
    expect(screen.getByText(/never sends the stored one to a new server/)).toBeTruthy()
    expect(api.PATCH).not.toHaveBeenCalled()
  })

  it('keeps the stored key for another path on the same server', async () => {
    api.PATCH.mockResolvedValue(
      ok({ service: { ...radarr, baseUrl: 'http://radarr:7878/radarr' }, jobId: null }),
    )
    renderWithProviders(<ServiceFormDialog open onOpenChange={vi.fn()} service={radarr as never} />)
    fireEvent.change(await screen.findByLabelText('URL'), {
      target: { value: 'http://radarr:7878/radarr' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(api.PATCH).toHaveBeenCalledTimes(1))
    expect(api.PATCH.mock.calls[0]?.[1].body.apiKey).toBeUndefined()
  })

  it('asks to re-authenticate when the server wants it, then saves', async () => {
    api.PATCH.mockResolvedValueOnce(fail('auth.elevation_required', 403)).mockResolvedValueOnce(
      ok({ service: { ...radarr, baseUrl: 'http://radarr:7878/radarr' }, jobId: null }),
    )
    api.POST.mockResolvedValue(ok({ elevatedUntil: '2099-01-01T00:00:00Z' }))
    const onOpenChange = vi.fn()
    renderWithProviders(
      <ServiceFormDialog open onOpenChange={onOpenChange} service={radarr as never} />,
    )
    fireEvent.change(await screen.findByLabelText('URL'), {
      target: { value: 'http://radarr:7878/radarr' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    const elevation = await screen.findByRole('dialog', { name: "Confirm it's you" })
    fireEvent.change(within(elevation).getByLabelText('Admin password'), {
      target: { value: 'correct horse battery staple' },
    })
    fireEvent.click(within(elevation).getByRole('button', { name: 'Confirm' }))
    await waitFor(() => expect(api.PATCH).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
  })
})

describe('Adding Navidrome', () => {
  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.GET.mockResolvedValue(ok(TYPES))
    api.POST.mockResolvedValue(
      ok({
        service: {
          id: 5,
          adapterType: 'navidrome',
          name: 'Navidrome',
          baseUrl: 'http://navidrome:4533',
          publicUrl: 'https://music.example.com',
          mode: 'probe',
          capabilities: ['libraries'],
          createdAt: '2026-11-03T10:00:00Z',
          updatedAt: '2026-11-03T10:00:00Z',
        },
        jobId: null,
      }),
    )
  })

  it('takes the admin login instead of an API key', async () => {
    renderWithProviders(<ServiceFormDialog open onOpenChange={() => {}} />)
    const username = await screen.findByLabelText('Admin username')
    expect(screen.queryByLabelText(/API key/)).toBeNull()
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Navidrome' } })
    fireEvent.change(screen.getByLabelText('URL'), { target: { value: 'http://navidrome:4533' } })
    fireEvent.change(screen.getByLabelText('Public URL (optional)'), {
      target: { value: 'https://music.example.com' },
    })

    // Both halves of the login are needed.
    fireEvent.click(screen.getByRole('button', { name: 'Add and check' }))
    await waitFor(() => expect(username.getAttribute('aria-invalid')).toBe('true'))
    expect(api.POST).not.toHaveBeenCalled()

    fireEvent.change(username, { target: { value: 'starr' } })
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: ' secret ' } })
    fireEvent.click(screen.getByRole('button', { name: 'Add and check' }))
    await waitFor(() =>
      expect(api.POST).toHaveBeenCalledWith('/api/v1/services', {
        body: {
          adapterType: 'navidrome',
          name: 'Navidrome',
          baseUrl: 'http://navidrome:4533',
          username: 'starr',
          password: ' secret ',
          publicUrl: 'https://music.example.com',
        },
      }),
    )
  })
})

describe('Adding a service (ADM-20)', () => {
  const sonarr = {
    typeId: 'sonarr',
    displayName: 'Sonarr',
    capabilities: ['requests'],
    categories: ['series'],
    testedVersions: { min: '4.0.0.0', maxExclusive: '5.0.0.0' },
    needsCredential: true,
    credential: 'api_key',
    defaultBaseUrl: null,
  } satisfies ServiceType

  beforeEach(() => {
    api.GET.mockReset()
    api.POST.mockReset()
    api.GET.mockResolvedValue(ok([sonarr, ...TYPES]))
  })

  it('prefills name and example address per type and reports all missing fields at once', async () => {
    renderWithProviders(<ServiceFormDialog open onOpenChange={() => {}} />)
    const name = (await screen.findByLabelText('Name')) as HTMLInputElement
    expect(name.value).toBe('Sonarr')
    expect(screen.getByLabelText('URL').getAttribute('placeholder')).toBe('http://sonarr:8989')
    fireEvent.change(screen.getByLabelText('Service'), { target: { value: 'navidrome' } })
    expect(name.value).toBe('Navidrome')
    expect(screen.getByLabelText('URL').getAttribute('placeholder')).toBe('http://navidrome:4533')

    fireEvent.click(screen.getByRole('button', { name: 'Add and check' }))
    // URL, username and password together, not one after the other.
    await waitFor(() => expect(screen.getAllByText('This field is required.')).toHaveLength(2))
    expect(screen.getByLabelText('URL').getAttribute('aria-invalid')).toBe('true')
    expect(api.POST).not.toHaveBeenCalled()
  })
})

describe('Removing a service in use (ADM-21)', () => {
  it('lists what it takes away and asks for the name', async () => {
    api.GET.mockResolvedValue(
      ok([
        {
          category: 'movies',
          catalogs: [],
          source: { id: 1, serviceId: 3, serviceName: 'Radarr' },
          consumers: [],
        },
      ]),
    )
    renderWithProviders(
      <DeleteServiceDialog
        service={{ id: 3, name: 'Radarr' } as Service}
        onOpenChange={() => {}}
      />,
    )
    expect(await screen.findByText(/Movies: the source requests go to/)).toBeTruthy()
    const remove = screen.getByRole('button', { name: 'Remove' })
    expect(remove).toHaveProperty('disabled', true)
    fireEvent.change(screen.getByLabelText(/Type Radarr/), { target: { value: 'Radarr' } })
    expect(remove).toHaveProperty('disabled', false)
  })
})
