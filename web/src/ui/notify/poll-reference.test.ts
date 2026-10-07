// The reference of a watched invoice comes from the invoice server: it is stored and shown with control and bidirectional characters
// replaced and at most 80 characters.
import { describe, expect, it } from 'vitest';
import { NotificationStore } from '../../kob/notifications';
import { pollInvoices } from './poll';

describe('invoice references from the invoice server', () => {
  it('are sanitised and bounded before they are stored', async () => {
    const store = new NotificationStore(null, 'mainnet', 'pk');
    store.addInvoice({ url: 'https://pay.example/invoices/i', id: 'i', reference: null, status: null, addedAt: 1 });
    const reference = `order‮gnp.exe\u0007 ${'x'.repeat(200)}`;
    const fetchFn = async () => new Response(JSON.stringify({ status: 'paid', reference }), { status: 200 });
    await pollInvoices(store, 2, fetchFn);
    const stored = store.invoices()[0]!.reference!;
    expect(stored).not.toMatch(/[‮\u0007]/);
    expect([...stored].length).toBeLessThanOrEqual(81);
    expect(stored.startsWith('order')).toBe(true);
  });
});
