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
