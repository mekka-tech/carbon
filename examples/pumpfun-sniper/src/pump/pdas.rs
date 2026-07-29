use solana_pubkey::Pubkey;

pub use carbon_pumpfun_decoder::PROGRAM_ID as PUMPFUN_PROGRAM_ID;

pub const FEE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

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

pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}
