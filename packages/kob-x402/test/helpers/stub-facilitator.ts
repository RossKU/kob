// An in-process stub facilitator: GET /supported, POST /verify, POST /settle with scripted answers.

import { createServer } from 'node:http';
import type { Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import type { FacilitatorRequest, SettlementResponse } from '../../src/types.ts';

export interface RecordedCall {
  method: string;
  path: string;
  headers: Record<string, string | string[] | undefined>;
  body: unknown;
}

export type SettleScript = (call: number, body: FacilitatorRequest) => { status?: number; body?: unknown; drop?: boolean; delayMs?: number } | undefined;

export interface StubFacilitator {
  url: string;
  calls: RecordedCall[];
  settleCalls(): RecordedCall[];
  close(): Promise<void>;
}

/** Success settlement derived from the payload the way the real facilitator would answer. */
export function defaultSettlement(req: FacilitatorRequest): SettlementResponse {
  const tx = JSON.parse(req.paymentPayload.payload.transaction) as { id: string };
  return {
    success: true,
    transaction: tx.id,
    network: req.paymentRequirements.network,
    payer: req.paymentPayload.payload.payerAddress ?? '',
    amount: req.paymentRequirements.amount,
    extensions: { kaspa: { exactProfile: req.paymentPayload.payload.profile, paymentOutputIndex: 0, finality: 'accepted', requestHash: req.requestHash } },
  };
}

export function failure(diagnostic: string, retryable: boolean, message: string, details?: unknown, reason = 'invalid_transaction_state'): SettlementResponse {
  return {
    success: false,
    errorReason: reason,
    transaction: '',
    extensions: { kaspa: { diagnostic, retryable, message, ...(details === undefined ? {} : { details }) } },
  };
}

export async function startStubFacilitator(opts: { apiKey?: string; settle?: SettleScript } = {}): Promise<StubFacilitator> {
  const calls: RecordedCall[] = [];
  let settleN = 0;
  const server: Server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => {
      void (async () => {
        const text = Buffer.concat(chunks).toString('utf8');
        const body: unknown = text ? JSON.parse(text) : undefined;
        calls.push({ method: req.method ?? '', path: req.url ?? '', headers: req.headers, body });
        const send = (status: number, b: unknown): void => {
          res.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(b));
        };
        if (opts.apiKey && req.headers.authorization !== `Bearer ${opts.apiKey}`) return send(401, { error: 'unauthorized' });
        if (req.method === 'GET' && req.url === '/supported') {
          return send(200, { kinds: [{ x402Version: 2, scheme: 'exact', network: 'kaspa:testnet-10' }], extensions: ['payment-identifier'], signers: {} });
        }
        if (req.method === 'POST' && req.url === '/verify') return send(200, { isValid: true });
        if (req.method === 'POST' && req.url === '/settle') {
          const fr = body as FacilitatorRequest;
          const scripted = opts.settle?.(++settleN, fr);
          if (scripted?.delayMs) await new Promise((r) => setTimeout(r, scripted.delayMs));
          if (scripted?.drop) return void req.socket.destroy();
          return send(scripted?.status ?? 200, scripted?.body ?? defaultSettlement(fr));
        }
        send(404, { error: 'not_found' });
      })();
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const port = (server.address() as AddressInfo).port;
  return {
    url: `http://127.0.0.1:${port}`,
    calls,
    settleCalls: () => calls.filter((c) => c.path === '/settle'),
    close: () => new Promise<void>((r) => (server.closeAllConnections(), server.close(() => r()))),
  };
}
