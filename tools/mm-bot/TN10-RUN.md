# TN10 run, 2026-11-16 17:37 to 18:08 UTC

A 31-minute live run on testnet-10 with fresh throwaway keys (a bot key and a taker key), capital 1,000 USD at the
live fair value (about 37.9 KAS per token). The node was a public TN10 node; the indexer was `https://kob-api-tn10.kaspa.ag`.
The kob CLI was a development build of 2026-11-15, because the shared cargo slot was busy.

- Funding: 17,500 KAS to the bot and 400 KAS to the taker, in one transaction
  `0517d91c7a662f1ed8c19cda71576ee499f5995a5f1391f2bfe067fe1aa57b1b`.
- Test token TUSDC6, 6 decimals, public-mint, 10,000 supply to the taker:
  `216eddd250b4383d65352ead320755b7e5ad891f20efbfdca3f39ce041cfb1d2`. Preparation
  `4515c24a02df689a28cd026fb0192bdce5b1b91258f0bb633a94ac9ea5ac5434`, genesis
  `1cd1dbb6483bb84221b1afa5108922d97352ebd9a61c4240380fa73e40eaff0c`.
- The taker opened the market with a 1 TUSDC6 ask at 60 KAS: `b074b2492eff153711614fb82fd1a24b3d052309035b49009f0f0136b9515307`.

| UTC | What happened | Transactions |
|---|---|---|
| 17:37 | Start in IDLE/cooldown. The ask side was idle: "Insufficient TUSDC6 (…): asks need 45% = 450 USD …; deposit 450.112528 TUSDC6 …" | |
| 17:41 | QUOTING; 3 bids placed | `803f9aeea38866ac27f7d9b1f3cb8fe7a8ed5e8dec2f443994ec41b4b3ebaced`, `56d4d653df6a9ab9c53fcea412b0e8c1dde40731941bda3b004060083739b421`, `57272a6c1b8060caf1ccccedb1f599d1e841aada45864d5719fa5e15705c99c9` |
| 17:42 | The taker deposited 500 TUSDC6 (`a9d1b4421c8d9fc658f7bfb19a8711f2423762ec08a55b6bd2507521c64dc689`); 3 asks placed; "Funds received, ask quoting resumed." | `b4f1158b9040d7a5a188f29a4c22415c1c3888bab5168b36bb1ea8c01bae032d`, `34d2475ad7f7c50c4b11c08307d38b9a1d7a71b9d6d439ccabf7ede54b72d901`, `400626b1f5f372fffd4114741f83a6b352e92ffacac3b2df135d2c3801f5872d` |
| 17:49 | A real move: fair moved 0.271 %, so the 3 bids were amended in place (same ids) | `f66358cf2e69b7678df59949ad2b9f283806375816ca775a124fc043e591c9a2`, `1e744042c8075beb6b0033f1764775932738244213d850b63bba092dedcd7a6b`, `9a1d51dcdf23a1ed80ce7755fb7a8cf0153d2fafee6b3b9513293703b0f89c43` |
| 17:50 | The bot process was killed; the keepalive restarted it 60 s later with the new 1 % total spread, and all 6 orders were amended to their new targets | `5e27b57a580e95a05b4a22920e66d76482b8309c30dd2a92371124e440af3ca0`, `9334d57ddaefd2d2b6cec11c9101d989df1645e3076eead6c944d3439c49222d`, `9ef5498a1f34b3a451ea909d81f7c58aa073c8c5e08c36738e61299cc66decc4`, `1b09e53feb147077da9c4bd9342dfefff476ce39bcb7fd010b3a1d9c906049df`, `9e7e09eebf43b6226c722c820d09f4e3dc1b6da544579b79b4bc39b7ae67dd7c`, `ab7d529136ef64246815c0aca3db202383a89d945c686c4af60d7d3c935c0238` |
| 17:55 | Fills by the taker: it bought 2 TUSDC6 from the best ask at 38.08687114 KAS, then sold 3 TUSDC6 into the best bid at 37.70789729 KAS. Both were logged as FILL | `8d9d665ddf647a1218d8c1448f26e0ab302c99ae25e0ac0ab66d29fec798b212`, `150d57e2dea4ae7191ca75333c4d859794151a5bbcb00bdb33e836114d9490ab` |
| 17:58 | A forced peg drift (pegBand set to 0.01 %): IDLE/peg, all 6 quotes pulled | `4e36ad82f0ac6f4fd32d62ac9e61da854b124ff7177767e9f740e60f55831b89`, `0656eea8c683c00f4da14e5625f1790f13228e0d8ea62d93a69f6a84021c04fb`, `6dab88db6411849cccd243aecb9713dab3197e35c2b59bc6835f73a2bf43358c`, `b9d24f3af85f19d0d3c74f0bb35f8b53691e50a6a7a51456a47699156c280257`, `4eb592193019e3c79eeead443f59558e91524f825a381fe367f0a0f834ab2298`, `7a499a6b1ab0e67826734d70ebb7c73d2608fa8f58bc07520df0a5afe69e0b30` |
| 18:01 | The band was restored: IDLE/cooldown, then QUOTING at 18:04 | |
| 18:04 | 3 bids placed; 5 token UTXOs merged in 2 transactions; 3 asks placed | `985ab6165fdb152a6a5d9007b322ddb16ebac6d16cac1fe27bd2670936a5bf60`, `b8e47bf73f538b6eebef2e3c3acdb6432adfe1a486d1bb79289a9c40731483a7`, `c70078552f38361e56a5d0a6a95f462a503396b64d923958ae9716dc5a34a378`, `014a2e0679c31d3079c9f2942686dd3a57b32e8f25ad38fb358b8a8166ad8caa`, `54fda6eed867c3cefc1105462420599d7882704b722049f51e3a023b2fdb6010`, `84c0aecf1718b7d5a15ead87a14c2868c5050cc73b185bad404320c0fc9cf463`, `233ee52d1cf240136e18ffbae23a9075ccf1d3643d94791ae4836b1060d1e739`, `8cdb50d87efc08de21d434a6082bed2b86d48cd65ae2d42632e577c19918f6d1` |
| 18:07 | STOP: 6 cancels; "stopped: no quote left"; the keepalive exited | `9d84621cb5e24ce2b00f961798bda4372f5b730bc0c49642cfbcb39ee17afcae`, `6a4e21e4e64d2a4b1bf5978c4eab1bfaefd859527ead487b97b1547673de3144`, `727b05aeb1ffb0f323a0df51ee83677739f559dea3b4d085327b6a5926a532fd`, `cd78f8fb018a32998f2d9dfe3a3fff5b32eedfa7b3475ee18343063ee46a86dd`, `5f205f410f0a46be87a0c7960407ecd6dd4a32b3ccfeb94625c0a6a6623a32a9`, `5c11703d65fbc7b51fd1f3f5a36ccf52db8bdd29ac9e0c853c73cbf059c61ad6` |

The bot paid 0.264424 KAS in fees over 34 transactions (12 placements, 9 amends, 12 cancels, 1 merge); the README lists
each kind. Found and fixed during the run:
- a restart after STOP stayed in STOPPING;
- the pull log line counted cancels that were already sent.

## To check on the next live run (round-3 fixes, not testable offline)

The stand-in CLI of `test/` imitates `kob recover`; these points need the real CLI, node and indexer:
- `kob recover` report shape: each entry has `covenant_id`, `status` (`live`, `not_found`, `other_maker`,
  `unsupported`), `template_hash`, `note`, and for a live order `outpoint`, `value`, `amount_left`; `--cancel` adds
  `cancel.status` (`submitted`, `signed`, `refused`, `failed`, `skipped`) and `cancel.txid`/`fee`. The bot reads exactly
  these.
- A cancelled or fully bought **bid**: `not_found` with a note that contains "spent: no unspent output at the order's
  script". The bot takes that text as proof the bid ended (R3T-3); if the wording differs, every ended bid stays
  unresolved with a `node-unconfirmed` alert.
- An **ask** partly filled by an amount that is not k x minFill, listed by the indexer: found `live` through
  `--amount-left <id>=<indexer amount_left>`, its `amount_left` updated in `state.json`. Repeated `--amount-left` for one
  id (the bot may pass up to 4) is accepted.
- The same ask with the indexer's list hiding it (or a fully filled ask): `not_found` with the "spent, or partly filled
  to an amount the search did not reach" note; the bot keeps it as unresolved (one `node-unconfirmed` alert, the level not
  placed again, STOP waits). Check that `GET /v1/orders/<id>` of the real indexer returns `amount_left`, which the bot
  passes as a hint.
- An order someone else makes under the bot's key without custody (R3T-2): `kob cancel <id>` refuses before signing
  (exit 1, "custody not found on the node: not cancelled"; no fee). The bot logs it, raises one `foreign-order` alert,
  tries the cancel at most 3 times, keeps quoting bids at their targets and places asks.
- A `kob place` refused for another-scale orders on an indexer-only-decimals token (R3T-5, fixed in the CLI): after 3
  refusals the `placement` alert carries the CLI's refusal text.
- A fully filled ask with `node` set to the JSON wRPC URL: the next cycle ends it with "filled (its fills are paid in
  outputs the node showed)" and no `node-unconfirmed` alert. This checks that the real node's `getUtxosByAddresses`
  entries carry `outpoint.transactionId` and `utxoEntry.scriptPublicKey` as `000020<key>ac`, that the indexer's
  `/v1/fills` items carry `txid` and `amount` (base units), and that the fill's payout to the maker is an output of that
  transaction of at least `ceil(amount x (price - tip) / scale)`. With `node` null the same fill leaves the ask
  unresolved, parked after its expiry, and `check` warns.

## To check on the next live run (round-4 fixes, not testable offline)

- R4T-2: the real node's `getUtxosByAddresses` entries carry `outpoint.index` as a number (an output is keyed
  `txid:index`). A fully filled ask whose taker paid after its last placement or amend ends with "filled (its fills are
  paid in outputs the node showed)", and `state.json` `payments` marks that output `usedBy` the ask. A KAS send to the
  bot's address made before a placement never proves a fill of that ask. Each placement and amend adds one
  `getUtxosByAddresses` call just before it (count them in the node log: at most 12 extra a cycle).
- R4T-3: `GET /v1/orders?maker=<key>&token=<id>&status=active` of the real indexer returns only that token's orders,
  with `next_cursor` while more are left.
- R4T-4: `retiredTokens` with a few custody-less orders of the retired token on the key: at most 2 `kob cancel` calls a
  cycle, none after 20 s, and the reprice of the live quotes in the next cycle is on time.
- R4T-5: `mm-bot check` on a mainnet config with `node` null (or a Borsh URL) exits 2 naming `node`; on testnet-10 it
  warns. `/v1/fills?token=<id>&limit=100&before=<id>` pages back (the bot reads up to 10 pages to its last fill id).
  `mm-bot unresolved` lists a parked ask; `mm-bot drop --id <id>` with the bot running refuses, with the bot stopped
  prints the `kob recover` answer and refuses while the order is live (pass `--amount-left` of a partly filled one),
  and drops an ended one (an `operator-drop` line in `logs/actions.jsonl`).
- R5T-1: a fully filled ask is kept as proven (`mm-bot unresolved` lists it as PROVEN, `status` shows it pending
  `proven`), its level is placed again, and it leaves `state.json` 24 h after its expiry.
