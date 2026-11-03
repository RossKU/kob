// What a token page says about a token's PROGRAM and DECIMALS where no registry entry does (a token only the indexer, the wallet or kascov.io names),
// as the tooltip of the token-family chip: the token program as KOB pins it (matched by template hash, so a name here is KOB's own table, never a claim of a third
// party) with its review status, and where the decimals come from, by the rules the whole app uses (declaration, the program's default such as KaspaRocket's 3,
// else the first order names them). Plus the link to the token on kascov.io for a row kascov.io named. Pure facts and labels: nothing here is a rating.
import { useServices } from '../../app/context';
import { decimalsOrigin } from '../../app/token-display';
import { kascovNetwork, kascovTokenUrl } from '../../data/kascov';
import { t } from '../../i18n';
import { TOKEN_PROGRAMS, programDefaultDecimals } from '../../kob/programs.generated';
import type { TokenRow } from './token-model';

/**
 * The "Program" row of a token's details, for EVERY token: which token program it runs and how that was established, as one neutral fact line (no colour, no
 * badge; the risk policy lives in token-model.ts `tokenRiskCodes` and is untouched). `name` is the program as this build knows it (`KCC-20 (KOB reference, 3x3)`,
 * `KCC-20 (KaspaCom 0.2.5)`) or "unknown program"; `basis` says how it was established:
 *   genesis       KOB's own genesis check passed (the program is the one the check decoded from the token's creation)
 *   genesisFailed the genesis check found a problem (the program is still read from the template hash, when there is one)
 *   matched       a registry token whose template matches the program pinned in this build
 *   differs       a registry token whose template does not match the pinned program (or names none this build has)
 *   hash          the template hash an indexer or kascov.io names is the one of a program in this build's table; the genesis was not checked
 * `basis` is null when nothing is known.
 */
export interface ProgramFact {
  name: string;
  basis: 'genesis' | 'genesisFailed' | 'matched' | 'differs' | 'hash' | null;
}

type ProgramRow = Pick<TokenRow, 'info' | 'index' | 'genesis' | 'source'>;

export function programFact(row: ProgramRow): ProgramFact {
  const g = row.genesis;
  const program = (g?.status === 'verified' ? (TOKEN_PROGRAMS.find((p) => p.name === g.program) ?? null) : null) ?? programOfRow(row);
  if (!program) {
    // a registry template this build has no program for: the family and the registry's id for it are all that is known
    const info = row.info;
    if (info && info.openList !== true) return { name: t('market.program.nameOfTemplate', { family: t(`market.badge.family.${info.family}`), template: info.templateId }), basis: 'differs' };
    return { name: t('market.program.unknown'), basis: null };
  }
  const name = t(`market.program.name.${program.name}`);
  if (g?.status === 'verified') return { name, basis: 'genesis' };
  if (g?.status === 'failed') return { name, basis: 'genesisFailed' };
  if (row.source === 'registry' && row.info) return { name, basis: row.info.templateMatchesPinned ? 'matched' : 'differs' };
  return { name, basis: 'hash' };
}

/** `KCC-20 (KOB reference, 3x3) · checked from its genesis`: the text of the Program row. */
export function programFactText(row: ProgramRow): string {
  const f = programFact(row);
  return f.basis ? `${f.name} · ${t(`market.program.basis.${f.basis}`)}` : f.name;
}

/** The pinned program a row names: its verified program (`info`), else the one its template hash names in this build's table; null when none. */
export function programOfRow(row: Pick<TokenRow, 'info' | 'index'>): (typeof TOKEN_PROGRAMS)[number] | null {
  const name = row.info?.program ?? null;
  const hash = row.info?.templateHash || row.index?.template_hash || null;
  return TOKEN_PROGRAMS.find((p) => (name !== null && p.name === name) || (hash !== null && p.templateHash === hash)) ?? null;
}

/** `reviewed`, `pending review`, `test only`: the review status of a program in this build's table. */
export const reviewText = (review: string): string => t(`market.program.review.${review}`);

/** Where the decimals of a row not yet opened for trading come from: the sentence, or the one saying they are not known yet. */
export function decimalsLine(row: Pick<TokenRow, 'declared' | 'decimals' | 'info' | 'index'>): string {
  const program = programOfRow(row);
  if (row.declared && row.decimals !== null) return t('market.open.decimals.declared', { decimals: row.decimals });
  const convention = programDefaultDecimals(program?.name);
  if (convention) return t('market.open.decimals.program', { decimals: convention.decimals, source: convention.source });
  return t('market.program.decimalsUnknown');
}

/**
 * What a token page says about a token's program and decimals where no registry entry does, as the tooltip text of the token-family chip (no box, no line
 * of its own): the program as KOB pins it with its review status, and where the decimals (and the price step) come from.
 */
export function programTitle(row: TokenRow): string | null {
  const lines: string[] = [];
  if (row.source !== 'registry') {
    const program = programOfRow(row);
    if (program) lines.push(t('market.program.line', { program: program.name, review: reviewText(program.review) }));
    if (!row.info) lines.push(decimalsLine(row));
  }
  const info = row.info;
  if (info && info.openList === true) lines.push(t(`market.open.decimals.${decimalsOrigin(info)}`, { decimals: info.decimals ?? 8, source: info.decimalsConvention ?? '' }));
  return lines.length ? lines.join('\n') : null;
}

/** The link to the token on kascov.io, for a row kascov.io named (a new tab, no referrer). */
export function KascovLink(props: { row: Pick<TokenRow, 'covenantId' | 'kascov'> }) {
  const { config } = useServices();
  const net = kascovNetwork(config.network);
  if (!props.row.kascov || !net) return null;
  const href = kascovTokenUrl(net, props.row.covenantId, config.tokenNamesUrl);
  if (!href) return null;
  return (
    <a class="small" href={href} target="_blank" rel="noopener noreferrer" data-testid="kascov-link" title={t('market.kascov.linkTitle')}>
      {t('market.kascov.link')}
    </a>
  );
}
