// The UI test-id contract: the UI hard-codes these `data-testid` values (it imports NOTHING from e2e/), the e2e specs use the constants.
// Keep this list authoritative and short. Dynamic ids and intent-form fields go through the helpers at the bottom.
export const TESTID = {
  // navigation
  navMarket: 'nav-market',
  navOrders: 'nav-orders',
  navIssue: 'nav-issue',
  navSettings: 'nav-settings',
  // wallet
  walletConnectKasware: 'wallet-connect-kasware',
  walletConnectKaspire: 'wallet-connect-kaspire',
  walletConnectKastle: 'wallet-connect-kastle',
  walletAddress: 'wallet-address',
  walletNetwork: 'wallet-network',
  // market
  tokenSelect: 'market-base-select',
  bookAsks: 'book-asks',
  bookBids: 'book-bids',
  tradesList: 'trades-list',
  // order ticket
  orderType: 'order-type',
  orderSideBuy: 'order-side-buy',
  orderSideSell: 'order-side-sell',
  orderAmount: 'order-amount',
  orderPrice: 'order-price',
  orderTip: 'order-tip',
  orderReview: 'order-review',
  orderIssues: 'order-issues',
  // pre-sign confirmation
  confirmScreen: 'confirm-screen',
  confirmSummary: 'confirm-summary',
  confirmBlocking: 'confirm-blocking',
  confirmSign: 'confirm-sign',
  confirmCancel: 'confirm-cancel',
  // transaction result
  txStatus: 'tx-status',
  txId: 'tx-id',
  // my orders
  ordersList: 'orders-list',
  ordersCancelAll: 'orders-cancel-all',
  ordersExport: 'orders-export',
  ordersImport: 'orders-import',
  balancesPanel: 'balances-panel',
  // issue
  issueForm: 'issue-form',
  issueName: 'issue-name',
  issueTicker: 'issue-ticker',
  issueDecimals: 'issue-decimals',
  issueSupply: 'issue-supply',
  issueReview: 'issue-review',
} as const;

export type TestId = (typeof TESTID)[keyof typeof TESTID];

/** Ids that carry a covenant id (64 hex chars, lower case). */
export const orderRow = (covenantId: string) => `order-row-${covenantId}`;
export const orderCancel = (covenantId: string) => `order-cancel-${covenantId}`;
export const orderReplace = (covenantId: string) => `order-replace-${covenantId}`;

/** Every intent form input has `data-testid="field-<intentFieldName>"` (the field names of src/kob/intent-*.ts). */
export const field = (intentFieldName: string) => `field-${intentFieldName}`;
