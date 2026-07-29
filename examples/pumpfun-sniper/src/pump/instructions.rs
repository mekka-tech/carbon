use {
    super::pdas,
    borsh::BorshSerialize,
    carbon_pumpfun_decoder::{instructions::buy_exact_sol_in::BuyExactSolIn, types::OptionBool},
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

/// Anchor discriminator for `buy_exact_sol_in` (see the decoder's `decode`).
const BUY_EXACT_SOL_IN_DISCRIMINATOR: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];

/// Accounts resolved once per snipe signal (identical for every buyer wallet).
pub struct CoinAccounts {
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub creator_vault: Pubkey,
}

impl CoinAccounts {
    pub fn new(
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        creator: &Pubkey,
    ) -> Self {
        Self {
            mint,
            bonding_curve,
            associated_bonding_curve,
            creator_vault: pdas::creator_vault(creator),
        }
    }
}

/// Program-level accounts resolved once at startup.
pub struct StaticAccounts {
    pub global: Pubkey,
    pub fee_recipient: Pubkey,
    pub event_authority: Pubkey,
    pub global_volume_accumulator: Pubkey,
    pub fee_config: Pubkey,
}

impl StaticAccounts {
    pub fn new(fee_recipient: Pubkey) -> Self {
        Self {
            global: pdas::global(),
            fee_recipient,
            event_authority: pdas::event_authority(),
            global_volume_accumulator: pdas::global_volume_accumulator(),
            fee_config: pdas::fee_config(),
        }
    }
}

/// Build the `buy_exact_sol_in` instruction. Account order and flags follow
/// `BuyExactSolInInstructionAccounts` in the pumpfun decoder.
pub fn buy_exact_sol_in(
    statics: &StaticAccounts,
    coin: &CoinAccounts,
    buyer: &Pubkey,
    spendable_sol_in: u64,
    min_tokens_out: u64,
    track_volume: bool,
) -> Instruction {
    let args = BuyExactSolIn {
        spendable_sol_in,
        min_tokens_out,
        track_volume: OptionBool(track_volume),
    };
    let mut data = BUY_EXACT_SOL_IN_DISCRIMINATOR.to_vec();
    args.serialize(&mut data).expect("borsh serialize");

    let accounts = vec![
        AccountMeta::new_readonly(statics.global, false),
        AccountMeta::new(statics.fee_recipient, false),
        AccountMeta::new_readonly(coin.mint, false),
        AccountMeta::new(coin.bonding_curve, false),
        AccountMeta::new(coin.associated_bonding_curve, false),
        AccountMeta::new(pdas::associated_token_address(buyer, &coin.mint), false),
        AccountMeta::new(*buyer, true),
        AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        AccountMeta::new_readonly(pdas::TOKEN_PROGRAM_ID, false),
        AccountMeta::new(coin.creator_vault, false),
        AccountMeta::new_readonly(statics.event_authority, false),
        AccountMeta::new_readonly(pdas::PUMPFUN_PROGRAM_ID, false),
        AccountMeta::new(statics.global_volume_accumulator, false),
        AccountMeta::new(pdas::user_volume_accumulator(buyer), false),
        AccountMeta::new_readonly(statics.fee_config, false),
        AccountMeta::new_readonly(pdas::FEE_PROGRAM_ID, false),
    ];

    Instruction {
        program_id: pdas::PUMPFUN_PROGRAM_ID,
        accounts,
        data,
    }
}

/// `CreateIdempotent` on the associated-token program, hand-rolled to avoid
/// pulling the full spl-associated-token-account crate into the workspace.
pub fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: pdas::ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(pdas::associated_token_address(owner, mint), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(pdas::TOKEN_PROGRAM_ID, false),
        ],
        data: vec![1],
    }
}
