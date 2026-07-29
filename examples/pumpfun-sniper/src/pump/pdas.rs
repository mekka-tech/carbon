use solana_pubkey::Pubkey;

pub use carbon_pumpfun_decoder::PROGRAM_ID as PUMPFUN_PROGRAM_ID;

pub const FEE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022. Coins launched through `create_v2` are minted here; coins
/// launched through `create` use [`TOKEN_PROGRAM_ID`]. Both addresses are
/// hardcoded in the respective pump instructions in the on-chain IDL, which is
/// why the buy builders take the token program as a field rather than assuming
/// one.
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// Wrapped SOL. `create_v2` coins trade against a *quote mint* rather than
/// native lamports, and `Global.whitelisted_quote_mints` currently contains
/// only this one. A v2 buy therefore spends SPL tokens out of the buyer's WSOL
/// account, not lamports off the buyer directly.
pub const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");

/// The "mayhem" program. `create_v2` routes several of its accounts here, and
/// mayhem-mode coins derive `bonding_curve_v2` on this program instead of pump.
pub const MAYHEM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e");

/// `Global.buyback_fee_recipients` as read from mainnet on 2026-07-29.
///
/// `buy` / `buy_exact_sol_in` require a *writable* buyback fee recipient as the
/// last account; the program rejects any pubkey outside this set with
/// `BuybackFeeRecipientNotAuthorized` (6057). The authority can rotate the list
/// via `update_buyback_config`, so prefer
/// [`super::instructions::StaticAccounts::from_global`], which reads the live
/// values out of the on-chain `Global` account. This constant is only the
/// fallback used by [`super::instructions::StaticAccounts::new`].
pub const BUYBACK_FEE_RECIPIENTS: [Pubkey; 8] = [
    Pubkey::from_str_const("5YxQFdt3Tr9zJLvkFccqXVUwhdTWJQc1fFg2YPbxvxeD"),
    Pubkey::from_str_const("9M4giFFMxmFGXtc3feFzRai56WbBqehoSeRE5GK7gf7"),
    Pubkey::from_str_const("GXPFM2caqTtQYC2cJ5yJRi9VDkpsYZXzYdwYpGnLmtDL"),
    Pubkey::from_str_const("3BpXnfJaUTiwXnJNe7Ej1rcbzqTTQUvLShZaWazebsVR"),
    Pubkey::from_str_const("5cjcW9wExnJJiqgLjq7DEG75Pm6JBgE1hNv4B2vHXUW6"),
    Pubkey::from_str_const("EHAAiTxcdDwQ3U4bU6YcMsQGaekdzLS3B5SmYo46kJtL"),
    Pubkey::from_str_const("5eHhjP8JaYkz83CWwvGU2uMUXefd3AazWGx4gpcuEEYD"),
    Pubkey::from_str_const("A7hAgCzFw14fejgCp387JUJRMNyz4j89JKnhtKU8piqW"),
];

pub fn global() -> Pubkey {
    Pubkey::find_program_address(&[b"global"], &PUMPFUN_PROGRAM_ID).0
}

pub fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &PUMPFUN_PROGRAM_ID).0
}

pub fn creator_vault(creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"creator-vault", creator.as_ref()], &PUMPFUN_PROGRAM_ID).0
}

pub fn global_volume_accumulator() -> Pubkey {
    Pubkey::find_program_address(&[b"global_volume_accumulator"], &PUMPFUN_PROGRAM_ID).0
}

pub fn user_volume_accumulator(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"user_volume_accumulator", user.as_ref()],
        &PUMPFUN_PROGRAM_ID,
    )
    .0
}

/// The pump fee-config account lives on the external fee program, seeded with
/// the pump program id. Verified against the on-chain account at startup.
pub fn fee_config() -> Pubkey {
    Pubkey::find_program_address(
        &[b"fee_config", PUMPFUN_PROGRAM_ID.as_ref()],
        &FEE_PROGRAM_ID,
    )
    .0
}

/// `bonding-curve` PDA for a mint. The sniper normally takes this straight off
/// the observed `create` instruction; this is here for cross-checking.
#[allow(dead_code)] // cross-check helper, exercised by tests
pub fn bonding_curve(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"bonding-curve", mint.as_ref()], &PUMPFUN_PROGRAM_ID).0
}

/// `bonding_curve_v2` — the first of the two trailing accounts every current
/// `buy` / `buy_exact_sol_in` carries. It is *not* in the IDL's account list
/// (the program reads it out of `remaining_accounts`), but omitting it or
/// passing the wrong address fails with `InvalidBondingCurveV2` (6074).
///
/// Note: coins in "mayhem mode" derive this on the mayhem program instead —
/// see [`bonding_curve_v2_mayhem`].
pub fn bonding_curve_v2(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &PUMPFUN_PROGRAM_ID).0
}

/// `bonding_curve_v2` as derived for a mayhem-mode coin, i.e. on the mayhem
/// program rather than pump. `create_v2` sets `is_mayhem_mode` per coin, so the
/// v2 builder picks between this and [`bonding_curve_v2`].
pub fn bonding_curve_v2_mayhem(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"bonding-curve-v2", mint.as_ref()], &MAYHEM_PROGRAM_ID).0
}

/// `sharing-config` for a v2 coin. Lives on the *fee* program, seeded with the
/// base mint — index 18 of `buy_exact_quote_in_v2`, read-only.
pub fn sharing_config(base_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"sharing-config", base_mint.as_ref()], &FEE_PROGRAM_ID).0
}

/// Standard associated-token-account derivation for the classic SPL token
/// program.
#[allow(dead_code)] // convenience wrapper, exercised by tests
pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    associated_token_address_with_program(owner, mint, &TOKEN_PROGRAM_ID)
}

/// Standard associated-token-account derivation:
/// `[owner, token_program, mint]` on the associated-token program.
pub fn associated_token_address_with_program(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expected value below was read off mainnet (see VERIFICATION.md):
    // the program-level PDAs from the on-chain Anchor IDL's `pda.seeds`, and the
    // per-coin PDAs from the account list of the real, successful buy in
    // transaction
    // 4iTDfoKjZF22dF9VdbskGY8irmZwNGj2ECzHSKjeaXPHqsGGabiYMRwLP1WpUwxf6oCCCAK6C9jTnPnX5nPHenuZ
    // (slot 435898365).
    const MINT: Pubkey = Pubkey::from_str_const("HerAuoQGDYKDHhDddQSbbiZ2EztTvogFwUgSRRAWZtHC");
    const CREATOR: Pubkey = Pubkey::from_str_const("9acz9ookuvGHEPCCwZHSXkPMW2HYDbitFFw2euKHwX4G");
    const USER: Pubkey = Pubkey::from_str_const("JDfuh8jY3LbP1j22vJGBfwtEtyWwf9Sy7AVXSPK29gfn");

    #[test]
    fn program_level_pdas_match_mainnet() {
        assert_eq!(
            global(),
            Pubkey::from_str_const("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf")
        );
        assert_eq!(
            event_authority(),
            Pubkey::from_str_const("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1")
        );
        assert_eq!(
            global_volume_accumulator(),
            Pubkey::from_str_const("Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y")
        );
        // Seeded on the *fee* program with the pump program id as second seed.
        assert_eq!(
            fee_config(),
            Pubkey::from_str_const("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt")
        );
    }

    #[test]
    fn per_coin_pdas_match_mainnet() {
        assert_eq!(
            bonding_curve(&MINT),
            Pubkey::from_str_const("4vscQFvtKuQ4gVSyMFQgR8NP1erfUxTgwtfRtbTW5cfr")
        );
        assert_eq!(
            bonding_curve_v2(&MINT),
            Pubkey::from_str_const("HnWSmuLBVrahUf5rhV26ra7ki6iTZxVhJNSP5cQJ2q5G")
        );
        assert_eq!(
            creator_vault(&CREATOR),
            Pubkey::from_str_const("7gNNmYipDqGPE5uTmjdy8i6ommvVirvGnrhCsKK4fHdN")
        );
        assert_eq!(
            user_volume_accumulator(&USER),
            Pubkey::from_str_const("574dmPVqZGSsXrebbxRvRxqN3HM2oBk5CAgWdb5Lxixg")
        );
    }

    #[test]
    fn associated_token_addresses_match_mainnet() {
        // Buyer ATA, and the bonding curve's ATA (which is derived the same way,
        // with the curve as owner).
        assert_eq!(
            associated_token_address(&USER, &MINT),
            Pubkey::from_str_const("DP8pRnKUz6vgMFMAofm5tFPNuhVoQNraLBqeKu6cniuF")
        );
        assert_eq!(
            associated_token_address(&bonding_curve(&MINT), &MINT),
            Pubkey::from_str_const("Amj4ZUv1zwxMnVBSe5wrGC4zwzneNexKS77F7KC64JQB")
        );
        // The token program is part of the seeds, so a Token-2022 mint lands on
        // a different address entirely.
        assert_ne!(
            associated_token_address_with_program(&USER, &MINT, &TOKEN_2022_PROGRAM_ID),
            associated_token_address(&USER, &MINT)
        );
    }

    #[test]
    fn derivations_are_deterministic() {
        assert_eq!(global(), global());
        assert_eq!(creator_vault(&CREATOR), creator_vault(&CREATOR));
        assert_ne!(creator_vault(&CREATOR), creator_vault(&USER));
        assert_ne!(bonding_curve(&MINT), bonding_curve_v2(&MINT));
    }

    #[test]
    fn program_ids_are_the_mainnet_ones() {
        assert_eq!(
            PUMPFUN_PROGRAM_ID,
            Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")
        );
        assert_eq!(BUYBACK_FEE_RECIPIENTS.len(), 8);
    }
}
