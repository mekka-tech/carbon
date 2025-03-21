use carbon_core::{borsh, CarbonDeserialize};

#[derive(
    CarbonDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
#[carbon(discriminator = "0x09")]
pub struct SwapBaseIn {
    pub amount_in: u64,
    pub minimum_amount_out: u64,
}

impl carbon_core::deserialize::ArrangeAccounts for SwapBaseIn {
    type ArrangedAccounts = crate::instructions::swap_base::SwapBaseInstructionAccounts;

    fn arrange_accounts(
        accounts: &[solana_sdk::instruction::AccountMeta],
    ) -> Option<Self::ArrangedAccounts> {
        match accounts.len() {
            17 => {
                let [token_program, amm, amm_authority, amm_open_orders, pool_coin_token_account, pool_pc_token_account, serum_program, serum_market, serum_bids, serum_asks, serum_event_queue, serum_coin_vault_account, serum_pc_vault_account, serum_vault_signer, uer_source_token_account, uer_destination_token_account, user_source_owner, _remaining @ ..] =
                    accounts
                else {
                    return None;
                };

                Some(crate::instructions::swap_base::SwapBaseInstructionAccounts {
                    token_program: token_program.pubkey,
                    amm: amm.pubkey,
                    amm_authority: amm_authority.pubkey,
                    amm_open_orders: amm_open_orders.pubkey,
                    amm_target_orders: None,
                    pool_coin_token_account: pool_coin_token_account.pubkey,
                    pool_pc_token_account: pool_pc_token_account.pubkey,
                    serum_program: serum_program.pubkey,
                    serum_market: serum_market.pubkey,
                    serum_bids: serum_bids.pubkey,
                    serum_asks: serum_asks.pubkey,
                    serum_event_queue: serum_event_queue.pubkey,
                    serum_coin_vault_account: serum_coin_vault_account.pubkey,
                    serum_pc_vault_account: serum_pc_vault_account.pubkey,
                    serum_vault_signer: serum_vault_signer.pubkey,
                    uer_source_token_account: uer_source_token_account.pubkey,
                    uer_destination_token_account: uer_destination_token_account.pubkey,
                    user_source_owner: user_source_owner.pubkey,
                })
            }
            18 => {
                let [token_program, amm, amm_authority, amm_open_orders, amm_target_orders, pool_coin_token_account, pool_pc_token_account, serum_program, serum_market, serum_bids, serum_asks, serum_event_queue, serum_coin_vault_account, serum_pc_vault_account, serum_vault_signer, uer_source_token_account, uer_destination_token_account, user_source_owner, _remaining @ ..] =
                    accounts
                else {
                    return None;
                };

                Some( crate::instructions::swap_base::SwapBaseInstructionAccounts {
                    token_program: token_program.pubkey,
                    amm: amm.pubkey,
                    amm_authority: amm_authority.pubkey,
                    amm_open_orders: amm_open_orders.pubkey,
                    amm_target_orders: Some(amm_target_orders.pubkey),
                    pool_coin_token_account: pool_coin_token_account.pubkey,
                    pool_pc_token_account: pool_pc_token_account.pubkey,
                    serum_program: serum_program.pubkey,
                    serum_market: serum_market.pubkey,
                    serum_bids: serum_bids.pubkey,
                    serum_asks: serum_asks.pubkey,
                    serum_event_queue: serum_event_queue.pubkey,
                    serum_coin_vault_account: serum_coin_vault_account.pubkey,
                    serum_pc_vault_account: serum_pc_vault_account.pubkey,
                    serum_vault_signer: serum_vault_signer.pubkey,
                    uer_source_token_account: uer_source_token_account.pubkey,
                    uer_destination_token_account: uer_destination_token_account.pubkey,
                    user_source_owner: user_source_owner.pubkey,
                })
            }
            _ => None,
        }
    }
}
