// Builder and node refusals in plain words (i18n/build-error.ts); the raw text stays behind "Details".
export default {
  'build.insufficient-funds': 'there is not enough free KAS in the wallet to pay for it',
  'build.not-yet-refundable': 'the order cannot be refunded yet (from DAA {daa})',
  'build.custody-missing': 'the token custody of the order was not found; refresh and try again',
  'build.mixed-extension': 'the token outputs involved belong to different token versions and cannot move together',
  'build.unsupported-token': 'this token program is not supported by this app',
  'build.not-enough-tokens': 'there are not enough tokens to cover it',
  'build.too-large': 'the transaction would be too large; try fewer orders or fills at once',
  'build.not-active': 'the order is not active yet',
  'build.wrong-utxo': 'an output it needs does not belong to this order or token; refresh and try again',
  'build.spent': 'an output it needs was already spent (the order may have just filled or changed); refresh and try again',
  'build.invalid-terms': 'the order terms are not valid (an amount, price or minimum fill is out of range)',
  'build.timing': 'its time terms do not fit the current time (expiry, start or lock time)',
  'build.other': 'the transaction builder refused it',
};
