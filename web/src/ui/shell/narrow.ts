// Whether the page is narrow (a phone or a small tablet): the header is one row in both layouts, but the narrow one folds the tabs, the theme toggle and
// the status into a menu and offers one "Connect" button. The width is the one the desktop grid starts at (styles.css `@media (min-width: 900px)`).
import { useEffect, useState } from 'preact/hooks';

export const NARROW_QUERY = '(max-width: 899px)';

const narrowNow = (): boolean => typeof matchMedia === 'function' && matchMedia(NARROW_QUERY).matches;

/** True while the window is narrower than 900 px; follows a resize or a rotation. */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(narrowNow);
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const q = matchMedia(NARROW_QUERY);
    const on = () => setNarrow(q.matches);
    on();
    q.addEventListener('change', on);
    return () => q.removeEventListener('change', on);
  }, []);
  return narrow;
}
