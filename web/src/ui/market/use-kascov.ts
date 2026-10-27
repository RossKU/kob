// The token names and icons list as component state (data/kascov.ts). A view that needs names or icons (the market list, a token page) calls this; the
// refresh policy lives here and in the client:
//   * fetch when the view is shown; while it stays visible, again only when the list is older than `KASCOV_REFRESH_MS` (2 minutes);
//   * nothing while the tab is hidden (the timer stops; coming back fetches only if the list is stale) and nothing after the view is left;
//   * every view shares one request and one in-memory cache (the client); a failure backs off up to 10 minutes and keeps the list shown so far;
//   * a list kascov did not rebuild (`generated_at_ms` unchanged) is the same object: the view does not render again.
import { useEffect, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { kascov, kascovNetwork, type KascovClient, type KascovResult, type KascovToken } from '../../data/kascov';

export type KascovState =
  | { status: 'off' }
  | { status: 'loading' }
  | { status: 'unavailable' }
  | { status: 'ok'; tokens: readonly KascovToken[]; result: KascovResult };

const OFF: KascovState = { status: 'off' };
const LOADING: KascovState = { status: 'loading' };
const UNAVAILABLE: KascovState = { status: 'unavailable' };

/** `base` empty or a network kascov does not serve: `off` (no request is ever made). */
export function useKascov(client: KascovClient = kascov): KascovState {
  const { config } = useServices();
  const net = kascovNetwork(config.network);
  const base = config.tokenNamesUrl;
  const enabled = net !== null && base !== '';
  const [state, setState] = useState<KascovState>(enabled ? LOADING : OFF);

  useEffect(() => {
    if (!enabled) {
      setState(OFF);
      return;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };
    const run = async () => {
      stop();
      if (!live || document.hidden) return;
      const r = await client.load(net, { base });
      if (!live) return;
      setState((prev) => (r.ok ? (prev.status === 'ok' && prev.result === r ? prev : { status: 'ok', tokens: r.tokens, result: r }) : prev.status === 'ok' ? prev : UNAVAILABLE));
      if (!document.hidden) timer = setTimeout(() => void run(), client.nextInMs(net, base));
    };
    const onVisibility = () => (document.hidden ? stop() : void run());
    document.addEventListener('visibilitychange', onVisibility);
    void run();
    return () => {
      live = false;
      stop();
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [client, enabled, net, base]);

  return state;
}
