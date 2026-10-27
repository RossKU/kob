# Trading on KOB: a guide for users

KOB is a non-custodial order book for Kaspa tokens. You trade from your own wallet in a web page; nobody holds your coins. This
guide walks through the web app as it is today. The labels in quotation marks are the ones on the screen.

**Read this first.** KOB is **pre-audit software**. Its contracts have had no external security review (`SECURITY.md`,
`docs/security/AUDIT-2026-10.md`). Trade only funds you can afford to lose. Testnet-10 coins have no value; use them to learn.

Contents: [1 What you need](#1-what-you-need) | [2 Connect a wallet](#2-connect-a-wallet) |
[3 Find a token](#3-find-a-token-and-read-its-facts) | [4 Buy and sell](#4-buy-and-sell) |
[5 Limit and market orders](#5-limit-and-market-orders) | [6 Partial fills](#6-partial-fills-and-you-may-receive-up-to) |
[7 Your orders: cancel, refund, renew](#7-your-orders-cancel-refund-renew) | [8 Merge token UTXOs](#8-merge-token-utxos) |
[9 Status banners](#9-status-banners-node-and-indexer) | [10 Fees: what goes where](#10-fees-what-goes-where) |
[11 Private-key guidance](#11-private-key-guidance) | [12 Create a token, and find one you created earlier](#12-create-a-token-and-find-one-you-created-earlier) |
[13 Backups and local data](#13-backups-and-local-data) | [14 Words you will meet](#14-words-you-will-meet)

## 1. What you need

* A **browser wallet extension on a desktop browser**: KasWare or Kaspire. This is the recommended setup.
* Or, on a phone or where no extension exists, the app's own **Private key** option (section 11). It is for small balances.
* A little KAS on the network the page uses. The header shows it as "Network: mainnet" or "Network: testnet-10", and your wallet
  must be on the same network. Testnet-10 coins have no value; you get them by mining or from a faucet (the repository's x402 example uses
  https://faucet-tn10.kaspanet.io, which sits behind a Cloudflare challenge, so open it in a browser).
* Tokens you hold are not shown by ordinary wallets (they live in covenant outputs). The app tracks them for your key; see "Balances
  the wallet does not show" in section 7.

## 2. Connect a wallet

1. Install **KasWare** or **Kaspire** in your desktop browser. If the page finds none it says "No Kaspa wallet detected", with the
   buttons "Get KasWare" and "Get Kaspire". The page notices a new extension by itself; extensions can take a few seconds to appear.
2. Press **Connect KasWare** (or **Connect Kaspire**) in the header. Approve in the wallet popup. The header then shows
   "Connected with KasWare" and your address.
3. Check the network. If your wallet is on another network than the page, you see "Wrong wallet network: trading is blocked".
   Switch the network in the wallet; nothing can be signed until they match.
4. **Cancelling needs a wallet that can sign a contract input.** KasWare, Kaspire and the Private key option are known to do it.
   For a wallet the app does not know, it asks once, before the first order, to "Sign a test cancel": a made-up cancel that is
   never sent. If that wallet cannot do it, the app says "{wallet} cannot cancel orders" and refuses to place orders with it (an
   order you cannot cancel would end only by a fill or by refund after expiry): use KasWare or Kaspire.

If you switch account in the wallet, the page reloads your balances and orders for the new account and resets any order form that
was open. If the wallet locks or disconnects the site, the session closes ("no longer exposes an account"): unlock it and connect again.

**Phone, iOS, Firefox.** Extension wallets do not run in phone browsers. The **Private key** option checks that you really see what
you sign, and it needs a browser feature (IntersectionObserver v2) that Chrome on Android and desktop Chromium browsers have, but
**iOS browsers and Firefox do not**. There the option is greyed out and the page says why in text ("This browser cannot use the in-page
private key signer ... Use Chrome on Android, or open this page on a desktop with an extension wallet"). Section 11 covers the key.

Kastle is off by default and not supported; the Settings switch "Offer the Kastle wallet" is experimental (Kastle cannot sign
covenant inputs reliably yet).

## 3. Find a token and read its facts

**Market** (the first page) lists the tokens. Search by ticker, name or covenant id ("Ticker, name or covenant id"). A token is
identified by its **covenant id**, a 64-character hex string; the ticker is only a label that anyone can pick.

* **Same ticker, different token.** If two tokens the page shows share a ticker (look-alike letters count), the page says "Another
  token uses this ticker — check the covenant id". The list shows the start and end of the id, and the token page the full id with a
  copy button. Compare it with the id the token's own project gave you.
* **Tokens without a name.** A token that has no ticker and declared no decimals is hidden behind "Show N token(s) without a name".
  A search or a pasted covenant id finds it anyway. A token the list does not know can be added with "Add an unknown token" (paste
  its covenant id, "Look up"); the indexer lists a token only after its first order was placed.
* **Declared by its creator.** A name that is not in the page's registry comes from the token's own on-chain record (KCC-23). The
  page marks it "declared by its creator": the creator's claim, which nobody has checked. KOB keeps no list of good tokens and
  decides nothing about any token.
* **Not tradable.** "{ticker} cannot be traded in this app" / "Not tradable in this build" means the token's program is one this
  build does not support. You can still see balances and orders.

**Token fact badges.** Wherever a token is shown (list rows, the token page header, next to the order button, each token of a pair)
the page prints neutral badges. They are facts the indexer derives from the chain or the token program, not a verdict:

| Badge | What it says |
|---|---|
| Genesis not verified | The token's creation was not checked against its token program, so units minted outside the program may exist. |
| Live mint authority | A mint authority still exists: more of this token can be created. |
| Mint authority | The token's program has a mint authority: whoever holds it can create more. |
| Public mint | The token's program lets anyone create more. |
| Can blacklist | The program lets the issuer block addresses from moving the token. |
| issuer can freeze or seize | The program can freeze or seize balances; escrowed orders can fail or be lost at the issuer's discretion. |

"what is this?" next to a badge opens its sentence (a touch screen has no tooltip, so it is printed or opens on a tap). A token
without badges is not thereby safe: the badges only report what could be checked.

## 4. Buy and sell

Open a token. The page shows the order book (asks above, bids below), recent trades, a price chart and the **Trade** panel with the
order ticket. On a phone, the "Trade" button jumps to the ticket.

* **Buy** pays KAS and receives the token. **Sell** gives the token and receives KAS. Pick it with the **Buy / Sell** switch.
* Prices are in KAS per whole token. The price is rounded to the market's price step "in your favour" and the page tells you when it
  rounded.
* A click on a price in the book fills the ticket: an ask becomes a buy at that price, a bid a sell.
* The **flip** button next to the pair title shows the market the other way round (tokens per KAS). Buy and Sell then refer to KAS.
  The transaction built is the same.

**The steps.**

1. Choose the **Order type** (Limit or Market to start; sections 5).
2. Enter the **Amount**. Under "Available ..." the page shows the balance the order spends; pressing that figure fills in the
   largest amount your balance allows ("Max"), after carriers, network fees and a small reserve. The slider under the box works in
   percent of Max.
3. For a limit order enter the **Limit price**.
4. Press the order button, labelled with what it does, for example **Buy Limit** or **Sell Market order**.
5. The app re-checks everything silently (wallet account and network, your coins on the node, the built transaction). If something
   makes the order wrong or impossible it stops with one line of text and **nothing is sent**.
6. Then you see the **final confirmation**, the one place with all the details: what you pay and receive, "Locked in orders", the
   "Network fee", and the order's terms (price, tip, time in force). The wallet's own popup will not show these details, so read
   this table. With an extension wallet press **Continue in wallet** and approve in the wallet popup. With the private-key option,
   the table is in the signer frame and its **Approve** button signs.
7. A status line under the button follows the order: "Approve in your wallet...", "Sending...", "Submitted, waiting for a block",
   "Accepted by the network", with a link to the block explorer. If the wallet does not answer, nothing was sent; try again.

**Notes next to the order button.** The app warns in one-line notes. They never stop the order and need no click; only errors
disable the button. You may see:

* "Your price is N% through the market ... Check for a typing error." A limit sell at 0.0025 instead of 0.025 would otherwise sell
  into every bid.
* "The book shows X per token, the last fill Y (N% apart) ... check the price before signing."
* "A slippage of N% can fill far from the market price."
* "This day order ends by 00:00 UTC, within N minutes."
* "Your limit crosses the market: it fills immediately as a taker at your limit; the matcher keeps the difference ..." (section 5).
* Notes of the stop and take-profit orders: the market is already beyond your stop (it arms on the next qualifying trade), a
  take-profit that crosses the market fills at once, and "The genesis of {token} is not verified" for a stop on such a token (a
  stop is armed by trades; trades of units minted outside the program could arm it).

If a red error line shows instead, it names what is missing, for example "Not enough KAS: needs X, you have Y (short Z)" or
"Not enough tokens". A self-trade is refused ("This order would trade against your own order"): cancel the other order first or
change the price.

## 5. Limit and market orders

**Limit.** "Rests at your price; fills at that price or better." It sits in the book until it is filled, you cancel it, or it
expires. Under **Lasts** choose:

* "Good till cancelled (90 days)": the default. It lasts 90 days from placement. Renew before day 85 in My orders (section 7).
  After expiry anyone may refund it to you.
* "Good till a date": you choose the date (maximum 90 days).
* "Day (until 00:00 UTC)": it ends by 00:00 UTC; the page also shows the time in your local zone. The refund follows shortly after.

Advanced options of a limit order: "Priority tip", "Minimum fill", "Start at (optional, local time)", "If it crosses the book
now" and "Fills to fund (optional)". "Ladder" places several limit orders at stepped prices; each is its own order and you sign
each transaction in turn (stopping keeps the levels already placed).

*If your limit crosses the book* (a buy at or above the best ask, a sell at or below the best bid), the default
"Auction to my limit (default)" fills it at once as a taker through a short auction from the best price to your limit and rests
whatever is left at your limit. "Plain limit" fills at your limit price; the matcher keeps the difference. "Refuse" rejects it.

**Market.** "Fills now in a short auction, never beyond your slippage; the rest is returned." The price moves from the best price
toward your worst price over about 20 seconds. **Slippage allowed** (default 3%) bounds the worst price against the best price now.
The confirmation shows the "Expected price (auction start)" and the "Worst price (all-in)". Anything that is not filled is
returned to you.

One transaction takes at most as many counterparties as the token program has token input slots (3 on KOB's standard program), so a
large market order against many small orders can fill only in part. Under Advanced, **Fills (transactions)** lets a market order
keep trading across several transactions for its short life (default 1). The ticket shows how many fills the visible book needs.

Other order types exist (IOC, FOK, stops, take-profit, OCO, IFD, IFO, TWAP, DCA, Dutch). Their help text is in the ticket, and
`docs/spec/order-types.md` defines them. Stops arm only on a trade that a matcher uses, never on a quote or a chart touch, so a
stop can trigger later than the chart suggests and it has no guaranteed price.

## 6. Partial fills, and "You may receive up to"

An order is filled by whoever matches it (a matcher), and it can be filled in several steps. A limit order that is filled in
part shows the status "partly filled" in My orders, and the rest stays in the book at the same price. "Fills (n)" under an order
lists each fill (time, amount, price, what you received, the transaction) with the average price. Only the last fill may be
smaller than the order's **Minimum fill**, so tiny fills cannot eat your carriers.

**Buying is a KAS budget.** A buy escrows KAS, not tokens. The escrow covers the amount you typed at your limit (or worst) price,
plus a delivery carrier for each fill (section 10). A carrier that a fill does not need is spent on more tokens, at your worst
price or better. The confirmation therefore says:

> You may receive up to ... more than the amount the budget was sized for: a buy is a KAS budget, and the carrier of a fill that
> does not happen buys more tokens, at the worst price or better. You never pay more than the budget.

So with a multi-fill market buy you can end up with a little more than you typed (on testnet-10 a buy of 0.6 delivered 0.6676).
You never pay more than the budget shown, and the price still never goes beyond your worst price.

## 7. Your orders: cancel, refund, renew

**My orders** lists your orders and positions in three tabs: "Active", "History" and "All". It works even when the indexer is down:
orders then come from your records in this browser and from the node ("Indexer unreachable: orders come from your records and the
node; status and fills may be missing"). The app reads at most 1,000 of your orders from the indexer (5 pages of 200); the list has no
"older orders" control, and "Export fills (CSV)" covers only what was loaded. Keep your own copy of the CSV and of the backup
(section 13) if you trade a lot.

Status words: open, partly filled, filled, cancelled, refunded, expired (IOC/FOK), closed.

* **Cancel.** Press **Cancel** on an order. It costs a small network fee. Your tokens or KAS, the carriers and any stray tokens
  return to your wallet. "Cancel all (n)" cancels every order, grouped into as few transactions as possible; you sign each in
  turn. "A fill confirmed before your cancel wins; the cancel then fails and nothing is lost." Only you can cancel your order.
* **Amend.** **Amend** changes price, amount and so on as one cancel-and-replace transaction.
* **Refund.** An order that has expired ("expired: refund pending") can be refunded by anyone to its maker, who gets the escrow
  back (the refund tip goes to whoever sends the refund). Press **Refund** yourself, or tick "Refund my expired orders
  automatically while the app is open (no signature; you keep the refund tip)". An order whose escrow no longer covers one
  minimum fill is hidden from the book; "Refund the KAS" gets it back.
* **Renew.** A "Good till cancelled" order lasts 90 days. "Renew before {date} (day N of 90)" warns you; press **Renew**
  (the new order is good for another 90 days from now).
* **Stray tokens.** Tokens sent to an order from outside are not liquidity. "Sweep strays" returns them and keeps the order open;
  a cancel returns them too. Once an order closes, its strays are lost for good.

**Balances the wallet does not show** (top of My orders) lists free tokens, tokens "In your orders", strays, "Free KAS", "KAS
locked in orders" and "KAS on your token UTXOs": every token UTXO carries KAS (its carrier and, after a buy, the price
improvement over your worst price). That KAS is yours; a merge, a sale or a transfer frees it.

Notifications (the bell in the header) are off until you turn them on in "Notification settings": fills, partial fills, stops
armed or triggered, expiry soon, refunds. They are worked out in this browser while the page is open. Nothing is sent to a server.

## 8. Merge token UTXOs

Your tokens sit in token UTXOs (outputs). One transaction can spend only a few of them (3 on KOB's standard program). After many
small buys you may hold more, and a sell then fails with "Your tokens are in too many UTXOs (at most 3 per transaction): merge them
first (My orders, Balances)."

* In **My orders**, under Balances, press **Merge (n)**. The button also shows roughly what the next click frees, for example
  "Merge (5): about +4 KAS".
* **One click is one transaction**: it merges your smallest UTXOs of that token up to the program's limit (3 into 1). It is manual and
  costs a network fee; click again to merge more. "One UTXO remains: nothing more to merge" ends it.
* The confirmation says how many UTXOs become how many ("The amount is unchanged") and how much KAS of their carriers returns to
  your wallet after the fee. A merge never touches an open order's escrow.
* The same applies to KAS: if "Your KAS is in too many small coins (UTXOs)", KOB cannot merge KAS. Send your KAS to your own address
  from your wallet to merge it, or place a smaller order.

## 9. Status banners: node and indexer

The strip under the header shows the **Node** ("reachable", "unreachable", "checking...", with the DAA score) and the **Indexer**.
The indexer is the server that lists the book, trades and order status; the node is a Kaspa node that gives balances and takes
your transactions.

| You see | Meaning and what to do |
|---|---|
| "Order book data may be out of date" with "The indexer cannot be reached" | Books and order status may be old or missing. **Cancelling your orders still works** (from your records and the node). Placing new orders is blocked until an indexer answers. |
| "The indexer is behind the chain by ..." | Prices and status may be older than the chain. |
| "The indexer is ... behind the chain. Do not trust the book shown" | Orders may already be filled or cancelled. Wait or use another indexer (Settings). |
| "The indexer is catching up" / "starting" | Data is incomplete for a while. |
| "The indexer lost its connection to the Kaspa node" | Its data is frozen until it reconnects. |
| "The indexer reports a gap in its data" | Order status may be wrong until it recovers. |
| "The indexer serves another network than this app" | Its data is ignored for trading. Fix the indexer URL in Settings. |
| "No indexer is configured" | No book and no order history. Set an indexer URL in Settings. |
| "No connection to a Kaspa node" | Balances, DAA time and sending are unavailable. Check the node URL in Settings (a `wss://` node of the right network, with its path). With no node URL the page asks the Kaspa SDK's public resolver; if none works it says so. After about 25 seconds the status says "unreachable". |
| "Single indexer, not cross-checked" | Only one indexer is configured. With two or more, token powers, price scales and best prices are cross-checked and a disagreement is shown ("Indexers disagree": check the covenant id and price yourself). |
| "Deep chain reorganisation" badge | The chain replaced many blocks; the data was refreshed and recent confirmations may have changed. |
| "Node saved in this browser ..." / "Indexer saved in this browser ..." | A node or indexer chosen in Settings overrides the site's default. If you did not set it, reset it ("Use the site's servers"). |

A book marked "stale since hh:mm:ss" is the last good book; the latest indexer answer was late or failed.

## 10. Fees: what goes where

There is **no KOB fee**: no protocol fee, no operator key, no admin function in the order contracts, and the project's hosted app adds nothing
to your orders (`docs/ops/web-hosting.md`, neutrality rules). What you pay:

| Item | What it is | Where it goes | Returned? |
|---|---|---|---|
| **Network fee** | The Kaspa transaction fee of the transaction you sign (placing, cancelling, merging ...), shown in the confirmation. | The network (miners). | No. |
| **Carrier KAS** | KAS held in each contract output (default 2 KAS per UTXO; "Carrier KAS is not a fee"). | Locked in the order or token UTXO. | Yes, when the order ends or the tokens are spent or merged. |
| **Delivery carrier** | A buy deposits one per possible fill; it rides on the tokens delivered to you. | Unused ones return with a cancel or refund, or buy more tokens (section 6). A used one stays on your token UTXO until you merge or sell. | Yes (see "KAS on your token UTXOs"). |
| **Priority tip** (optional, default 0) | Your tip per token to make matchers prefer your order (the chain has no time priority). A sell receives price minus tip, a buy pays price plus tip. It is part of the all-in price shown. | The matcher that fills the order. | No. |
| **Refund tip** | Kept in the order. | Paid only to whoever refunds the expired order; otherwise returned to you. | Yes, unless someone refunds it. |
| **Keeper tip** (stops, trailing) | Prepaid for the matcher that arms or trails your stop. | The keeper, per update. | The confirmation shows how much is reserved. |
| **The spread** | When your limit crosses the market it fills as a taker at your limit; "the matcher keeps the difference" to the better price. | The matcher. | No. |

Matchers pay the network fee of the fill transaction themselves; they earn the spread and the tips. Two switches in Settings
change what your own transactions pay: "Pay the priority fee" (also counts the storage mass, KIP-9; by default a transaction pays
exactly the node's relay minimum) and "Dynamic network fee" (pay the rate the node currently estimates, capped, instead of the
minimum). Neither is the "Priority tip" of the ticket, which goes to the matcher.

## 11. Private-key guidance

Use an extension wallet when you can. The **Private key** option exists for places where there is none.

* Press **Private key** in the header. The key is typed into a separate, sandboxed frame ("Private key signer") that the page's own
  code cannot read. It shows each transaction, decoded from the transaction itself, and signs only after you press **Approve**.
* The frame asks for a **64-character hexadecimal key (0-9, a-f)**. A seed phrase of 12 or 24 words does **not** work.
* **The key stays in this tab only.** Reloading or closing the page forgets it and you enter it again. If your phone discards the
  background tab while you copy the key from another app, the session (and any ladder or cancel-all in progress) is lost.
* Use a **dedicated key with a small balance**, not your main wallet.
* **Approve** stays locked until you have seen the whole request: "Keep this window fully on screen and scroll through every line.
  Anything over it, a zoomed page or a cut-off window keeps it locked."
* This does not protect against a malicious script that already runs in the page: such a script could draw a fake key field. Only
  open the app from an address you trust, and prefer an extension wallet.

Never type a key anywhere else, never paste it in chats, and do not keep it in a screenshot.

## 12. Create a token, and find one you created earlier

**Create token** (header) makes a fixed-supply token: name, ticker, decimals, supply, and the holders. It takes two transactions
and one approval: the first records its details on chain, the second creates the token. The supply is fixed forever; there is no mint
and no burn. Each token output locks some KAS (the carrier, default 2 KAS), returned when the tokens are spent or merged. The
summary shows "KAS locked in token outputs" and the network fees. Anyone may pick any ticker; the form warns when it equals or looks like a ticker in the page's
registry (on a testnet-10 page the registry holds no tokens, so there is no warning there, and it never sees tokens the registry
does not list). The covenant id tells tokens apart. Decimals may be 0 to 18, but the market uses at most 9: a token with more
trades with 9 decimals, so choose 9 or fewer.

After creation the page shows the token id and "Open its market". The token appears in the Market list once a first sell order exists;
until then only a wallet that holds it can open it. KOB keeps no listing. The "registry entry" the page offers is optional: apps read
the on-chain record.

**Your tokens are found from your key.** A token created with the current version is listed for your key in any browser. A token
created by an **older** version was known only to the browser that created it. To find such a token again:

1. Open **Settings** (footer: "Settings and data"), section **Find a token you created earlier**.
2. Enter the **Token (covenant id)** and the **Amount your key received at creation** (in whole tokens, as the create form showed it,
   for example 1000). If the token declares no decimals on chain, also enter the **Decimals of the token**.
3. Press **Find it**. The app asks the node whether your key holds an output of that token with exactly that amount; it
   remembers it ("Found N output(s) of this token for your key"). If it says none, check the amount (exactly what the creation
   gave your key) and that this is the wallet that created it.

The **Token tracker** panel of Settings lists the tracked outputs and can export or import them as a backup.

## 13. Backups and local data

The terms of an order exist only in its placement transaction. This browser keeps a **record** of each order so you can cancel it
without the indexer. Under **My orders** > "Recover and back up" press **Export backup** and keep the file. "Import file" takes a
KOB backup or a maker recovery file; every record is re-checked and only orders your key placed are added. "Rescan from my records"
reads them again. If the browser cannot store the records the page warns "Your order records are not saved" and keeps them in the
tab only: export a backup now.

**Settings** also holds the node, indexer and registry URLs, "Local data" (what this browser stores) and "Clear local data". Export a
backup first: orders on chain stay, but without their records the app can only find them through the indexer.

## 14. Words you will meet

| Word | Meaning |
|---|---|
| Covenant id | The 64-hex identity of a token (or of an order). The ticker is not an identity. |
| UTXO | An unspent output: a coin of KAS or a token. A wallet's balance is made of many. |
| Carrier | The KAS that sits in a contract output. Not a fee; returned when the output is spent. |
| Escrow | What an order holds: tokens for a sell, a KAS budget for a buy. |
| Matcher / keeper | Programs run by third parties that fill crossing orders (matcher) and refund expired ones or arm stops (keeper). Anyone may run one. |
| Indexer | The server that follows the chain and lists books, trades and orders. |
| Tip | An optional payment to the matcher, set by you in the ticket. |
| DAA score | Kaspa's block-time counter (10 per second). The app turns it into times such as "in 90 d". |
| Stray | A token output sent to an order from outside. Sweep or cancel to recover it. |
| sompi | The smallest unit of KAS (1 KAS = 100,000,000 sompi). |
| Tick / price step | The smallest price change of a market. |
