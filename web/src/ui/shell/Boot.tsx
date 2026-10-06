import { SETTINGS_KEY } from '../../config';
import { t } from '../../i18n';
import { Button, Spinner } from '../kit';

/** Shown while the configuration, the wasm engine, the SDK and the registry load. */
export function BootScreen() {
  return (
    <div class="boot" data-testid="boot-screen" role="status">
      <h1>{t('common.appName')}</h1>
      <p class="muted">{t('common.tagline')}</p>
      <Spinner large />
      <p>{t('shell.boot.title')}</p>
    </div>
  );
}

const errorText = (e: unknown): string => {
  if (e instanceof Error) return `${e.name}: ${e.message}${e.stack ? `\n\n${e.stack}` : ''}`;
  return String(e);
};

/** Clear boot failure page: what happened, technical details, retry, and a reset of the saved settings (a bad setting can prevent a start). */
export function BootError(props: { error: unknown; onRetry: () => void }) {
  const resetSettings = () => {
    try {
      localStorage.removeItem(SETTINGS_KEY);
    } catch {
      /* storage blocked: retry anyway */
    }
    props.onRetry();
  };
  return (
    <div class="boot" data-testid="boot-error">
      <div class="section boot-card">
        <h1>{t('shell.boot.errorTitle')}</h1>
        <p>{t('shell.boot.errorBody')}</p>
        <details>
          <summary>{t('shell.boot.errorDetails')}</summary>
          <pre data-testid="boot-error-details">{errorText(props.error)}</pre>
        </details>
        <div class="row" style="margin-top:12px">
          <Button variant="primary" onClick={props.onRetry} data-testid="boot-retry">{t('common.retry')}</Button>
          <Button onClick={resetSettings} data-testid="boot-reset">{t('shell.boot.reset')}</Button>
        </div>
        <p class="small muted" style="margin-top:8px">{t('shell.boot.resetHint')}</p>
      </div>
    </div>
  );
}
