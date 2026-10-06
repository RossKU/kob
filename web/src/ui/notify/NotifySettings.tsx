// Settings section "Notifications": in-app on/off, browser notifications on/off (the browser permission is requested here, from the click), per-kind switches.
import { useState } from 'preact/hooks';
import { t } from '../../i18n';
import { NOTIFY_KINDS } from '../../kob/notifications';
import { Banner, Checkbox, Section } from '../kit';
import { useNotify, type BrowserPermission } from './NotifyProvider';

export function NotifySettingsSection() {
  const n = useNotify();
  const [perm, setPerm] = useState<BrowserPermission>(() => n.permission());
  const s = n.settings;

  const toggleBrowser = async (on: boolean) => {
    if (!on) {
      n.update({ browser: false });
      return;
    }
    const p = await n.requestBrowser();
    setPerm(p);
    n.update({ browser: p === 'granted' });
  };

  return (
    <Section title={t('notify.settings.title')} data-testid="settings-notify">
      <div class="stack-sm">
        <p class="muted">{t('notify.settings.intro')}</p>
        <Checkbox checked={s.enabled} onChange={(c) => n.update({ enabled: c })} label={t('notify.settings.enable')} hint={t('notify.settings.enable.hint')} data-testid="notify-enable" />
        <Checkbox
          checked={s.browser && perm === 'granted'}
          onChange={(c) => void toggleBrowser(c)}
          disabled={perm === 'unsupported'}
          label={t('notify.settings.browser')}
          hint={t('notify.settings.browser.hint')}
          data-testid="notify-browser"
        />
        {perm === 'denied' ? <Banner tone="warn" data-testid="notify-browser-denied">{t('notify.settings.browser.denied')}</Banner> : null}
        {perm === 'unsupported' ? <Banner tone="info" data-testid="notify-browser-unsupported">{t('notify.settings.browser.unsupported')}</Banner> : null}
        <strong>{t('notify.settings.kinds')}</strong>
        {NOTIFY_KINDS.map((k) => (
          <Checkbox key={k} checked={s.kinds[k]} onChange={(c) => n.update({ kinds: { [k]: c } })} label={t(`notify.kind.${k}`)} data-testid={`notify-kind-${k}`} />
        ))}
      </div>
    </Section>
  );
}
