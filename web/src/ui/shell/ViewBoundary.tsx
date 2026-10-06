import { Component, type ComponentChildren } from 'preact';
import { t } from '../../i18n';
import { Banner, Button } from '../kit';

interface State { error: Error | null }

/** Catches a render error of one view so the header, status bar and navigation stay usable; `resetKey` (the route) clears the error. */
export class ViewBoundary extends Component<{ children?: ComponentChildren; resetKey: string }, State> {
  state: State = { error: null };
  private lastKey = this.props.resetKey;

  static getDerivedStateFromError(error: unknown): State {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: unknown): void {
    // keep the trace in the console for bug reports; the page shows a readable message
    console.error('view crashed', error);
  }

  componentDidUpdate(): void {
    if (this.props.resetKey !== this.lastKey) {
      this.lastKey = this.props.resetKey;
      if (this.state.error) this.setState({ error: null });
    }
  }

  render() {
    if (this.state.error) {
      return (
        <Banner
          tone="error"
          data-testid="view-crashed"
          actions={<Button small onClick={() => this.setState({ error: null })}>{t('common.retry')}</Button>}
        >
          {t('shell.view.error', { message: this.state.error.message })}
        </Banner>
      );
    }
    return this.props.children;
  }
}
