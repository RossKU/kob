// The ticket's "?": it shows and hides the help lines under the fields. It flashes once when the
// order being entered turns dust (dust-hint.ts), because its help then says why.
import { t } from '../../i18n';
import { dustFlashClass, type DustFlash } from './dust-hint';

export function HelpToggle(props: { helpOn: boolean; onClick: () => void; flash: DustFlash }) {
  return (
    <button
      type="button"
      class={`tk-help-toggle${dustFlashClass(props.flash)}`}
      aria-pressed={props.helpOn ? 'true' : 'false'}
      title={t(props.helpOn ? 'ticket.helpToggle.hide' : 'ticket.helpToggle.show')}
      aria-label={t('ticket.helpToggle.label')}
      data-testid="order-help-toggle"
      onClick={props.onClick}
    >
      ?
    </button>
  );
}
