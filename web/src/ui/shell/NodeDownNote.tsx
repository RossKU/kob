// The one-line note for a dead node (review M-13): the market page and the ticket look normal while the Kaspa node is unreachable, and nothing can be sent. The
// bell and the status dot say it already ("never only on the page"); this is the inline cue at the place it matters, next to the order button, with the way to
// try again. It is a line, not a banner: no box, no colour field, no dismiss. It reads the node status the header already polls (StatusProvider), so it adds no
// request. The ticket disables its button with `useNodeDown()`.
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { Button } from '../kit';
import { nodeIsDown } from './status';
import { useSystemStatus } from './StatusProvider';

/** True while the Kaspa node is unreachable (the status the header polls): the order button is disabled then. */
export function useNodeDown(): boolean {
  return nodeIsDown(useSystemStatus().node);
}

/**
 * "No connection to a Kaspa node: orders cannot be sent." with a Retry that reads the node again. Renders nothing while the node is fine or not yet known.
 * Usage: `<NodeDownNote />` under or beside the order button; `class` adds a class to the line.
 */
export function NodeDownNote(props: { class?: string; 'data-testid'?: string }) {
  const { node } = useSystemStatus();
  // a press waits for the next answer of the node (a read already in flight counts: it is fresh too)
  const [since, setSince] = useState(0);
  if (!nodeIsDown(node)) return null;
  const retrying = since > 0 && node.checkedAt < since;
  return (
    <p class={`node-down-note${props.class ? ` ${props.class}` : ''}`} role="status" data-testid={props['data-testid'] ?? 'node-down-note'}>
      <span>{t('shell.nodeDown.note')}</span>
      <Button
        small
        variant="ghost"
        class="node-down-retry"
        loading={retrying}
        onClick={() => {
          setSince(Date.now());
          node.refresh();
        }}
        data-testid="node-down-retry"
      >
        {t('common.retry')}
      </Button>
    </p>
  );
}
