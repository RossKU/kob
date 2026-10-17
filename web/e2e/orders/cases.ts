// The order-type matrix: one entry per order type and side of docs/spec/order-types.md, with what the final confirmation popup, the accepted transaction
// (indexer state fields, locked value) and My orders must show. Every expected figure follows from the typed inputs and
// the protocol rules (prices in KAS per EXKCC; amounts in EXKCC, 1 EXKCC = 1e8 base units = the order scale; the default seed rests around 0.025 KAS
// with bids <= 0.0249 and asks >= 0.0251;
// e.g. a 3% market bound is 0.0249 x 0.97 = 0.024153, a sell-first IFO prefund is stop-worst 0.02781 - entry 0.026 = 0.00181): a wrong number in
// the UI fails the test.
//
// Price fields of the on-chain state are sompi per whole token (per `scale` = 1e8 base units): 0.027 KAS / EXKCC = 2_700_000; amounts are base
// units. All wallet defaults come from docs/spec/order-types.md and kob-wasm `defaultConstants` (tip 0, carrier 2 KAS per covenant UTXO, market 3%
// over 20 s, stop 3% band over 30 s, rest time R 5 s, trigger threshold = the minimum fill, keeper tip 0.022 KAS on the 8/8 program, refund tip
// 0.049 KAS). Minimum fill defaults (matcher.md 10): a resting order the amount worth 10 KAS (a notional, not a carrier) at its price, capped at the whole
// amount (so every 2-token order here is filled whole: ONE delivery carrier of a bid); IOC / FOK / market 1 base unit; an IFD entry a quarter of
// the amount (four fills: four delivery carriers and four prefunded exit carriers).
import type { FieldValues, Side } from '../helpers/ticket';

/** What a case may need from the browser / mock clock to type dates. */
export interface Ctx {
  /** `Date.getTimezoneOffset()` of the browser */
  tz: number;
  /** the mock node's UTC unix seconds when the test started */
  unix: number;
}

export type Timing =
  | { kind: 'gtc' }
  | { kind: 'day' }
  | { kind: 'gtd'; afterSeconds: number }
  | { kind: 'timed'; afterSeconds: number }
  | { kind: 'ioc' }
  | { kind: 'auction' }
  | { kind: 'none' };

export interface ExitExpect {
  /** kind and fields of the exit state committed inside the entry (decoded with kob-wasm) */
  state: { kind: string; fields: Record<string, string> };
}

export interface OrderCase {
  name: string;
  type: string;
  side: Side;
  fields: (c: Ctx) => FieldValues;
  /** covenant the transaction creates */
  kind: string;
  /** My orders type key (`data-type`) and its English label */
  listType: string;
  listLabel: string;
  /**
   * The final confirmation (the signer's table), read before it is confirmed: `orderTitle` = the title of the one order it lists; `lines` = the token
   * lines of its summary (the net KAS and "Locked in orders" are checked against the accepted transaction); `card` = the rows of that order's table
   * (every row except the DAA times, which are compared with the on-chain state); `exitTitle` / `exitCard` = the exit an if-done entry commits to.
   */
  receipt: { orderTitle: string; lines?: Record<string, string>; card: Record<string, string>; exitTitle?: string; exitCard?: Record<string, string> };
  /** the KAS the accepted transaction locks in contracts (order output + custody carriers), summed from its outputs */
  locked: string;
  escrowed: string | null;
  /** the exit an if-done entry commits to */
  exit?: ExitExpect;
  /** exact fields of the state the indexer holds after acceptance (the on-chain facts: price, tif, tip, refund tip, carriers, slippage) */
  state: Record<string, string>;
  timing: Timing;
}

// ------------------------------------------------------------------------------------------------ limit family
const limit = (side: Side): Pick<OrderCase, 'type' | 'side' | 'kind' | 'listType' | 'listLabel'> => ({
  type: 'limit', side, kind: side === 'sell' ? 'KobAsk' : 'KobBid', listType: 'limit', listLabel: 'Limit (GTC)',
});

const SELL = { locked: '4 KAS', escrowed: '2 EXKCC' } as const;
/** a 2-token resting buy at price p (KAS per token): escrow 2p, one delivery carrier (the default minimum fill is the whole amount) */
const buyLimit = (escrowKas: string) => ({ locked: `${(2 + Number(escrowKas)).toFixed(3).replace(/0+$/, '').replace(/\.$/, '')} KAS`, escrowed: null });

export const CASES: OrderCase[] = [
  // ---- limit GTC
  {
    name: 'limit GTC sell', ...limit('sell'), ...SELL, fields: () => ({ amount: '2', price: '0.027' }),
    receipt: {
      orderTitle: 'Sell limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', tip: '0', tif: '0', activeFrom: '0', amountLeft: '200000000', refundTip: '4900000', scale: '100000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit GTC buy', ...limit('buy'), ...buyLimit('0.049'), fields: () => ({ amount: '2', price: '0.0245' }),
    receipt: {
      orderTitle: 'Buy limit order',
      card: { 'KAS budget': '0 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.049 KAS' },
    },
    state: { price: '2450000', tip: '0', tif: '0', activeFrom: '0', reserve: '0', deliveryCarrier: '200000000', refundTip: '4900000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit sell with a priority tip: all-in is limit minus tip', ...limit('sell'), ...SELL, fields: () => ({ amount: '2', price: '0.027', tip: '0.001' }),
    receipt: {
      orderTitle: 'Sell limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.026 EXKCC / KAS', 'Priority tip': '0.001 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', tip: '100000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'limit buy with a priority tip: all-in is limit plus tip', ...limit('buy'), ...buyLimit('0.051'), fields: () => ({ amount: '2', price: '0.0245', tip: '0.001' }),
    receipt: {
      orderTitle: 'Buy limit order',
      card: { 'KAS budget': '0.002 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0255 EXKCC / KAS', 'Priority tip': '0.001 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.051 KAS' },
    },
    state: { price: '2450000', tip: '100000' },
    timing: { kind: 'gtc' },
  },
  // ---- limit GTD (date)
  {
    name: 'limit GTD sell', ...limit('sell'), listType: 'limit-gtd', listLabel: 'Limit (until date)', ...SELL,
    fields: (c) => ({ amount: '2', price: '0.027', lifetime: 'gtd', lifetimeAt: localAfter(c, 3 * 86_400) }),
    receipt: {
      orderTitle: 'Sell limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', tif: '0', amountLeft: '200000000' },
    timing: { kind: 'gtd', afterSeconds: 3 * 86_400 },
  },
  {
    name: 'limit GTD buy', ...limit('buy'), listType: 'limit-gtd', listLabel: 'Limit (until date)', ...buyLimit('0.049'),
    fields: (c) => ({ amount: '2', price: '0.0245', lifetime: 'gtd', lifetimeAt: localAfter(c, 3 * 86_400) }),
    receipt: {
      orderTitle: 'Buy limit order',
      card: { 'KAS budget': '0 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.049 KAS' },
    },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'gtd', afterSeconds: 3 * 86_400 },
  },
  // ---- limit day (until 00:00 UTC)
  {
    name: 'limit day sell (ends 00:00 UTC)', ...limit('sell'), listType: 'limit-day', listLabel: 'Limit (day order)', ...SELL,
    fields: () => ({ amount: '2', price: '0.027', lifetime: 'day' }),
    receipt: {
      orderTitle: 'Sell day limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', tif: '0' },
    timing: { kind: 'day' },
  },
  {
    name: 'limit day buy (ends 00:00 UTC)', ...limit('buy'), listType: 'limit-day', listLabel: 'Limit (day order)', ...buyLimit('0.049'),
    fields: () => ({ amount: '2', price: '0.0245', lifetime: 'day' }),
    receipt: {
      orderTitle: 'Buy day limit order',
      card: { 'KAS budget': '0 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.049 KAS' },
    },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'day' },
  },
  // ---- timed activation
  {
    name: 'limit with timed activation, sell', ...limit('sell'), listType: 'timed', listLabel: 'Limit (timed start)', ...SELL,
    fields: (c) => ({ amount: '2', price: '0.027', activeFrom: localAfter(c, 2 * 3600) }),
    receipt: {
      orderTitle: 'Sell limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', tif: '0' },
    timing: { kind: 'timed', afterSeconds: 2 * 3600 },
  },
  {
    name: 'limit with timed activation, buy', ...limit('buy'), listType: 'timed', listLabel: 'Limit (timed start)', ...buyLimit('0.049'),
    fields: (c) => ({ amount: '2', price: '0.0245', activeFrom: localAfter(c, 2 * 3600) }),
    receipt: {
      orderTitle: 'Buy limit order',
      card: { 'KAS budget': '0 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.049 KAS' },
    },
    state: { price: '2450000', tif: '0' },
    timing: { kind: 'timed', afterSeconds: 2 * 3600 },
  },
  // ---- IOC / FOK
  {
    name: 'IOC sell', type: 'ioc', side: 'sell', kind: 'KobAsk', listType: 'ioc', listLabel: 'IOC', ...SELL,
    fields: () => ({ amount: '2', price: '0.0245' }),
    receipt: {
      orderTitle: 'Sell IOC limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You receive at least per token (all-in)': '0.0245 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2450000', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'IOC buy', type: 'ioc', side: 'buy', kind: 'KobBid', listType: 'ioc', listLabel: 'IOC', locked: '2.053 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0265' }),
    receipt: {
      orderTitle: 'Buy IOC limit order',
      card: { 'KAS budget': '0.004 KAS', 'Minimum fill': '0.00000001 EXKCC', 'Limit price': '0.0265 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0265 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.053 KAS' },
    },
    state: { price: '2650000', tif: '1', deliveryCarrier: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'FOK sell', type: 'fok', side: 'sell', kind: 'KobAsk', listType: 'fok', listLabel: 'FOK', ...SELL,
    fields: () => ({ amount: '2', price: '0.0245' }),
    receipt: {
      orderTitle: 'Sell FOK limit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You receive at least per token (all-in)': '0.0245 EXKCC / KAS', 'Time in force': 'fill or kill: all or nothing', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2450000', tif: '2', amountLeft: '200000000' },
    timing: { kind: 'ioc' },
  },
  {
    name: 'FOK buy', type: 'fok', side: 'buy', kind: 'KobBid', listType: 'fok', listLabel: 'FOK', locked: '2.053 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0265' }),
    receipt: {
      orderTitle: 'Buy FOK limit order',
      card: { 'KAS budget': '0.004 KAS', 'Minimum fill': '0.00000001 EXKCC', 'Limit price': '0.0265 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0265 EXKCC / KAS', 'Time in force': 'fill or kill: all or nothing', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.053 KAS' },
    },
    state: { price: '2650000', tif: '2' },
    timing: { kind: 'ioc' },
  },
  // ---- market (expected vs worst: the 3% slippage bound of the auction)
  {
    name: 'market sell: expected best bid, worst 3% below', type: 'market', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2' }),
    receipt: {
      orderTitle: 'Sell market order (auction)',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0249 EXKCC / KAS', 'Worst price (all-in): you receive at least per token': '0.024153 EXKCC / KAS', 'Auction ends at (worst price)': '0.024153 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2490000', priceEnd: '2415300', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'auction' },
  },
  {
    name: 'market buy: expected best ask, worst 3% above', type: 'market', side: 'buy', kind: 'KobBid', listType: 'market', listLabel: 'Market', locked: '2.051706 KAS', escrowed: null,
    fields: () => ({ amount: '2' }),
    receipt: {
      orderTitle: 'Buy market order (auction)',
      card: { 'KAS budget': '0.002706 KAS', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0251 EXKCC / KAS', 'Worst price (all-in): you pay at most per token': '0.025853 EXKCC / KAS', 'Auction ends at (worst price)': '0.025853 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.051706 KAS' },
    },
    state: { price: '2510000', priceEnd: '2585300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'market sell with a 1% slippage bound', type: 'market', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2', slippageBps: '1' }),
    receipt: {
      orderTitle: 'Sell market order (auction)',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0249 EXKCC / KAS', 'Worst price (all-in): you receive at least per token': '0.024651 EXKCC / KAS', 'Auction ends at (worst price)': '0.024651 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2490000', priceEnd: '2465100', tif: '1' },
    timing: { kind: 'auction' },
  },
  // ---- streaming (quote and execute from the displayed price)
  {
    name: 'streaming buy from the displayed price', type: 'streaming', side: 'buy', kind: 'KobBid', listType: 'market', listLabel: 'Market', locked: '2.051706 KAS', escrowed: null,
    fields: () => ({ amount: '2', displayedPrice: '0.0251' }),
    receipt: {
      orderTitle: 'Buy market order (auction)',
      card: { 'KAS budget': '0.002706 KAS', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0251 EXKCC / KAS', 'Worst price (all-in): you pay at most per token': '0.025853 EXKCC / KAS', 'Auction ends at (worst price)': '0.025853 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.051706 KAS' },
    },
    state: { price: '2510000', priceEnd: '2585300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'streaming sell from the displayed price', type: 'streaming', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2', displayedPrice: '0.0249' }),
    receipt: {
      orderTitle: 'Sell market order (auction)',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0249 EXKCC / KAS', 'Worst price (all-in): you receive at least per token': '0.024153 EXKCC / KAS', 'Auction ends at (worst price)': '0.024153 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2490000', priceEnd: '2415300', tif: '1' },
    timing: { kind: 'auction' },
  },
  {
    name: 'close (sell the held tokens at market)', type: 'close', side: 'sell', kind: 'KobAsk', listType: 'market', listLabel: 'Market', ...SELL,
    fields: () => ({ amount: '2' }),
    receipt: {
      orderTitle: 'Sell market order (auction)',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.00000001 EXKCC', 'Expected price (auction start)': '0.0249 EXKCC / KAS', 'Worst price (all-in): you receive at least per token': '0.024153 EXKCC / KAS', 'Auction ends at (worst price)': '0.024153 EXKCC / KAS', 'Time in force': 'immediate or cancel: the unfilled rest is returned', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2490000', priceEnd: '2415300', tif: '1', amountLeft: '200000000' },
    timing: { kind: 'auction' },
  },
  // ---- TWAP / DCA
  {
    name: 'TWAP: sell 6 EXKCC, at most 2 per 10 minutes', type: 'twap', side: 'sell', kind: 'KobAsk', listType: 'twap', listLabel: 'TWAP', locked: '4 KAS', escrowed: '6 EXKCC',
    fields: () => ({ amount: '6', sliceAmount: '2', interval: '10', price: '0.0245' }),
    receipt: {
      orderTitle: 'Sell TWAP schedule',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−6' },
      card: { 'Amount': '6 EXKCC', 'Minimum fill': '2 EXKCC', 'Limit price': '0.0245 EXKCC / KAS', 'You receive at least per token (all-in)': '0.0245 EXKCC / KAS', 'Rate limit': 'at most 2 EXKCC every 600 s', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2450000', interval: '6000', maxFill: '200000000', amountLeft: '600000000', tif: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'DCA: buy 6 EXKCC, at most 2 per 10 minutes', type: 'dca', side: 'buy', kind: 'KobBid', listType: 'dca', listLabel: 'DCA', locked: '6.15600002 KAS', escrowed: null,
    fields: () => ({ amount: '6', sliceAmount: '2', interval: '10', price: '0.026' }),
    receipt: {
      orderTitle: 'Buy DCA schedule',
      card: { 'KAS budget': '4.10700002 KAS', 'Minimum fill': '2 EXKCC', 'Limit price': '0.026 EXKCC / KAS', 'You pay at most per token (all-in)': '0.026 EXKCC / KAS', 'Rate limit': 'at most 2 EXKCC every 600 s', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '6.15600002 KAS' },
    },
    state: { price: '2600000', interval: '6000', maxFill: '200000000', tif: '0', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- Dutch / rising bid
  {
    name: 'Dutch sell: 0.027 falling to 0.0255 over 10 minutes', type: 'dutch', side: 'sell', kind: 'KobAsk', listType: 'dutch', listLabel: 'Dutch / price decay', ...SELL,
    fields: () => ({ amount: '2', price: '0.027', priceEnd: '0.0255', duration: '10' }),
    receipt: {
      orderTitle: 'Sell declining-price (Dutch) order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Expected price (auction start)': '0.027 EXKCC / KAS', 'Worst price (all-in): you receive at least per token': '0.0255 EXKCC / KAS', 'Auction ends at (worst price)': '0.0255 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { price: '2700000', priceEnd: '2550000', tif: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'rising bid: buy at 0.024 rising to 0.0255 over 10 minutes', type: 'dutch', side: 'buy', kind: 'KobBid', listType: 'dutch', listLabel: 'Dutch / price decay', locked: '2.051 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.024', priceEnd: '0.0255', duration: '10' }),
    receipt: {
      orderTitle: 'Buy rising-bid order',
      card: { 'KAS budget': '0.002 KAS', 'Minimum fill': '2 EXKCC', 'Expected price (auction start)': '0.024 EXKCC / KAS', 'Worst price (all-in): you pay at most per token': '0.0255 EXKCC / KAS', 'Auction ends at (worst price)': '0.0255 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.051 KAS' },
    },
    state: { price: '2400000', priceEnd: '2550000', tif: '0' },
    timing: { kind: 'gtc' },
  },
  // ---- stop-market / stop-limit / trailing
  {
    name: 'stop-market sell', type: 'stopMarket', side: 'sell', kind: 'KobCondAsk', listType: 'stop', listLabel: 'Stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023' }),
    receipt: {
      orderTitle: 'Sell stop order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { stopPrice: '2300000', tpPrice: '0', slipBps: '300', bandDaa: '300', minRestDaa: '50', armed: '0', keeperTip: '2200000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-market buy', type: 'stopMarket', side: 'buy', kind: 'KobCondBid', listType: 'stop', listLabel: 'Stop', locked: '2.07762 KAS', escrowed: null,
    fields: () => ({ amount: '2', stop: '0.027' }),
    receipt: {
      orderTitle: 'Buy stop order',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.027 EXKCC / KAS', 'Worst fill price after the trigger': '0.02781 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.07762 KAS' },
    },
    state: { stopPrice: '2700000', tpPrice: '0', slipBps: '300', bandDaa: '300', armed: '0', keeperTip: '2200000', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-limit sell: the band is the distance to the limit', type: 'stopLimit', side: 'sell', kind: 'KobCondAsk', listType: 'stop', listLabel: 'Stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023', limit: '0.0225' }),
    receipt: {
      orderTitle: 'Sell stop order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.0225009 EXKCC / KAS', 'Fill band': '2.17% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { stopPrice: '2300000', slipBps: '217', bandDaa: '300', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'stop-limit buy: the band is the distance to the limit', type: 'stopLimit', side: 'buy', kind: 'KobCondBid', listType: 'stop', listLabel: 'Stop', locked: '2.076999 KAS', escrowed: null,
    fields: () => ({ amount: '2', stop: '0.027', limit: '0.0275' }),
    receipt: {
      orderTitle: 'Buy stop order',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.027 EXKCC / KAS', 'Worst fill price after the trigger': '0.0274995 EXKCC / KAS', 'Fill band': '1.85% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.076999 KAS' },
    },
    state: { stopPrice: '2700000', slipBps: '185', bandDaa: '300', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'trailing stop sell', type: 'trailingStop', side: 'sell', kind: 'KobCondAsk', listType: 'trailing', listLabel: 'Trailing stop', ...SELL,
    fields: () => ({ amount: '2', stop: '0.023', 'trail.step': '0.0005', 'trail.gap': '0.001' }),
    receipt: {
      orderTitle: 'Sell trailing stop',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trailing': 'step 0.0005 KAS, gap 0.001 KAS, one update per 10 min at most', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { stopPrice: '2300000', trailStep: '50000', trailGap: '100000', trailWait: '6000', armed: '0', keeperTip: '2200000', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'trailing stop buy', type: 'trailingStop', side: 'buy', kind: 'KobCondBid', listType: 'trailing', listLabel: 'Trailing stop', locked: '2.51762 KAS', escrowed: null,
    fields: () => ({ amount: '2', stop: '0.027', 'trail.step': '0.0005', 'trail.gap': '0.001' }),
    receipt: {
      orderTitle: 'Buy trailing stop',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Stop trigger': '0.027 EXKCC / KAS', 'Worst fill price after the trigger': '0.02781 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trailing': 'step 0.0005 KAS, gap 0.001 KAS, one update per 10 min at most', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.51762 KAS' },
    },
    state: { stopPrice: '2700000', trailStep: '50000', trailGap: '100000', trailWait: '6000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- take-profit / OCO
  {
    name: 'take-profit sell', type: 'takeProfit', side: 'sell', kind: 'KobCondAsk', listType: 'take-profit', listLabel: 'Take-profit', ...SELL,
    fields: () => ({ amount: '2', price: '0.027' }),
    receipt: {
      orderTitle: 'Sell take-profit order',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Take-profit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { tpPrice: '2700000', stopPrice: '0', slipBps: '0', amountLeft: '200000000', keeperTip: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'take-profit buy', type: 'takeProfit', side: 'buy', kind: 'KobCondBid', listType: 'take-profit', listLabel: 'Take-profit', locked: '2.046 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.023' }),
    receipt: {
      orderTitle: 'Buy take-profit order',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Take-profit price': '0.023 EXKCC / KAS', 'You pay at most per token (all-in)': '0.023 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.046 KAS' },
    },
    state: { tpPrice: '2300000', stopPrice: '0', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'OCO sell: take-profit above, stop below', type: 'oco', side: 'sell', kind: 'KobCondAsk', listType: 'oco', listLabel: 'OCO', ...SELL,
    fields: () => ({ amount: '2', takeProfit: '0.027', stop: '0.023' }),
    receipt: {
      orderTitle: 'Sell OCO order (take-profit and stop)',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Take-profit price': '0.027 EXKCC / KAS', 'You receive at least per token (all-in)': '0.027 EXKCC / KAS', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2 KAS' },
    },
    state: { tpPrice: '2700000', stopPrice: '2300000', slipBps: '300', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'OCO buy: take-profit below, stop above', type: 'oco', side: 'buy', kind: 'KobCondBid', listType: 'oco', listLabel: 'OCO', locked: '2.07762 KAS', escrowed: null,
    fields: () => ({ amount: '2', takeProfit: '0.023', stop: '0.027' }),
    receipt: {
      orderTitle: 'Buy OCO order (take-profit and stop)',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '2 EXKCC', 'Take-profit price': '0.023 EXKCC / KAS', 'You pay at most per token (all-in)': '0.023 EXKCC / KAS', 'Stop trigger': '0.027 EXKCC / KAS', 'Worst fill price after the trigger': '0.02781 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 2 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '2.07762 KAS' },
    },
    state: { tpPrice: '2300000', stopPrice: '2700000', slipBps: '300', amountLeft: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- IFD / IFO
  {
    name: 'IFD buy-first: buy 0.0245, then sell 0.026', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0245', 'exit.takeProfit': '0.026' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '2600000', stopPrice: '0' } } },
    receipt: {
      orderTitle: 'Buy IFD entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '16.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.026 EXKCC / KAS', 'You receive at least per token (all-in)': '0.026 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000', entryStop: '0', rptAmount: '0', exitCarrier: '200000000', deliveryCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD sell-first: sell 0.026, then buy back 0.0245', type: 'ifd', side: 'sell', kind: 'KobIfdAsk', listType: 'ifd', listLabel: 'IFD', locked: '12.00000003 KAS', escrowed: '2 EXKCC',
    fields: () => ({ amount: '2', price: '0.026', 'exit.takeProfit': '0.0245' }),
    exit: { state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '0' } } },
    receipt: {
      orderTitle: 'Sell IFD entry',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.026 EXKCC / KAS', 'You receive at least per token (all-in)': '0.026 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.00000003 KAS' },
      exitTitle: 'Exit created after each entry fill: Buy take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2600000', amountLeft: '200000000', minFill: '50000000', prefund: '0', exitCarrier: '200000000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD buy-first with a stop exit', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0245', 'exit.kind': 'stop', 'exit.stop': '0.023' }),
    exit: { state: { kind: 'KobCondAsk', fields: { stopPrice: '2300000', tpPrice: '0', slipBps: '300' } } },
    receipt: {
      orderTitle: 'Buy IFD entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '16.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell stop order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 0.5 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFO buy-first (bracket): entry, then take-profit 0.026 or stop 0.023', type: 'ifo', side: 'buy', kind: 'KobIfdBid', listType: 'ifo', listLabel: 'IFO', locked: '16.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0245', 'exit.takeProfit': '0.026', 'exit.stop': '0.023' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '2600000', stopPrice: '2300000', slipBps: '300' } } },
    receipt: {
      orderTitle: 'Buy IFO entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '16.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell OCO order (take-profit and stop)', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.026 EXKCC / KAS', 'You receive at least per token (all-in)': '0.026 EXKCC / KAS', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 0.5 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', minFill: '50000000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFO sell-first (bracket): the prefund covers a stop buy-back above the entry', type: 'ifo', side: 'sell', kind: 'KobIfdAsk', listType: 'ifo', listLabel: 'IFO', locked: '12.00362003 KAS', escrowed: '2 EXKCC',
    fields: () => ({ amount: '2', price: '0.026', 'exit.takeProfit': '0.0245', 'exit.stop': '0.027' }),
    exit: { state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '2700000', slipBps: '300' } } },
    receipt: {
      orderTitle: 'Sell IFO entry',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.026 EXKCC / KAS', 'You receive at least per token (all-in)': '0.026 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Prefund': '0.00181 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.00362003 KAS' },
      exitTitle: 'Exit created after each entry fill: Buy OCO order (take-profit and stop)', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Stop trigger': '0.027 EXKCC / KAS', 'Worst fill price after the trigger': '0.02781 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 0.5 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2600000', amountLeft: '200000000', prefund: '181000', rptAmount: '0' },
    timing: { kind: 'gtc' },
  },
  // ---- IFD with a stop entry
  {
    name: 'IFD with a stop entry, buy-first', type: 'ifd', side: 'buy', kind: 'KobIfdBid', listType: 'ifd', listLabel: 'IFD', locked: '16.077 KAS', escrowed: null,
    fields: () => ({ amount: '2', price: '0.0275', 'entry.stop': '0.027', 'exit.takeProfit': '0.029' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '2900000' } } },
    receipt: {
      orderTitle: 'Buy IFD stop entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.0275 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0275 EXKCC / KAS', 'Entry trigger (stop entry)': '0.027 EXKCC / KAS', 'Trigger rule': 'a fill of at least 0.5 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '16.077 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.029 EXKCC / KAS', 'You receive at least per token (all-in)': '0.029 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2750000', entryStop: '2700000', bandDaa: '300', keeperTip: '2200000', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'IFD with a stop entry, sell-first', type: 'ifd', side: 'sell', kind: 'KobIfdAsk', listType: 'ifd', listLabel: 'IFD', locked: '12.00000003 KAS', escrowed: '2 EXKCC',
    fields: () => ({ amount: '2', price: '0.0225', 'entry.stop': '0.023', 'exit.takeProfit': '0.021' }),
    exit: { state: { kind: 'KobCondBid', fields: { tpPrice: '2100000' } } },
    receipt: {
      orderTitle: 'Sell IFD stop entry',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Entry limit price': '0.0225 EXKCC / KAS', 'You receive at least per token (all-in)': '0.0225 EXKCC / KAS', 'Entry trigger (stop entry)': '0.023 EXKCC / KAS', 'Trigger rule': 'a fill of at least 0.5 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Exit carrier': '2 KAS', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.00000003 KAS' },
      exitTitle: 'Exit created after each entry fill: Buy take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '0.5 EXKCC', 'Take-profit price': '0.021 EXKCC / KAS', 'You pay at most per token (all-in)': '0.021 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2250000', entryStop: '2300000', bandDaa: '300', keeperTip: '2200000', armed: '0', amountLeft: '200000000' },
    timing: { kind: 'gtc' },
  },
  // ---- repeat IFD / IFO (default unlimited within 90 days; explicit count). The merge tip is charged per exit minimum fill (cond-common.ts
  // mergeTipRate): with the IFD default minimum fill (a quarter of the amount, 0.5 EXKCC) it would double per token and turn these spreads into a
  // loss, so the cases set a 1 EXKCC minimum fill (merge tip 0.01 KAS per token buy-first, 0.015 sell-first; a 0.0155 KAS spread is the smallest
  // profitable one here).
  {
    name: 'repeat IFD buy-first, default unlimited (within 90 days)', type: 'repeatIfd', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000', tip: '1000000' } } },
    receipt: {
      orderTitle: 'Buy repeat IFD entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Repeat': 'unlimited (within 90 days) (2 EXKCC per cycle)', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Take-profit price': '0.04 EXKCC / KAS', 'You receive at least per token (all-in)': '0.03 EXKCC / KAS', 'Priority tip': '0.01 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '2000000000000001', exitCarrier: '200000000' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFD buy-first with an explicit count of 3', type: 'repeatIfd', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'repeat.count': '3' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000' } } },
    receipt: {
      orderTitle: 'Buy repeat IFD entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Repeat': 're-arms up to 3 times (2 EXKCC per cycle)', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Take-profit price': '0.04 EXKCC / KAS', 'You receive at least per token (all-in)': '0.03 EXKCC / KAS', 'Priority tip': '0.01 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '600000001' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFD sell-first, default unlimited', type: 'repeatIfd', side: 'sell', kind: 'KobIfdAsk', listType: 'repeat', listLabel: 'Repeat IFD', locked: '8.00000001 KAS', escrowed: '2 EXKCC',
    fields: () => ({ amount: '2', minFill: '1', price: '0.04', 'exit.takeProfit': '0.0245' }),
    exit: { state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', tip: '1500000' } } },
    receipt: {
      orderTitle: 'Sell repeat IFD entry',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Entry limit price': '0.04 EXKCC / KAS', 'You receive at least per token (all-in)': '0.04 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Repeat': 'unlimited (within 90 days) (2 EXKCC per cycle)', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '6.00000001 KAS' },
      exitTitle: 'Exit created after each entry fill: Buy take-profit order', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Take-profit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0395 EXKCC / KAS', 'Priority tip': '0.015 EXKCC / KAS', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '4000000', amountLeft: '200000000', rptAmount: '2000000000000001', prefund: '0' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFO buy-first, default unlimited', type: 'repeatIfo', side: 'buy', kind: 'KobIfdBid', listType: 'repeat', listLabel: 'Repeat IFD', locked: '10.049 KAS', escrowed: null,
    fields: () => ({ amount: '2', minFill: '1', price: '0.0245', 'exit.takeProfit': '0.04', 'exit.stop': '0.023' }),
    exit: { state: { kind: 'KobCondAsk', fields: { tpPrice: '4000000', stopPrice: '2300000' } } },
    receipt: {
      orderTitle: 'Buy repeat IFO entry',
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Entry limit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0245 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Delivery carrier': '2 KAS', 'Repeat': 'unlimited (within 90 days) (2 EXKCC per cycle)', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '10.049 KAS' },
      exitTitle: 'Exit created after each entry fill: Sell OCO order (take-profit and stop)', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Take-profit price': '0.04 EXKCC / KAS', 'You receive at least per token (all-in)': '0.03 EXKCC / KAS', 'Priority tip': '0.01 EXKCC / KAS', 'Stop trigger': '0.023 EXKCC / KAS', 'Worst fill price after the trigger': '0.02231 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 1 EXKCC of a sell order resting at or below the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '2450000', amountLeft: '200000000', rptAmount: '2000000000000001' },
    timing: { kind: 'gtc' },
  },
  {
    name: 'repeat IFO sell-first with an explicit count of 3', type: 'repeatIfo', side: 'sell', kind: 'KobIfdAsk', listType: 'repeat', listLabel: 'Repeat IFD', locked: '8.05300001 KAS', escrowed: '2 EXKCC',
    fields: () => ({ amount: '2', minFill: '1', price: '0.04', 'exit.takeProfit': '0.0245', 'exit.stop': '0.05', 'repeat.count': '3' }),
    exit: { state: { kind: 'KobCondBid', fields: { tpPrice: '2450000', stopPrice: '5000000' } } },
    receipt: {
      orderTitle: 'Sell repeat IFO entry',
      lines: { 'EXKCC (e5e5e5e5…e5e5e5e5)': '−2' },
      card: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Entry limit price': '0.04 EXKCC / KAS', 'You receive at least per token (all-in)': '0.04 EXKCC / KAS', 'Exit carrier': '2 KAS', 'Prefund': '0.0265 EXKCC / KAS', 'Repeat': 're-arms up to 3 times (2 EXKCC per cycle)', 'Refund tip kept in the order': '0.049 KAS', 'KAS on the order output': '6.05300001 KAS' },
      exitTitle: 'Exit created after each entry fill: Buy OCO order (take-profit and stop)', exitCard: { 'Amount': '2 EXKCC', 'Minimum fill': '1 EXKCC', 'Take-profit price': '0.0245 EXKCC / KAS', 'You pay at most per token (all-in)': '0.0395 EXKCC / KAS', 'Priority tip': '0.015 EXKCC / KAS', 'Stop trigger': '0.05 EXKCC / KAS', 'Worst fill price after the trigger': '0.0515 EXKCC / KAS', 'Fill band': '3% band, 30 s auction', 'Trigger rule': 'a fill of at least 1 EXKCC of a buy order resting at or above the stop for 5 s, used by a matcher to arm it', 'Keeper tip': '0.022 KAS', 'Trigger armed': 'not yet armed', 'Refund tip kept in the order': '0.049 KAS' },
    },
    state: { price: '4000000', amountLeft: '200000000', rptAmount: '600000001', prefund: '2650000' },
    timing: { kind: 'gtc' },
  },
];

/** `datetime-local` text of `c.unix + seconds` in the browser's wall clock (truncated to the minute, like the input itself). */
function localAfter(c: Ctx, seconds: number): string {
  const d = new Date((c.unix + seconds - c.tz * 60) * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}T${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}
