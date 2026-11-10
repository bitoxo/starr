import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

import { api } from '@/api/client'
import tmdbLogo from '@/assets/tmdb-logo.svg'
import { PageHeader } from '@/components/AppShell'
import { StatusChip } from '@/components/StatusChip'
import { Card, CardHeader, CardTitle } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'

export default function SettingsPage() {
  const { t } = useTranslation(['admin', 'common'])
  const status = useQuery({
    queryKey: ['system-status'],
    queryFn: async () => {
      const { data, error } = await api.GET('/api/v1/system/status')
      if (error || !data) throw new Error('system status request failed')
      return data
    },
  })

  return (
    <>
      <PageHeader
        variant="admin"
        title={t('settings.about')}
        description={t('settings.description')}
      />
      <Card className="max-w-xl gap-0 p-0">
        <CardHeader className="px-5 py-4">
          <CardTitle>{t('settings.about')}</CardTitle>
        </CardHeader>
        <Separator />
        <dl className="grid grid-cols-[max-content_1fr] items-center gap-x-8 gap-y-3 px-5 py-4 text-base">
          <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
            {t('settings.version')}
          </dt>
          <dd className="type-data text-text">
            {status.isPending && <span className="text-text-3">{t('common:status.loading')}</span>}
            {status.isError && <StatusChip tone="bad">{t('settings.unreachable')}</StatusChip>}
            {status.data?.version}
          </dd>
          <dt className="type-data text-xs tracking-wide text-text-3 uppercase">
            {t('settings.database')}
          </dt>
          <dd>
            {status.data &&
              (status.data.database === 'ok' ? (
                <StatusChip tone="ok">{t('settings.databaseOk')}</StatusChip>
              ) : (
                <StatusChip tone="bad">{t('settings.databaseDown')}</StatusChip>
              ))}
          </dd>
        </dl>
      </Card>
      <Attribution />
    </>
  )
}

const CC0 = 'https://creativecommons.org/publicdomain/zero/1.0/'
const CC_BY_NC_SA = 'https://creativecommons.org/licenses/by-nc-sa/3.0/'

function Licence({ href, children }: { href: string; children: string }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="rounded-control text-text underline decoration-line underline-offset-2 hover:decoration-text"
    >
      {children}
    </a>
  )
}

/**
 * Where Starr's catalog data comes from and under which terms (spec 0008 Q17, slice 10): TMDB's
 * notice and logo (less prominent than Starr's own), JustWatch for streaming offers, MusicBrainz
 * core data (CC0) and genres (CC BY-NC-SA, displayed and cached only), ListenBrainz listens.
 */
function Attribution() {
  const { t } = useTranslation('admin')
  return (
    <Card className="mt-6 max-w-xl gap-0 p-0">
      <CardHeader className="px-5 py-4">
        <CardTitle>{t('settings.attribution.title')}</CardTitle>
      </CardHeader>
      <Separator />
      <ul className="flex flex-col divide-y divide-line text-sm text-text-2">
        <li className="flex items-start gap-4 px-5 py-4">
          <img
            src={tmdbLogo}
            alt={t('settings.attribution.tmdbLogo')}
            className="mt-1 h-3 w-auto shrink-0"
          />
          <p>{t('settings.attribution.tmdbNotice')}</p>
        </li>
        <li className="px-5 py-4">
          <p>{t('settings.attribution.justwatch')}</p>
        </li>
        <li className="flex flex-col gap-1.5 px-5 py-4">
          <p className="font-medium text-text">{t('settings.attribution.musicbrainz')}</p>
          <p>
            {t('settings.attribution.musicbrainzCore')} <Licence href={CC0}>CC0 1.0</Licence>
          </p>
          <p>
            {t('settings.attribution.musicbrainzGenres')}{' '}
            <Licence href={CC_BY_NC_SA}>CC BY-NC-SA 3.0</Licence>
          </p>
          <p className="text-text-3">{t('settings.attribution.musicbrainzShareAlike')}</p>
        </li>
        <li className="flex flex-col gap-1.5 px-5 py-4">
          <p className="font-medium text-text">{t('settings.attribution.listenbrainz')}</p>
          <p>
            {t('settings.attribution.listenbrainzText')} <Licence href={CC0}>CC0 1.0</Licence>
          </p>
        </li>
      </ul>
    </Card>
  )
}
