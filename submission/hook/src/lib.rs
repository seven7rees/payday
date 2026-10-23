//! Payday hook: a hold clock in every holding, a short sell brake in SOL, and the payout records
//! the paired companion script reads and the companion program writes to.
//!
//! # What it does
//! * Buys pay no SOL cut. A sell pays `BRAKE_BPS` (80 bps; the launchpad adds a quarter, so about
//!   1% of the SOL out) only while the seller's hold clock is under `BRAKE_SECS` (10 minutes).
//!   After that a sell pays nothing to this hook.
//! * The hold clock (`clock::Record.stamp`) is a weighted-average entry time: coins arriving in an
//!   empty holding start it at now; a top-up moves it toward now in proportion to the amount;
//!   sending or selling part of a holding leaves it; emptying a holding clears it.
//! * Each record keeps the lowest balance its holding held during the current weekly round
//!   (`clock::Record.low`), so a payout counts only coins held through the whole round.
//! * `before_transfer` records a sell's real source clock in the hook's state (the relay) so the
//!   brake keys on the holding the coins leave, not on a delegate's holding. `quote_cut` reads it.
//! * `record_payout` lets the companion program (its payout authority alone) record what it paid a
//!   holding, the round it measured it in, the balance it counted and the weight that gives
//!   (`clock::measured_weight`, the same rule and numbers as the paired script). The script names
//!   who is paid; this hook only keeps the records. It never moves SOL and never refuses a transfer.
//!
//! # hook.json
//! `{ "version": 1, "quoteCut": { "maxBuyBps": 1, "maxSellBps": 80 } }`: the launchpad asks on
//! every buy (answered 0) and caps sells at the brake.
//!
//! # State layout (PDA ["state", mint] under this program; 242 bytes, little-endian)
//! offset   0 : discriminator [u8; 8]
//! offset   8 : version       u8      1
//! offset   9 : bump          u8
//! offset  10 : mint          Pubkey
//! offset  42 : pool          Pubkey  the launch pool (derived at prepare: its sends are buys)
//! offset  74 : launch        Pubkey  the launch account (never holds a record)
//! offset 106 : creator       Pubkey  the companion's creator address (never holds a record)
//! offset 138 : burned        u64     tokens burned on this mint since launch
//! offset 146 : paid_total    u64     lamports recorded by record_payout
//! offset 154 : records       u64     record_payout calls (touches included)
//! offset 162 : relay_slot    u64     the slot of the last sell transfer
//! offset 170 : relay_stamp   i64     the hold clock of the holding that sell left
//! offset 178 : relay_owner   Pubkey  the owner of that holding
//! offset 210 : reserved      [u8; 32]
//!
//! # Hook data layout: see `clock.rs` (shared with the companion script, byte for byte).
//!
//! # Registry (one extra on every transfer)
//! [5] state (writable).

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use bordrless_game::{
    companion_creator_address, eligible, launch_address, read_holding, write_own_hook_data,
    LAUNCH_PROGRAM_ID, TOKEN_EVENT_AUTHORITY, TOKEN_PROGRAM_ID,
};
use bordrless_hook::{
    hook_accounts_address, hook_signer, write_registry, AccountSource,
    ExtraAccount, HookAccountList, HookReturn, Phase, PoolHookArgs, Seed, TokenHookArgs, TokenOp,
};

pub mod clock;

use clock::{
    brake_bps, low_of, measured_weight, on_receive, on_send, round_of, seller_stamp, Record, FREE_BPS,
};

// Placeholder program id: Bordrless writes the real one when it builds the source.
declare_id!("2bpmtMTxHkfsp4vADQxXG6EKmyycCA2CmkpdZV9ZaJC7");

/// Layout version of [`HookState`].
pub const VERSION: u8 = 1;
/// State PDA seed: `["state", mint]` under this program.
pub const STATE_SEED: &[u8] = b"state";
/// The launchpad's seed for its `quote_cut` signer: `["quote-cut", hook]`.
pub const QUOTE_CUT_SEED: &[u8] = b"quote-cut";
/// The Bordrless DEX, whose `["pool", mint, quote, lp_fee, launchpad]` is a launch's pool.
pub const SWAP_ID: Pubkey = Pubkey::from_str_const("GyzKSnnEu2uN5bBRecE4XYY2enbfR2D2MtxnbJPGy7hk");
/// The DEX's pool seed.
pub const POOL_SEED: &[u8] = b"pool";
/// Bridged SOL, every launch's quote.
pub const BRIDGED_SOL_MINT: Pubkey =
    Pubkey::from_str_const("A49oVhX22ExMwTEtFC6Y8nhBdZ4LJDGhdXLDn4c2f59i");
/// The LP fee every launch pool is made with (the launchpad config's), a seed of the pool.
pub const LAUNCH_LP_FEE_BPS: u16 = 30;
/// The companion program's `PDA(["payout-authority"])`: the one signer of `record_payout`.
pub const PAYOUT_AUTHORITY: Pubkey =
    Pubkey::from_str_const("8XJcBCYmYyggTQBNvK3RausfyRMpmtjJTjKd53W3StkG");
/// The SOL cut caps, the same numbers `hook.json` declares: the launchpad asks on every buy
/// (answered 0), and a sell is capped at the brake.
pub const MAX_BUY_BPS: u16 = 1;
pub const MAX_SELL_BPS: u16 = clock::BRAKE_BPS;
/// Swap direction value meaning quote to base (a buy).
const DIRECTION_BUY: u8 = 1;

/// This program's `["hook-authority"]`, which signs the token program's `write_hook_data`.
pub fn own_hook_authority() -> Pubkey {
    bordrless_hook::hook_authority(&crate::ID).0
}

/// The launch pool of `mint`: `["pool", mint, bridged SOL, lp fee, launchpad]` under the DEX.
pub fn pool_address(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            POOL_SEED,
            mint.as_ref(),
            BRIDGED_SOL_MINT.as_ref(),
            &LAUNCH_LP_FEE_BPS.to_le_bytes(),
            LAUNCH_PROGRAM_ID.as_ref(),
        ],
        &SWAP_ID,
    )
    .0
}

/// The registry's one extra: the state (writable). Seed Account(1) is the mint in the
/// token-hook prefix.
pub fn extra_accounts(_mint: &Pubkey) -> HookAccountList {
    HookAccountList::new(vec![ExtraAccount {
        writable: true,
        source: AccountSource::Pda {
            program: crate::ID,
            seeds: vec![Seed::Literal(STATE_SEED.to_vec()), Seed::Account(1)],
        },
    }])
}

#[program]
pub mod payday_hook {
    use super::*;

    /// Standard `prepare`: creates the hook state for `mint` and writes the registry. The launch
    /// sends it for every new mint; anyone may send it, once per mint.
    pub fn prepare(ctx: Context<Prepare>) -> Result<()> {
        let mint = ctx.accounts.mint.key();
        let state = &mut ctx.accounts.state;
        state.version = VERSION;
        state.bump = ctx.bumps.state;
        state.mint = mint;
        state.pool = pool_address(&mint);
        state.launch = launch_address(&mint);
        state.creator = companion_creator_address(&mint);
        state.burned = 0;
        state.paid_total = 0;
        state.records = 0;
        state.relay_slot = 0;
        state.relay_stamp = 0;
        state.relay_owner = Pubkey::default();
        state.reserved = [0; 32];

        let (registry_addr, registry_bump) = hook_accounts_address(&crate::ID, &mint);
        require_keys_eq!(
            ctx.accounts.registry.key(),
            registry_addr,
            HookError::WrongAccount
        );
        write_registry(
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.registry.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            &crate::ID,
            &mint,
            registry_bump,
            &extra_accounts(&mint),
        )
    }

    /// Every transfer: the sender's record cut to what it keeps (a sell sets its sold flag and the
    /// relay), the receiver's clock started or blended. Never refuses a transfer, takes no tokens.
    pub fn before_transfer(ctx: Context<Callback>, args: TokenHookArgs) -> Result<HookReturn> {
        check_call(&ctx, &args)?;
        if args.op != TokenOp::Transfer {
            return Ok(HookReturn::default());
        }
        let clock = Clock::get()?;
        let now = clock.unix_timestamp;
        let state = &mut ctx.accounts.state;
        let excluded = state.excluded();
        let sell = args.destination_owner == state.pool;
        let mut answer = HookReturn::default();
        let source = Record::decode(&args.source_hook_data);
        if sell {
            // A sell: remember the clock of the holding the coins leave, for quote_cut.
            state.relay_slot = clock.slot;
            state.relay_stamp = source.stamp_opt().unwrap_or(now);
            state.relay_owner = args.source_owner;
        }
        if eligible(&args.source_owner, &excluded) {
            let mut r = source;
            on_send(
                &mut r,
                args.source_balance,
                args.source_balance.saturating_sub(args.amount),
                sell,
                now,
            );
            answer.source_hook_data = Some(r.encode());
        }
        if eligible(&args.destination_owner, &excluded) {
            let mut r = Record::decode(&args.destination_hook_data);
            on_receive(&mut r, args.amount, args.destination_balance, now);
            answer.destination_hook_data = Some(r.encode());
        }
        Ok(answer)
    }

    /// Every burn: the burner's record cut to what it keeps, the burned counter grown.
    pub fn before_burn(ctx: Context<Callback>, args: TokenHookArgs) -> Result<HookReturn> {
        check_call(&ctx, &args)?;
        if args.op != TokenOp::Burn {
            return Ok(HookReturn::default());
        }
        let now = Clock::get()?.unix_timestamp;
        let state = &mut ctx.accounts.state;
        state.burned = state.burned.saturating_add(args.amount);
        let mut answer = HookReturn::default();
        if eligible(&args.source_owner, &state.excluded()) {
            let mut r = Record::decode(&args.source_hook_data);
            on_send(
                &mut r,
                args.source_balance,
                args.source_balance.saturating_sub(args.amount),
                false,
                now,
            );
            answer.source_hook_data = Some(r.encode());
        }
        Ok(answer)
    }

    /// The launchpad's SOL-cut callback on every buy (Before) and sell (After) of a coin launched
    /// with this hook. A buy: 0. A sell before the After phase: 0. A sell after it: the brake
    /// when the seller's hold clock is under `BRAKE_SECS`, else 0. Only reads; never fails for a
    /// call the launchpad signs; never panics.
    pub fn quote_cut(ctx: Context<QuoteCut>, args: PoolHookArgs) -> Result<u16> {
        let (expected_signer, _) =
            Pubkey::find_program_address(&[QUOTE_CUT_SEED, crate::ID.as_ref()], &LAUNCH_PROGRAM_ID);
        require!(
            ctx.accounts.signer.is_signer && ctx.accounts.signer.key() == expected_signer,
            HookError::NotLaunchpad
        );
        if args.direction == DIRECTION_BUY || args.phase != Phase::After {
            return Ok(FREE_BPS);
        }
        let clock = Clock::get()?;
        let actor_stamp = read_holding(&ctx.accounts.actor_holding)
            .filter(|h| h.mint == args.base_mint)
            .and_then(|h| Record::decode(&h.hook_data).stamp_opt());
        let state = &ctx.accounts.state;
        let stamp = seller_stamp(state.relay_slot, state.relay_stamp, clock.slot, actor_stamp);
        Ok(brake_bps(clock.unix_timestamp, stamp))
    }

    /// The companion program's payout authority alone: records `lamports` paid to `holding` (0: a
    /// touch, the holding measured) in round `tag`, the balance it was counted at (the lowest it
    /// held this round so far, `low_of`) and the weight that balance gives at this round's first
    /// second (`measured_weight`): the same rule and numbers the script measures by, so the weight
    /// recorded is the weight the script added to the round's total. The round's lowest-balance
    /// tracker is kept. The holding is of this mint and its owner may hold a record; the record is
    /// written through the token program.
    pub fn record_payout(ctx: Context<RecordPayout>, lamports: u64, tag: u32) -> Result<()> {
        let mint_key = ctx.accounts.mint.key();
        let holding = read_holding(&ctx.accounts.holding).ok_or(HookError::WrongAccount)?;
        require_keys_eq!(holding.mint, mint_key, HookError::WrongAccount);
        let state = &mut ctx.accounts.state;
        require!(
            eligible(&holding.owner, &state.excluded()),
            HookError::NotEligible
        );
        let mut r = Record::decode(&holding.hook_data);
        require!(r.is_set(), HookError::NoRecord);
        r.paid = r.paid.checked_add(lamports).ok_or(HookError::MathOverflow)?;
        r.round = tag;
        let round = round_of(Clock::get()?.unix_timestamp);
        r.counted = low_of(&r, holding.amount, round);
        r.measured = measured_weight(&r, holding.amount, round);
        state.paid_total = state.paid_total.saturating_add(lamports);
        state.records = state.records.saturating_add(1);
        write_own_hook_data(
            &crate::ID,
            &ctx.accounts.hook_authority.to_account_info(),
            &ctx.accounts.mint.to_account_info(),
            &ctx.accounts.holding.to_account_info(),
            &ctx.accounts.token_event_authority.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            r.encode(),
        )
    }
}

/// A token-hook callback is genuine when the token program's signer for this hook signs it and
/// `args.mint` is the mint the state was prepared for.
fn check_call(ctx: &Context<Callback>, args: &TokenHookArgs) -> Result<()> {
    let signer = &ctx.accounts.hook_signer;
    require!(signer.is_signer, HookError::BadHookSigner);
    require_keys_eq!(
        signer.key(),
        hook_signer(&TOKEN_PROGRAM_ID, &crate::ID).0,
        HookError::BadHookSigner
    );
    require_keys_eq!(args.mint, ctx.accounts.state.mint, HookError::WrongAccount);
    Ok(())
}

/// Hook state for one mint. PDA `["state", mint]` under this program; 242 bytes (see the module).
#[account]
#[derive(InitSpace)]
pub struct HookState {
    pub version: u8,
    pub bump: u8,
    pub mint: Pubkey,
    /// The launch pool: a transfer out of it is a buy, one into it a sell.
    pub pool: Pubkey,
    /// The launch account: never holds a record.
    pub launch: Pubkey,
    /// The companion's creator address: never holds a record.
    pub creator: Pubkey,
    /// Tokens burned on this mint since launch (saturating).
    pub burned: u64,
    /// Lamports recorded by `record_payout`, and entries recorded (touches included).
    pub paid_total: u64,
    pub records: u64,
    /// The relay: the slot, hold clock and owner of the holding the last sell left.
    pub relay_slot: u64,
    pub relay_stamp: i64,
    pub relay_owner: Pubkey,
    pub reserved: [u8; 32],
}

impl HookState {
    /// Account size.
    pub const LEN: usize = 8 + Self::INIT_SPACE;

    /// The owners that never hold a record, besides the default key and addresses off the curve.
    pub fn excluded(&self) -> [Pubkey; 3] {
        [self.launch, self.pool, self.creator]
    }
}

/// Accounts of the standard `prepare`.
#[derive(Accounts)]
pub struct Prepare<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the mint the hook is prepared for; need not exist yet.
    pub mint: UncheckedAccount<'info>,
    #[account(
        init,
        payer = payer,
        space = HookState::LEN,
        seeds = [STATE_SEED, mint.key().as_ref()],
        bump,
    )]
    pub state: Box<Account<'info, HookState>>,
    /// CHECK: registry PDA `["bordrless-hook-accounts", mint]`; address verified in the handler.
    #[account(mut)]
    pub registry: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Accounts of every token-hook callback: the token program's five, then the registry's state.
#[derive(Accounts)]
pub struct Callback<'info> {
    /// CHECK: the token program's signer for this hook; verified in `check_call`.
    pub hook_signer: UncheckedAccount<'info>,
    /// CHECK: the token mint.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: source holding (balances, owners and hook data come from the arguments).
    pub source: UncheckedAccount<'info>,
    /// CHECK: destination holding (the mint for a burn).
    pub destination: UncheckedAccount<'info>,
    /// CHECK: who signed the operation.
    pub authority: UncheckedAccount<'info>,
    #[account(mut, seeds = [STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Box<Account<'info, HookState>>,
}

/// Accounts of the launchpad's `quote_cut`, in the launchpad's order.
#[derive(Accounts)]
pub struct QuoteCut<'info> {
    /// CHECK: launchpad's `["quote-cut", hook]` signer; verified in the handler.
    pub signer: UncheckedAccount<'info>,
    /// CHECK: the launch account.
    pub launch: UncheckedAccount<'info>,
    /// CHECK: the pool.
    pub pool: UncheckedAccount<'info>,
    /// CHECK: the base (token) mint.
    pub base_mint: UncheckedAccount<'info>,
    /// CHECK: the quote (SOL-side) mint.
    pub quote_mint: UncheckedAccount<'info>,
    /// CHECK: the trader.
    pub actor: UncheckedAccount<'info>,
    /// CHECK: the trader's holding of the token, read for its hook data (owner-checked when read).
    pub actor_holding: UncheckedAccount<'info>,
    /// The hook's state (passed writable by the launchpad; only read here).
    #[account(seeds = [STATE_SEED, base_mint.key().as_ref()], bump = state.bump)]
    pub state: Box<Account<'info, HookState>>,
}

/// Accounts of `record_payout`, in the order the companion program builds them.
#[derive(Accounts)]
pub struct RecordPayout<'info> {
    /// The companion program's `PDA(["payout-authority"])`, signing by CPI.
    #[account(address = PAYOUT_AUTHORITY @ HookError::NotTheCompanion)]
    pub payout_authority: Signer<'info>,
    /// CHECK: the mint (the token program checks its hook is this program).
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the holding, read as the token program's and of `mint`; the token program writes it.
    #[account(mut)]
    pub holding: UncheckedAccount<'info>,
    #[account(mut, seeds = [STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Box<Account<'info, HookState>>,
    /// CHECK: this program's `["hook-authority"]`, which signs the token program's write.
    #[account(address = own_hook_authority() @ HookError::WrongAccount)]
    pub hook_authority: UncheckedAccount<'info>,
    /// CHECK: the token program.
    #[account(address = TOKEN_PROGRAM_ID @ HookError::WrongAccount)]
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: the token program's event authority.
    #[account(address = TOKEN_EVENT_AUTHORITY @ HookError::WrongAccount)]
    pub token_event_authority: UncheckedAccount<'info>,
}

#[error_code]
pub enum HookError {
    #[msg("an account is not the one this mint's hook expects")]
    WrongAccount,
    #[msg("the hook signer is not the token program's signer for this hook")]
    BadHookSigner,
    #[msg("the caller is not the launchpad's signer for this hook")]
    NotLaunchpad,
    #[msg("only the companion program's payout authority records a payout")]
    NotTheCompanion,
    #[msg("this owner holds no record: the launch, its pool, the companion's creator address, or an address off the curve")]
    NotEligible,
    #[msg("this holding has no record to pay against")]
    NoRecord,
    #[msg("arithmetic overflow")]
    MathOverflow,
}
