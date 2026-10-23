# Payday: a Bordrless Workshop pair, ready to submit

Builder: Patrick (@TetonTrading). Payout address (public): AxoN4hpQJWKFkSimhqnA7dUsqf5VGojA5RhhZTiP4KFm.
Item name: Payday. Kind: pair (Both). Builder's share at submission: None (author share 0).
The builder's only earning is the pot cut written into the script (BUILDER_WALLET and
BUILDER_BPS in script/src/params.rs), so whoever's wallet submits changes nothing about who is paid.

This REPLACES the Dip Vault pair sent earlier (not deployed yet). Same hold-clock hook; the
script now does one job: weekly SOL to holders.

## The files, exactly as the submit page takes them

- hook/src/lib.rs, hook/src/clock.rs, and hook/hook.json beside them (SOL cut caps: buys 1 bps,
  always answered 0; sells 80 bps). One declare_id!, the standard prepare.
- script/src/*.rs (6 files) and script/keeper.json beside them.
- notes.txt: the builder's words, under 600 characters.

Both halves build on the Workshop templates with no added crate. The declare_id! values are local
build addresses; the builder writes the real ones.

## What it does

The hook keeps a hold clock in every holding (weighted-average entry time). It answers 80 bps of
a sell's SOL only while the seller's clock is under 10 minutes, 0 after, 0 on buys; a sell's
real source clock is relayed through the hook state so a delegate sale cannot dodge it.
record_payout, signed only by the companion's payout authority, records what a holding was paid.
The hook's registry lists only its state.

The hook also keeps each holding's lowest balance during the current weekly round and the round
before (record bytes 46..64), so a top-up mid-week earns nothing extra that week, and locks the
holding's share when it is measured (record bytes 29..46: the balance counted and the weight),
so a later buy cannot change it and a later sell can only lower it.

The script keeps the pot in two piles that always equal the companion's pot: the builder's
quarter of new pot money, and the holders' pile (all the rest). Weekly rounds: a holding of at
least MIN_HOLDING (1,000 coins, checked in the code as well as in keeper.json) is measured once a
round by its lowest balance that round and paid the round after by weight (1x at 6 hours, 1.5x
at 7 days, 2x at 30 days, +10% never sold), through the companion's payout answer. The round's
budget holds back one round of sender bounties. A step that answers nothing is refused
(NothingToDo, as loss_cover_script does), so it earns no bounty. The script never trades, burns
or adds liquidity.

## What has been checked (and what has not)

- Host tests pass (layouts pinned against the programs' own crates at c6676977, the split, the
  weights, every rule and bound).
- LiteSVM, the whole story, against the programs built from the pinned source AND against the
  live mainnet program bytes: the brake (79 to 80 bps of the SOL out), a free late sell, a
  delegate sale paying the brake, the builder paid exactly a quarter, 19 holders measured one
  week and paid the next by their exact weights, a step with nobody due paying nobody, piles
  equal to the pot after every step, every lamport claimed accounted for to the lamport.
- LiteSVM, the weight tiers on chain: five equal bags held 1 day, 8 days and 40 days (one of
  them sold a little) and under 6 hours, paid exactly budget x weight / total: 1x, 1.5x, 2x,
  the sold bag 1/1.1 of its never-sold peer, the under-6-hour bag 0.
- LiteSVM, hostile steps: an empty step refused with nothing paid; a holding under 1,000 coins
  never touched; a mid-week 100,000-coin top-up credited only its lowest balance; a holding
  passed twice paid once; a payout held back when the owner wallet is missing or read-only and
  paid on a later step; a holding that sells part after being measured paid on the week it
  held; a wallet too empty to receive its share counted for 0 (its share stays in the pot), with
  the rent edge checked to the lamport; a program-owned owner counted for 0; a 50,000-coin buy in
  the payout week leaving the locked share unchanged; coins moved between wallets counted once;
  a step of only the pool, creator and launcher holdings refused; a holding owned by one of the
  runtime's reserved keys counted for 0 (the companion never pays those keys).
- keeper.json passes the SDK's parseKeeperDeclaration (bordrless-sdk 2c77a82).
- Not run: the Workshop's own rule scan, build, trading test and reading. Not audited.

## Things to confirm on your side

1. Compute: a step paying 4 holdings measured up to about 238k CU (LiteSVM), above
   the 200k default for one instruction. The keeper needs to set a compute limit on paying steps.
2. Holder reach: 4 holdings fit a step (1,232-byte packet, protocol lookup table, no swap
   accounts), so keeper.json asks for 4 a step: about 8,000 holders a week at 5-minute steps.
3. record_payout follows the pair brief's pattern 4 (as loss_cover_hook does).
4. Empty steps are refused (NothingToDo). Please confirm the keeper moves on after a refused
   simulation, as it does for loss_cover_script.
