// Setup core: one TN10 transaction (funded + signed by the DEV key) that creates, for a wallet user's pubkey W:
//   * N KCC-20 token UTXOs (reference template, fixed supply = N * tokenUnits, owner scheme 0x00 = W,
//     borrow disabled, 0x04 enabled in the template's config), all in ONE covenant group (same covenant id);
//   * M BidOrder UTXOs whose maker key is W (each its own covenant id, holding a little KAS);
//   * K plain P2PK UTXOs paying W's address (for test T1 and fees).
import * as S from './script.mjs';
import * as C from './contracts.mjs';
import * as T from './txbuild.mjs';

const COINBASE_MATURITY = 1010n; // tn10: 1000 DAA + safety

export async function runSetup({
  k, rpc, devKey, walletPubkey, kcc20Artifact, bidTemplate,
  tokens = 4, tokenUnits = 1000n, tokenCarrier = 2n * C.KAS,
  bids = 4, bidValue = 3n * C.KAS,
  fundCount = 3, fundValue = 5n * C.KAS,
  log = console.log,
}) {
  const network = 'testnet';
  const devPub = devKey.toPublicKey().toXOnlyPublicKey().toString();
  const devAddr = devKey.toAddress(network).toString();
  const walletAddr = C.addressOfPubkey(k, walletPubkey);
  const tokenAbi = C.loadToken(kcc20Artifact);

  // ---- dev funding UTXOs (mature only)
  const dag = await rpc.getBlockDagInfo();
  const vdaa = BigInt(dag.virtualDaaScore);
  const all = await T.utxosOf(rpc, devAddr);
  const mature = all.map((e) => T.utxoToInput(e, 12)).filter((u) => !u.isCoinbase || vdaa - u.daa >= COINBASE_MATURITY);
  log(`dev ${devAddr}: ${all.length} utxos, ${mature.length} spendable, ${Number(mature.reduce((a, u) => a + u.amount, 0n)) / 1e8} KAS`);

  const need = BigInt(tokens) * tokenCarrier + BigInt(bids) * bidValue + BigInt(fundCount) * fundValue + 2n * C.KAS;
  const { chosen } = T.selectUtxos(mature, need);
  const inputs = chosen.map((u) => ({ ...u, budget: 12 }));
  const genesisOutpoint = { transactionId: inputs[0].txid, index: inputs[0].index };

  // ---- outputs
  const tokenStateFor = (owner) => C.kcc20State({ amount: tokenUnits, owner, ownerScheme: C.OWNER_P2PK_SCHNORR, ext: C.EXT_HEX });
  const tokenRedeem = C.kcc20Redeem(tokenAbi, tokenStateFor(walletPubkey));
  const tokenSpk = C.p2shSpk(k, tokenRedeem);
  const tokenOuts = Array.from({ length: tokens }, () => ({ value: tokenCarrier, spk: tokenSpk.script }));
  // token covenant id = H(genesis outpoint, authorised outputs) ; computed from a temp tx to feed the bids
  const tmpTx = T.toWasmTx(k, { inputs, outputs: tokenOuts });
  tmpTx.populateGenesisCovenants([{ authorizingInput: 0, outputs: tokenOuts.map((_, i) => i) }]);
  const tokenCovId = tmpTx.outputs[0].covenant.covenantId.toString();
  const check = k.covenantId(genesisOutpoint, tmpTx.outputs.map((output, index) => ({ index, output })));
  if (check.toString() !== tokenCovId) throw new Error('covenant id mismatch (populate vs covenantId())');
  log('token covenant id', tokenCovId);

  const bid = C.bidRedeem(bidTemplate, { maker: walletPubkey, tokenCovId });
  const bidSpk = C.p2shSpk(k, bid.redeem);
  const bidOuts = Array.from({ length: bids }, () => ({ value: bidValue, spk: bidSpk.script }));
  const fundOuts = Array.from({ length: fundCount }, () => ({ value: fundValue, spk: C.p2pkScriptHex(walletPubkey) }));
  const changeOut = { value: 0n, spk: C.p2pkScriptHex(devPub) };
  const outputs = [...tokenOuts, ...bidOuts, ...fundOuts, changeOut];
  const plan = { inputs, outputs };
  const fee = T.minFee(plan, inputs.map(() => 66));
  const inSum = inputs.reduce((a, u) => a + u.amount, 0n);
  const outSum = outputs.reduce((a, o) => a + o.value, 0n);
  changeOut.value = inSum - outSum - fee;
  if (changeOut.value < C.KAS) throw new Error('change too small; fund the dev address more');
  const sm = k.calculateStorageMass('testnet-10', inputs.map((u) => Number(u.amount)), outputs.map((o) => Number(o.value)));
  log(`inputs ${inputs.length}, outputs ${outputs.length}, fee ${fee} sompi, storage mass ${sm}`);

  const tx = T.toWasmTx(k, plan);
  const groups = [{ authorizingInput: 0, outputs: tokenOuts.map((_, i) => i) }];
  bidOuts.forEach((_, i) => groups.push({ authorizingInput: 0, outputs: [tokens + i] }));
  tx.populateGenesisCovenants(groups);
  if (tx.outputs[0].covenant.covenantId.toString() !== tokenCovId) throw new Error('token covenant id changed after populate');

  // ---- sign P2PK inputs with the dev key
  for (let i = 0; i < inputs.length; i++) {
    const sigscript = T.signP2pkInput(k, tx, i, devKey);
    tx.inputs[i].signatureScript = sigscript;
  }
  const txid = await T.submit(rpc, tx);
  log('submitted genesis tx', txid);
  const acc = await T.waitAccepted(rpc, C.addressOfPubkey(k, devPub), txid, outputs.length - 1);
  log('accepted:', acc);
  if (!acc.accepted) throw new Error('genesis tx not accepted in time');

  const addrOf = (spk) => k.addressFromScriptPublicKey(new k.ScriptPublicKey(0, spk), 'testnet-10').toString();
  const state = {
    schema: 'kob-wallet-gate-setup/1',
    network: 'testnet-10',
    createdAt: new Date().toISOString(),
    genesisTxid: txid,
    devAddress: devAddr,
    walletPubkey, walletAddress: walletAddr, recipientPubkey: devPub,
    tokenCovId, extHex: C.EXT_HEX, tokenUnits: tokenUnits.toString(),
    tokenTemplateHash: S.hex(Uint8Array.from(kcc20Artifact.contracts.KCC20.compiled.template_hash)),
    tokens: tokenOuts.map((o, i) => ({ txid, index: i, value: o.value.toString(), amount: tokenUnits.toString(), address: addrOf(o.spk) })),
    bids: bidOuts.map((o, i) => ({ txid, index: tokens + i, value: o.value.toString(), covenantId: tx.outputs[tokens + i].covenant.covenantId.toString(), address: addrOf(o.spk) })),
    funds: fundOuts.map((o, i) => ({ txid, index: tokens + bids + i, value: o.value.toString() })),
  };
  return state;
}
