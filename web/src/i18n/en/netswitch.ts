export default {
  'netswitch.trigger': 'Network: {network}. Change network',
  'netswitch.title': 'Network',
  'netswitch.current': 'current',
  'netswitch.note': 'Choosing a network reloads the page.',
  'netswitch.noteMainnet': 'Mainnet uses real funds.',
  'netswitch.otherServers': "This site's indexer and node are for {home}. On {network} the page uses the Kaspa SDK's public node and no indexer, so the book and the charts stay empty until Settings names servers for it.",
  'netswitch.blocked': 'This browser has saved servers ({fields}). They would stay in force on the other network, so the switch is off until they are removed.',
  'netswitch.settings': 'Open Settings',
  'netswitch.failed': 'The choice could not be saved (this browser blocks storage). Change the network in Settings.',
} as Record<string, string>;
