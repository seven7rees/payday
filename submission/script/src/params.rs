//! The numbers this script runs on. Every setting is a named constant here and nowhere else.
//!
//! Payday: the pot (the companion fee plus the hook's 1% brake) is split every step. The
//! builder's share is a quarter of new pot money (`BUILDER_BPS`); ALL the rest goes to the weekly
//! holder payouts. The script never trades, burns or adds liquidity.

use anchor_lang::prelude::Pubkey;

/// The wallet that receives the builder's share of new pot money.
pub const BUILDER_WALLET: Pubkey =
    Pubkey::from_str_const("AxoN4hpQJWKFkSimhqnA7dUsqf5VGojA5RhhZTiP4KFm");

/// The builder's share of new pot money, in basis points: a quarter.
pub const BUILDER_BPS: u16 = 2_500;

/// Upper bound, in lamports, on what the companion pays the step's sender from the pot.
/// The companion's own limit is 20_000.
pub const BOUNTY_BOUND: u64 = 100_000;

/// The coin must have this script's hook: its records are what the payouts are measured by.
pub const NEEDS_HOOK: bool = true;

// ─── Holder payouts: weekly, weighted by amount and by hold time ─────────────────────────────

/// A payout round: a week. The keeper's epoch for the hook data's round field is the same number.
pub const ROUND_SECS: u32 = 604_800;
/// The least balance a holding is counted at: 1,000 coins (6 decimals). A holding under it is
/// never measured or paid; keeper.json `holdings.minBalance` brings the same.
pub const MIN_HOLDING: u64 = 1_000_000_000;
/// A holding earns nothing before its hold clock reaches this: 6 hours.
pub const MIN_HOLD_SECS: i64 = 21_600;
/// Hold-time weights, in basis points of the balance: 1x from 6 hours, 1.5x from 7 days, 2x from
/// 30 days. Tiers in rising order.
pub struct WeightTier {
    pub from_secs: i64,
    pub bps: u16,
}
pub const WEIGHT_TIERS: [WeightTier; 3] = [
    WeightTier { from_secs: 21_600, bps: 10_000 },
    WeightTier { from_secs: 604_800, bps: 15_000 },
    WeightTier { from_secs: 2_592_000, bps: 20_000 },
];
/// A holding that has not sold into the pool since it was funded weighs this much more, in basis
/// points of its weight: 10%. Set to 0 to turn the bonus off.
pub const NEVER_SOLD_BONUS_BPS: u16 = 1_000;
/// The most holdings one step pays or measures, under the companion's own limit of 16. The keeper
/// brings what fits one transaction (keeper.json `perStep`), so a step pays at most that many in
/// practice.
pub const PAYOUT_MAX_ENTRIES: usize = 8;
/// A payout under this many lamports is not sent; the holding is measured (touched) instead
/// and its weight still counts: 0.0001 SOL.
pub const MIN_PAYOUT_LAMPORTS: u64 = 100_000;
/// The steps a round holds at the keeper's interval: 604,800 / 300 (keeper.json `stepSecs`).
pub const STEPS_PER_ROUND: u64 = 2_016;
/// The companion's flat step bounty, in lamports (`SCRIPT_BOUNTY`), the most one step pays its
/// sender from the pot.
pub const STEP_BOUNTY: u64 = 20_000;
/// What a round's budget keeps back for the step bounties the holder pile pays over the round:
/// `STEPS_PER_ROUND` bounties, 0.04032 SOL. Every step's bounty comes out of the holder pile;
/// without this reserve the last holder paid in a round could find the pile a few bounties short
/// of the share the budget promised. A pile under the reserve pays nobody that round and carries
/// over.
pub const ROUND_BOUNTY_RESERVE: u64 = STEPS_PER_ROUND * STEP_BOUNTY;

/// The runtime's reserved keys (the builtin programs, the sysvars and the sysvar program). Every
/// transaction demotes them to read-only, so the companion never pays them (its `payable`): a
/// holding one of them owns is counted for 0 when its share falls due.
pub const RESERVED_KEYS: [Pubkey; 31] = [
    Pubkey::from_str_const("AddressLookupTab1e1111111111111111111111111"),
    Pubkey::from_str_const("BPFLoader2111111111111111111111111111111111"),
    Pubkey::from_str_const("BPFLoader1111111111111111111111111111111111"),
    Pubkey::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111"),
    Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
    Pubkey::from_str_const("Config1111111111111111111111111111111111111"),
    Pubkey::from_str_const("Ed25519SigVerify111111111111111111111111111"),
    Pubkey::from_str_const("Feature111111111111111111111111111111111111"),
    Pubkey::from_str_const("LoaderV411111111111111111111111111111111111"),
    Pubkey::from_str_const("KeccakSecp256k11111111111111111111111111111"),
    Pubkey::from_str_const("Secp256r1SigVerify1111111111111111111111111"),
    Pubkey::from_str_const("StakeConfig11111111111111111111111111111111"),
    Pubkey::from_str_const("Stake11111111111111111111111111111111111111"),
    Pubkey::from_str_const("11111111111111111111111111111111"),
    Pubkey::from_str_const("Vote111111111111111111111111111111111111111"),
    Pubkey::from_str_const("ZkE1Gama1Proof11111111111111111111111111111"),
    Pubkey::from_str_const("ZkTokenProof1111111111111111111111111111111"),
    Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111"),
    Pubkey::from_str_const("SysvarEpochRewards1111111111111111111111111"),
    Pubkey::from_str_const("SysvarEpochSchedu1e111111111111111111111111"),
    Pubkey::from_str_const("SysvarFees111111111111111111111111111111111"),
    Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111"),
    Pubkey::from_str_const("SysvarLastRestartS1ot1111111111111111111111"),
    Pubkey::from_str_const("SysvarRecentB1ockHashes11111111111111111111"),
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111"),
    Pubkey::from_str_const("SysvarRewards111111111111111111111111111111"),
    Pubkey::from_str_const("SysvarS1otHashes111111111111111111111111111"),
    Pubkey::from_str_const("SysvarS1otHistory11111111111111111111111111"),
    Pubkey::from_str_const("SysvarStakeHistory1111111111111111111111111"),
    Pubkey::from_str_const("NativeLoader1111111111111111111111111111111"),
    Pubkey::from_str_const("Sysvar1111111111111111111111111111111111111"),
];
