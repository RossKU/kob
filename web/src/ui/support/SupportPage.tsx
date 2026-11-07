// `#/support`: the volunteers' note, the donation address and those who gave to it, linked from the footer of every page (support-model.ts). The text is in src/i18n/en/support.ts; the table is read from the public Kaspa explorer (data/contributors.ts).
import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { readContributions, type Contributions } from '../../data/contributors';
import { formatDateTime, t } from '../../i18n';
import { formatKas } from '../../kob/units';
import { CopyText, shortAddress, Table, TableMessage, useAsync } from '../kit';
import { README_URL } from '../landing/landing-model';
import { qrPath } from './qr';
import { supportAddressOf, titleOfRank } from './support-model';

/** Screen pixels per module: a whole number keeps the modules sharp. */
const QR_SCALE = 4;

/** Fixed black on white in both themes, the quiet zone included: a wallet reads it on a dark page too. */
function QrCode(props: { text: string }) {
  const { size, path } = useMemo(() => qrPath(props.text), [props.text]);
  return (
    <svg class="support-qr" width={size * QR_SCALE} height={size * QR_SCALE} viewBox={`0 0 ${size} ${size}`} shape-rendering="crispEdges" role="img" aria-label={t('support.qr')} data-testid="support-qr">
      <rect width={size} height={size} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}

function ContributorRows(props: { data: Contributions | undefined }) {
  const rows = props.data?.contributors;
  if (!rows) return <TableMessage colSpan={5}>{t('support.contributors.loading')}</TableMessage>;
  if (rows.length === 0) return <TableMessage colSpan={5} data-testid="support-contributors-empty">{t('support.contributors.empty')}</TableMessage>;
  return (
    <>
      {rows.map((c, i) => (
        <tr key={c.address} data-testid="support-contributor">
          <td class="num">{i + 1}</td>
          <td>{titleOfRank(i + 1) ?? ''}</td>
          <td>
            <CopyText value={c.address} text={shortAddress(c.address)} />
          </td>
          <td>{formatDateTime(Math.floor(c.first / 1000), { timeStyle: undefined })}</td>
          <td class="right num">{formatKas(c.total, { group: ',', maxFraction: 2 })}</td>
        </tr>
      ))}
    </>
  );
}

function Contributors(props: { address: string }) {
  const { data, error } = useAsync((signal) => readContributions(props.address, { signal }), [props.address]);
  return (
    <section data-testid="support-contributors">
      <h2>{t('support.contributors.title')}</h2>
      {error ? (
        <p class="muted" data-testid="support-contributors-failed">{t('support.contributors.failed')}</p>
      ) : (
        <>
          <Table dense data-testid="support-contributors-table">
            <thead>
              <tr>
                <th>{t('support.contributors.rank')}</th>
                <th>{t('support.contributors.titleColumn')}</th>
                <th>{t('support.contributors.address')}</th>
                <th>{t('support.contributors.first')}</th>
                <th class="right">{t('support.contributors.total')}</th>
              </tr>
            </thead>
            <tbody>
              <ContributorRows data={data} />
            </tbody>
          </Table>
          {data && !data.complete ? <p class="muted small" data-testid="support-contributors-partial">{t('support.contributors.partial')}</p> : null}
        </>
      )}
    </section>
  );
}

export function SupportPage() {
  const { config } = useServices();
  const address = supportAddressOf(config.features);
  if (!address) return null;
  return (
    <article class="legal" data-testid="support-page">
      <h1>{t('support.title')}</h1>
      <p data-testid="support-volunteers">{t('support.volunteers')}</p>
      <section data-testid="support-donate">
        <h2>{t('support.donate.title')}</h2>
        <div class="support-pay">
          <QrCode text={address} />
          <CopyText class="support-address" value={address} short={false} label={t('support.copy')} data-testid="support-address" />
        </div>
        <p data-testid="support-network"><strong>{t('support.network')}</strong></p>
        <p data-testid="support-terms">{t('support.terms')}</p>
        <p class="muted small">
          {t('support.public')}{' '}
          <a href={README_URL}>{t('support.readme')}</a>
        </p>
      </section>
      <Contributors address={address} />
    </article>
  );
}
