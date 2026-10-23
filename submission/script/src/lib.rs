//! Payday companion script: the pot, split in two, paid out to holders every week.
//!
//! Every step (the companion calls it with the creator address signing):
//!   1. Check the launch, the hook setup and the companion account; read the pot.
//!   2. Split the pot's new money: the builder's share (`params::BUILDER_BPS`, a quarter); ALL the
//!      rest goes to the holder pile. The two piles always add up to the pot.
//!   3. Roll the weekly payout round when a new one has begun: the round's budget is the holder
//!      pile at its first step, less a reserve for the step bounties (`ROUND_BOUNTY_RESERVE`).
//!   4. Pay holders (`holders.rs`): each holding the step brings is measured once a round and paid
//!      from the pile by its weight (amount times hold time, 1x at 6 hours, 1.5x at 7 days, 2x at
//!      30 days, 10% more if it never sold) as measured the round before, as a payout answer the
//!      companion pays and the hook records.
//!   5. The builder is paid what it is owed, within the payout terms, as a wallet answer on any
//!      step that answers no holder payouts.
//!
//! The script makes no call to any program: it never trades, burns or adds liquidity. Money only
//! leaves the pot through the answers, which the companion checks against the payout terms and
//! pays. Nothing a step sender passes can change where it goes: the builder wallet is a constant,
//! the holdings are checked against their hook data, and every account is found by key.

#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use bordrless_launch::client as launch_client;
use bordrless_token::client as token_client;

use crate::math::Piles;
use crate::params::{
    BOUNTY_BOUND, BUILDER_BPS, BUILDER_WALLET, MIN_HOLDING, NEEDS_HOOK, PAYOUT_MAX_ENTRIES, RESERVED_KEYS,
    ROUND_SECS,
};
use crate::record::Record;

pub mod holders;
pub mod layout;
pub mod math;
pub mod params;
pub mod record;

// Placeholder: Bordrless writes the real program id when it builds the source.
declare_id!("8uTvZUK6XK9456ckErnjTNuyWfqaTcHYiMwZoSXo2Z9Y");

/// The companion program that calls this script and checks its answers.
pub const COMPANION_ID: Pubkey =
    Pubkey::from_str_const("6ZUM1gWBH9hBBNoJoaVAGwSftyZ6CUda6vUZTW9MsJuo");

// Script state, 185 bytes (keeper.json `stateLen`). Offsets include Anchor's 8-byte discriminator:
//   [0..8]     discriminator
//   [8]        version        u8   always 1
//   [9..41]    mint           Pubkey
//   [41..49]   last_pot       u64  the pot as the companion left it after the previous step
//   [49..57]   owed           u64  the builder's pile: earned, not yet paid
//   [57..65]   holder_pile    u64  the holder payouts' pile
//   [65..73]   paid_builder   u64  lamports paid to the builder wallet, all time
//   [73..81]   holders_paid   u64  lamports paid to holders, all time
//   [81..89]   steps          u64  steps run so far
//   [89..93]   round          u32  the payout round (week) of the last step
//   [93..109]  w_prev         u128 total weight measured the round before
//   [109..125] w_cur          u128 total weight measured this round so far
//   [125..133] round_budget   u64  what this round pays out: the holder pile at its first step,
//                                  less the bounty reserve
//   [133..141] round_paid     u64  paid to holders this round
//   [141..145] measured       u32  holdings measured this round
//   [145..153] round_start    i64  when this round began (unix seconds)
//   [153..185] reserved       32 bytes, zero

/// Version byte written into every initialised state.
pub const STATE_VERSION: u8 = 1;

#[account]
#[derive(InitSpace, Debug)]
pub struct ScriptState {
    pub version: u8,
    pub mint: Pubkey,
    pub last_pot: u64,
    pub owed: u64,
    pub holder_pile: u64,
    pub paid_builder: u64,
    pub holders_paid: u64,
    pub steps: u64,
    pub round: u32,
    pub w_prev: u128,
    pub w_cur: u128,
    pub round_budget: u64,
    pub round_paid: u64,
    pub measured: u32,
    pub round_start: i64,
    pub reserved: [u8; 32],
}

impl ScriptState {
    /// The two piles as the state keeps them.
    pub fn piles(&self) -> Piles {
        Piles {
            owed: self.owed,
            holders: self.holder_pile,
        }
    }

    fn set_piles(&mut self, p: Piles) {
        self.owed = p.owed;
        self.holder_pile = p.holders;
    }

    fn round(&self) -> holders::Round {
        holders::Round {
            round: self.round,
            w_prev: self.w_prev,
            w_cur: self.w_cur,
            budget: self.round_budget,
            paid: self.round_paid,
            measured: self.measured,
        }
    }

    fn set_round(&mut self, r: holders::Round) {
        self.round = r.round;
        self.w_prev = r.w_prev;
        self.w_cur = r.w_cur;
        self.round_budget = r.budget;
        self.round_paid = r.paid;
        self.measured = r.measured;
        self.round_start = holders::round_start(r.round, ROUND_SECS);
    }
}

#[error_code]
pub enum ScriptError {
    #[msg("the launch is not a native-SOL launch")]
    NotNative,
    #[msg("the launch's hook setup does not match this script's variant")]
    WrongHookSetup,
    #[msg("the launch uses kit rules, which this script does not support")]
    Unsupported,
    #[msg("an account is wrong, missing, or owned by the wrong program")]
    BadAccount,
    #[msg("nothing to do: no holding to measure or pay and nothing owed to pay")]
    NothingToDo,
}

#[derive(Accounts)]
#[instruction(mint: Pubkey)]
pub struct CompanionInit<'info> {
    /// The companion's creator address for this mint; only the companion can sign for it.
    #[account(seeds = [b"creator", mint.as_ref()], bump, seeds::program = COMPANION_ID)]
    pub creator: Signer<'info>,
    /// CHECK: the mint, which does not exist yet at init time.
    pub mint_account: UncheckedAccount<'info>,
    /// The state the companion made for this mint: zeroed and owned by this program.
    #[account(zero, seeds = [b"script-state", mint.as_ref()], bump, seeds::program = COMPANION_ID)]
    pub state: Box<Account<'info, ScriptState>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(mint: Pubkey)]
pub struct CompanionStep<'info> {
    /// The companion's creator address for this mint; it holds the pot.
    #[account(seeds = [b"creator", mint.as_ref()], bump, seeds::program = COMPANION_ID)]
    pub creator: Signer<'info>,
    /// CHECK: the mint.
    pub mint_account: UncheckedAccount<'info>,
    /// CHECK: the launch for this mint; read and checked in the handler.
    pub launch: UncheckedAccount<'info>,
    /// CHECK: the companion account for this mint; owner and mint checked, pot read by offset.
    pub companion: UncheckedAccount<'info>,
    /// CHECK: the launch's pool; its key is checked against the launch. Never paid.
    pub pool: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [b"script-state", mint.as_ref()],
        bump,
        seeds::program = COMPANION_ID,
        constraint = state.mint == mint && state.version == STATE_VERSION @ ScriptError::BadAccount,
    )]
    pub state: Box<Account<'info, ScriptState>>,
}

#[program]
pub mod payday_script {
    use super::*;

    /// Called once by the companion when the launch is set up, before the mint exists.
    /// `init_data` must be empty: every setting is a constant in `params.rs`.
    pub fn companion_init(
        ctx: Context<CompanionInit>,
        mint: Pubkey,
        init_data: Vec<u8>,
    ) -> Result<()> {
        require!(init_data.is_empty(), ScriptError::BadAccount);
        let s = &mut ctx.accounts.state;
        s.version = STATE_VERSION;
        s.mint = mint;
        s.reserved = [0u8; 32];
        Ok(())
    }

    /// Called by the companion once per step interval.
    pub fn companion_step<'a>(ctx: Context<'a, CompanionStep<'a>>, mint: Pubkey) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;

        // 1. The launch.
        let launch = launch_client::read_launch(&ctx.accounts.launch.to_account_info())?;
        require_keys_eq!(launch.mint, mint, ScriptError::BadAccount);
        require_keys_eq!(launch.pool, ctx.accounts.pool.key(), ScriptError::BadAccount);
        require!(launch.is_native(), ScriptError::NotNative);
        require!(launch.custom_hook.is_some() == NEEDS_HOOK, ScriptError::WrongHookSetup);
        require!(launch.modules == 0, ScriptError::Unsupported);

        // The companion: check it is this mint's, then read the pot and the launcher's wallet.
        let (pot_before, beneficiary): (u64, Pubkey) = {
            let info = ctx.accounts.companion.to_account_info();
            require_keys_eq!(*info.owner, COMPANION_ID, ScriptError::BadAccount);
            let data = info.try_borrow_data()?;
            let companion_mint =
                layout::read_companion_mint(&data).ok_or(error!(ScriptError::BadAccount))?;
            require_keys_eq!(companion_mint, mint, ScriptError::BadAccount);
            (
                layout::read_pending_pot(&data).ok_or(error!(ScriptError::BadAccount))?,
                layout::read_companion_beneficiary(&data).ok_or(error!(ScriptError::BadAccount))?,
            )
        };

        // The payout terms, if the step carries them (the companion's, of this mint, writable).
        let terms_key =
            Pubkey::find_program_address(&[b"payout-terms", mint.as_ref()], &COMPANION_ID).0;
        let terms: Option<layout::PayoutTermsView> =
            match ctx.remaining_accounts.iter().find(|a| a.key == &terms_key) {
                Some(info)
                    if *info.owner == COMPANION_ID
                        && info.data_len() >= layout::PAYOUT_TERMS_LEN
                        && info.is_writable =>
                {
                    let data = info.try_borrow_data()?;
                    match (
                        layout::read_payout_terms_mint(&data),
                        layout::read_payout_terms(&data),
                    ) {
                        (Some(m), Some(t)) if m == mint => Some(t),
                        _ => None,
                    }
                }
                _ => {
                    msg!("payout terms not in this step: nobody is paid");
                    None
                }
            };

        // 2. The split: new money since the last step, the builder's quarter, all the rest to the
        // holder pile. The piles are then made to add up to the pot exactly.
        let mut piles = ctx.accounts.state.piles();
        let new_money = pot_before.saturating_sub(ctx.accounts.state.last_pot);
        piles.split(new_money, BUILDER_BPS);
        piles.reconcile(pot_before);

        // 3. The payout round.
        let now_round = holders::round_of(now, ROUND_SECS);
        let mut round = holders::roll(ctx.accounts.state.round(), now_round, piles.holders);

        // The owners a payout never goes to.
        let excluded = [
            ctx.accounts.launch.key(),
            launch.pool,
            ctx.accounts.creator.key(),
            ctx.accounts.companion.key(),
            crate::ID,
            ctx.accounts.state.key(),
            terms_key,
            launch.custom_hook.unwrap_or_default(),
            beneficiary,
        ];

        // 4. Holder payouts, then 5. the builder's wallet payout when no holder is answered.
        let mut entries: Vec<(u8, u64, u32)> = Vec::new();
        let mut paid_holders = 0u64;
        let mut pay_builder = 0u64;
        if let Some(ref t) = terms {
            let window_start = math::window_start_now(now, t.window_secs);
            let (window_pot, window_paid) = if t.window_start == window_start {
                (t.window_pot, t.window_paid)
            } else {
                (pot_before, 0u64)
            };
            let budget = math::step_budget(
                pot_before,
                pot_before,
                BOUNTY_BOUND,
                window_pot,
                window_paid,
                t.step_bps,
                t.window_bps,
            );
            let holding_cap = math::bps64(budget, u64::from(t.holding_bps));
            let rent_min = Rent::get()?.minimum_balance(0);
            let mut step_left = budget;
            let mut seen: Vec<Pubkey> = Vec::with_capacity(PAYOUT_MAX_ENTRIES);
            for (i, info) in ctx.remaining_accounts.iter().enumerate() {
                if entries.len() == PAYOUT_MAX_ENTRIES || i > usize::from(u8::MAX) {
                    break;
                }
                // A holding of the mint, passed writable (the hook records into it), once.
                if *info.owner != bordrless_token::ID || !info.is_writable || seen.contains(info.key) {
                    continue;
                }
                let Ok(h) = token_client::read_holding(info) else {
                    continue;
                };
                if h.mint != mint || h.amount < MIN_HOLDING || !eligible(&h.owner, &excluded) {
                    continue;
                }
                let rec = Record::decode(&h.hook_data);
                if !rec.is_set() || rec.amount != h.amount || rec.round == round.round {
                    continue;
                }
                // Measured for this round on the lowest balance it has held during the round, at
                // its hold clock as of the round's first second (a clock started after it counts
                // 0: a touch marks it so it is not brought again until next week). The hook
                // records that same weight as `measured`. Paid for the round before only if it
                // was measured then, on the weight locked at that measurement, scaled down if its
                // balance went lower later in that round (`record::pay_weight`): never over the
                // weight added to that round's total, so the shares paid never add up past the
                // budget.
                let w_now = u128::from(record::measured_weight(&rec, h.amount, now_round));
                let w_prev_basis = if round.w_prev > 0 && rec.round == round.round.wrapping_sub(1) {
                    u128::from(record::pay_weight(&rec, h.amount, rec.round))
                } else {
                    0
                };
                // An owner the companion never pays (a reserved key) is counted for 0, whatever its
                // share and whether or not its account is passed.
                let decision = if RESERVED_KEYS.contains(&h.owner) {
                    holders::Decision::Touch
                } else {
                    holders::decide(&round, w_prev_basis, piles.holders, step_left, holding_cap)
                };
                let mut pay = match decision {
                    holders::Decision::Pay(x) => x,
                    holders::Decision::Touch => 0,
                    holders::Decision::Skip => continue,
                };
                if pay > 0 {
                    // Paid only into an account the companion will take: the holding's owner, a
                    // plain system-owned wallet among the step's accounts, writable, left at or
                    // above the rent-exempt minimum. When the owner is missing from the step, or
                    // is a plain wallet but read-only, the holding is left alone for a step that
                    // carries it right: a sender cannot cost a holder its week by leaving its
                    // wallet out. An owner that can never take a payment (a program, or an
                    // account another program owns), or a wallet whose balance plus the share
                    // stays under the rent-exempt minimum, is touched for 0: marked for the week
                    // so the keeper does not bring it again, its share left in the holder pile.
                    let Some(owner) = ctx.remaining_accounts.iter().find(|a| *a.key == h.owner) else {
                        continue;
                    };
                    let plain = !owner.executable && *owner.owner == anchor_lang::system_program::ID;
                    if plain && !owner.is_writable {
                        continue;
                    }
                    if !plain || !owner.lamports().checked_add(pay).is_some_and(|l| l >= rent_min) {
                        pay = 0;
                    }
                }
                seen.push(*info.key);
                round.w_cur = round.w_cur.saturating_add(w_now);
                round.measured = round.measured.saturating_add(1);
                round.paid = round.paid.saturating_add(pay);
                paid_holders = paid_holders.saturating_add(pay);
                step_left = step_left.saturating_sub(pay);
                piles.holders = piles.holders.saturating_sub(pay);
                entries.push((i as u8, pay, round.round));
            }
            if entries.is_empty() && t.wallets_allowed && piles.owed > 0 {
                let candidate = piles.owed.min(budget).min(holding_cap);
                if candidate > 0 {
                    if let Some(wallet) = ctx.remaining_accounts.iter().find(|a| a.key == &BUILDER_WALLET) {
                        let rent = Rent::get()?;
                        let funded = wallet
                            .lamports()
                            .checked_add(candidate)
                            .is_some_and(|l| l >= rent.minimum_balance(wallet.data_len()));
                        if wallet.is_writable
                            && !wallet.executable
                            && *wallet.owner == anchor_lang::system_program::ID
                            && funded
                        {
                            pay_builder = candidate;
                        }
                    }
                }
            }
        }

        // A step with nothing to measure, pay or answer is refused: it takes no interval slot and
        // pays its sender no bounty, and the pot and the state stay as they were.
        require!(!entries.is_empty() || pay_builder > 0, ScriptError::NothingToDo);

        // The answer: holder payouts, else the builder's wallet payout.
        let answered: Option<u64> = if !entries.is_empty() {
            anchor_lang::solana_program::program::set_return_data(&math::encode_payout_answer(&entries));
            Some(entries.len() as u64)
        } else if pay_builder > 0 {
            let tag = u32::try_from(ctx.accounts.state.steps).unwrap_or(u32::MAX);
            anchor_lang::solana_program::program::set_return_data(&math::encode_wallet_answer(
                &BUILDER_WALLET,
                pay_builder,
                tag,
            ));
            Some(1)
        } else {
            None
        };

        // 6. The state: the pot as the companion leaves it (after the payouts and the sender's
        // bounty, which comes out of the holder pile first), the piles made to add up to it.
        piles.owed = piles.owed.saturating_sub(pay_builder);
        let bounty = math::step_bounty(pot_before, answered);
        piles.take(bounty);
        let pot_left = pot_before
            .saturating_sub(paid_holders)
            .saturating_sub(pay_builder)
            .saturating_sub(bounty);
        piles.reconcile(pot_left);

        let state = &mut ctx.accounts.state;
        state.set_piles(piles);
        state.set_round(round);
        state.last_pot = pot_left;
        state.paid_builder = state.paid_builder.saturating_add(pay_builder);
        state.holders_paid = state.holders_paid.saturating_add(paid_holders);
        state.steps = state.steps.saturating_add(1);
        Ok(())
    }
}

/// Whether `owner` may be paid: none of `excluded`, not the default key, on the ed25519 curve.
pub fn eligible(owner: &Pubkey, excluded: &[Pubkey]) -> bool {
    *owner != Pubkey::default() && !excluded.contains(owner) && owner.is_on_curve()
}
