use solana_sdk::pubkey::Pubkey;

#[derive(Debug)]
pub struct SwapBaseInstructionAccounts {
    pub token_program: Pubkey,
    pub amm: Pubkey,
    pub amm_authority: Pubkey,
    pub amm_open_orders: Pubkey,
    pub amm_target_orders: Option<Pubkey>,
    pub pool_coin_token_account: Pubkey,
    pub pool_pc_token_account: Pubkey,
    pub serum_program: Pubkey,
    pub serum_market: Pubkey,
    pub serum_bids: Pubkey,
    pub serum_asks: Pubkey,
    pub serum_event_queue: Pubkey,
    pub serum_coin_vault_account: Pubkey,
    pub serum_pc_vault_account: Pubkey,
    pub serum_vault_signer: Pubkey,
    pub uer_source_token_account: Pubkey,
    pub uer_destination_token_account: Pubkey,
    pub user_source_owner: Pubkey,
}