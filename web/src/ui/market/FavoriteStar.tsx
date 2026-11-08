import { t } from '../../i18n';

/** A star that marks a token or a pair as a favourite (favorites.ts). `name`: what it is about, for the title and the screen reader. */
export function FavoriteStar(props: { on: boolean; name: string; onToggle: () => void; 'data-testid': string }) {
  const what = t(props.on ? 'market.favorite.remove' : 'market.favorite.add', { name: props.name });
  return (
    <button type="button" class="fav-star" aria-pressed={props.on ? 'true' : 'false'} aria-label={what} title={what} onClick={props.onToggle} data-testid={props['data-testid']}>
      <svg viewBox="0 0 24 24" width="18" height="18" fill={props.on ? 'currentColor' : 'none'} stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" aria-hidden="true" focusable="false">
        <path d="M12 3.4l2.66 5.39 5.95.87-4.3 4.2 1.01 5.92L12 17l-5.32 2.78 1.01-5.92-4.3-4.2 5.95-.87z" />
      </svg>
    </button>
  );
}

/** The eye beside the star: hides a token from the market list in this browser, or (pressed, in the Hidden view) shows it there again (hidden-tokens.ts). */
export function HideToggle(props: { hidden: boolean; name: string; onToggle: () => void; 'data-testid': string }) {
  const what = t(props.hidden ? 'market.hide.undo' : 'market.hide.add', { name: props.name });
  return (
    <button type="button" class="fav-star hide-btn" aria-pressed={props.hidden ? 'true' : 'false'} aria-label={what} title={what} onClick={props.onToggle} data-testid={props['data-testid']}>
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
        <path d="M3 3l18 18" />
        <path d="M10.6 5.1A10.6 10.6 0 0 1 12 5c5 0 8.6 4.3 9.7 7a13.4 13.4 0 0 1-3 4.2M6.6 6.6C4.4 8 2.9 10.1 2.3 12c1.1 2.7 4.7 7 9.7 7a9.9 9.9 0 0 0 5.3-1.5" />
        <path d="M9.9 9.9a3 3 0 0 0 4.2 4.2" />
      </svg>
    </button>
  );
}
