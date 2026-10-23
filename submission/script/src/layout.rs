//! Foreign-account readers: byte-offset access to `Companion` and `PayoutTerms`.
//!
//! The script builds without the `bordrless-companion` crate, so every
//! layout is read by hand, as plain byte offsets into the account data. Offsets count the 8-byte
//! Anchor discriminator. Each reader returns `None` when the data is too short.
//!
//! # Companion layout (Borsh, with 8-byte Anchor discriminator)
//!
//! ```text
//! [0..7]   discriminator (8 bytes)
//! [8]      version        u8
//! [9]      bump           u8
//! [10]     creator_bump   u8
//! [11..42] mint           Pubkey (32 bytes)
//! [43..74] beneficiary    Pubkey (32 bytes)
//! [75..80] split          Split { buyback_bps u16, holders_bps u16, beneficiary_bps u16 }
//! [81..82] bounty_bps     u16
//! [83..90] max_buyback    u64
//! [91..98] buyback_interval i64
//! [99..106] vest_secs     i64
//! [107]    launched       bool
//! [108..115] launched_at  i64
//! [116..123] dev_tokens   u64
//! [124..131] dev_released u64
//! [132..139] pending_buyback u64
//! [140..147] pending_holders u64
//! [148..155] pending_beneficiary u64
//! [156..163] last_buyback_at i64
//! [164..171] claimed_total u64
//! [172..179] spent_total  u64
//! [180..187] burned_total u64
//! [188..195] shared_total u64
//! [196..203] paid_beneficiary_total u64
//! [204..211] bounties_total u64
//! [212..227] reference_price u128
//! [228..235] reference_at i64
//! [236..267] game_hook    Pubkey (32 bytes)
//! [268..269] pot_bps      u16
//! [270..277] pending_pot  u64
//! [278..281] round_secs   u32
//! ...
//! ```
//!
//! # PayoutTerms layout (Borsh, with 8-byte discriminator, total 128 bytes)
//!
//! ```text
//! [0..7]   discriminator (8 bytes)
//! [8]      version        u8
//! [9]      bump           u8
//! [10..41] mint           Pubkey (32 bytes)
//! [42..43] step_bps       u16
//! [44..45] holding_bps    u16
//! [46..47] window_bps     u16
//! [48..51] window_secs    u32
//! [52..59] window_start   i64
//! [60..67] window_pot     u64
//! [68..75] window_paid    u64
//! [76..83] paid_total     u64
//! [84..91] payouts        u64
//! [92..99] last_paid_at   i64
//! [100..127] reserved     [u8; 28]
//!   [100]  reserved[0]    = 1 when wallet payouts are allowed (wallets_allowed)
//! ```
//!

use anchor_lang::prelude::Pubkey;

// ── Companion ────────────────────────────────────────────────────────────────

/// Byte offset of `Companion.mint` in on-chain account data (after 8-byte discriminator).
pub const COMPANION_MINT_OFFSET: usize = 11;
/// Byte offset of `Companion.beneficiary` (Pubkey): the wallet that launched the coin.
pub const COMPANION_BENEFICIARY_OFFSET: usize = 43;
/// Byte offset of `Companion.pending_pot` (u64 LE): the SOL the script may spend.
pub const COMPANION_POT_OFFSET: usize = 270;

/// Reads the mint from a raw `Companion` account data slice. `None` if too short.
pub fn read_companion_mint(data: &[u8]) -> Option<Pubkey> {
    let end = COMPANION_MINT_OFFSET + 32;
    Some(Pubkey::new_from_array(
        data.get(COMPANION_MINT_OFFSET..end)?.try_into().ok()?,
    ))
}

/// Reads `Companion.beneficiary` from raw account data. `None` if too short.
pub fn read_companion_beneficiary(data: &[u8]) -> Option<Pubkey> {
    let end = COMPANION_BENEFICIARY_OFFSET + 32;
    Some(Pubkey::new_from_array(
        data.get(COMPANION_BENEFICIARY_OFFSET..end)?.try_into().ok()?,
    ))
}

/// Reads `Companion.pending_pot` (u64 LE) from raw account data. `None` if too short.
pub fn read_pending_pot(data: &[u8]) -> Option<u64> {
    let end = COMPANION_POT_OFFSET + 8;
    Some(u64::from_le_bytes(
        data.get(COMPANION_POT_OFFSET..end)?.try_into().ok()?,
    ))
}

// ── PayoutTerms ───────────────────────────────────────────────────────────────

/// Byte offset of `PayoutTerms.mint` in on-chain account data.
pub const TERMS_MINT_OFFSET: usize = 10;
/// Byte offset of `PayoutTerms.step_bps` (u16 LE): the most one step pays, of the pot at its start.
pub const TERMS_STEP_BPS_OFFSET: usize = 42;
/// Byte offset of `PayoutTerms.holding_bps` (u16 LE): the most one entry gets of a step's budget.
pub const TERMS_HOLDING_BPS_OFFSET: usize = 44;
/// Byte offset of `PayoutTerms.window_bps` (u16 LE): the most a window pays, of the pot.
pub const TERMS_WINDOW_BPS_OFFSET: usize = 46;
/// Byte offset of `PayoutTerms.window_secs` (u32 LE): the window's length.
pub const TERMS_WINDOW_SECS_OFFSET: usize = 48;
/// Byte offset of `PayoutTerms.window_start` (i64 LE): first second of the window the counters are for.
pub const TERMS_WINDOW_START_OFFSET: usize = 52;
/// Byte offset of `PayoutTerms.window_pot` (u64 LE): the pot at the window's first step.
pub const TERMS_WINDOW_POT_OFFSET: usize = 60;
/// Byte offset of `PayoutTerms.window_paid` (u64 LE): what the window has paid so far.
pub const TERMS_WINDOW_PAID_OFFSET: usize = 68;
/// Byte offset of `PayoutTerms.reserved[0]`, the byte that says wallet payouts are allowed.
pub const TERMS_WALLETS_ALLOWED_OFFSET: usize = 100;
/// Value that `reserved[0]` holds when wallet payouts are allowed.
pub const WALLET_PAYOUTS_ALLOWED: u8 = 1;
/// Expected exact length of a `PayoutTerms` account (128 bytes total including discriminator).
pub const PAYOUT_TERMS_LEN: usize = 128;

/// Parsed fields of a `PayoutTerms` account needed by the script.
#[derive(Clone, Copy, Debug)]
pub struct PayoutTermsView {
    pub step_bps: u16,
    pub holding_bps: u16,
    pub window_bps: u16,
    pub window_secs: u32,
    pub window_start: i64,
    pub window_pot: u64,
    pub window_paid: u64,
    /// Whether `reserved[0] == 1` (wallet payouts allowed by `allow_wallet_payouts`).
    pub wallets_allowed: bool,
}

/// Reads the mint from a `PayoutTerms` account data slice. `None` if too short.
pub fn read_payout_terms_mint(data: &[u8]) -> Option<Pubkey> {
    let end = TERMS_MINT_OFFSET + 32;
    Some(Pubkey::new_from_array(
        data.get(TERMS_MINT_OFFSET..end)?.try_into().ok()?,
    ))
}

/// Reads the fields the script needs from raw `PayoutTerms` account data.
/// Returns `None` if the data is shorter than `PAYOUT_TERMS_LEN`.
pub fn read_payout_terms(data: &[u8]) -> Option<PayoutTermsView> {
    if data.len() < PAYOUT_TERMS_LEN {
        return None;
    }
    let u16_at = |off: usize| -> Option<u16> {
        Some(u16::from_le_bytes(data.get(off..off + 2)?.try_into().ok()?))
    };
    let u32_at = |off: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?))
    };
    let i64_at = |off: usize| -> Option<i64> {
        Some(i64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?))
    };
    let u64_at = |off: usize| -> Option<u64> {
        Some(u64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?))
    };
    Some(PayoutTermsView {
        step_bps: u16_at(TERMS_STEP_BPS_OFFSET)?,
        holding_bps: u16_at(TERMS_HOLDING_BPS_OFFSET)?,
        window_bps: u16_at(TERMS_WINDOW_BPS_OFFSET)?,
        window_secs: u32_at(TERMS_WINDOW_SECS_OFFSET)?,
        window_start: i64_at(TERMS_WINDOW_START_OFFSET)?,
        window_pot: u64_at(TERMS_WINDOW_POT_OFFSET)?,
        window_paid: u64_at(TERMS_WINDOW_PAID_OFFSET)?,
        wallets_allowed: data.get(TERMS_WALLETS_ALLOWED_OFFSET).copied() == Some(WALLET_PAYOUTS_ALLOWED),
    })
}
