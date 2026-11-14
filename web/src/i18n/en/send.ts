// The send flow of an order action and its one-line status (ui/confirm/send.ts, SendStatus.tsx). Keys `send.*`; decoder findings are in `issues.*`.
export default {
  'send.checking': 'Checking the order...',
  'send.confirming': 'Confirm the order...',
  'send.continueInWallet': 'Continue in wallet',
  'send.cancelled': 'Cancelled. Nothing was sent.',
  'send.signing': 'Confirm in {wallet}...',
  // the visibility gate (wallet/visibility.ts): the coin this transaction spends is the change of the one just sent, not yet seen by the wallet
  'send.waitWallet': 'Waiting until {wallet} sees your last transaction...',
  'send.yourWallet': 'your wallet',
  'send.confirmTitle': 'Confirm order',
  'send.via': 'via {wallet}',
  'send.sending': 'Sending...',
  'send.submitted.waiting': 'Submitted, waiting for a block:',
  'send.submitted.confirmed': 'Accepted by the network:',
  'send.submitted.unconfirmed': 'Submitted, not seen in a block yet; check again in a minute:',
  'send.noWallet': 'Connect your wallet to sign.',
  'send.accountChanged': 'The wallet account changed (planned for {planned}, now {now}). Nothing was sent; try again.',
  'send.networkMismatch': 'Wallet on {wallet}, app on {app}: switch the wallet network and try again.',
  'send.alreadySpending': 'Your last transaction already spends a coin of this one (it is on its way to a block). Nothing was sent; wait a few seconds and try again.',
  'send.inputsUnavailable': 'The node could not confirm your coins. Nothing was sent; try again.',
  'send.decodeFailed': 'The transaction could not be read back. Nothing was sent.',
  'send.checkFailed': 'The order could not be checked. Nothing was sent; try again.',
};
