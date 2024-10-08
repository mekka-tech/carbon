use carbon_core::borsh;
use carbon_core::deserialize::{ArrangeAccounts, CarbonDeserialize};
use carbon_proc_macros::CarbonDeserialize;

#[derive(
    CarbonDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
#[carbon(discriminator = "0x04")]
pub struct AdvanceNonceAccount;

pub struct AdvanceNonceAccountAccounts {
    pub nonce_account: solana_sdk::pubkey::Pubkey,
    pub recent_blockhashes_sysvar: solana_sdk::pubkey::Pubkey,
    pub nonce_authority: solana_sdk::pubkey::Pubkey,
}

impl ArrangeAccounts for AdvanceNonceAccount {
    type ArrangedAccounts = AdvanceNonceAccountAccounts;

    fn arrange_accounts(
        &self,
        accounts: Vec<solana_sdk::pubkey::Pubkey>,
    ) -> Option<Self::ArrangedAccounts> {
        let nonce_account = accounts.get(0)?;
        let recent_blockhashes_sysvar = accounts.get(1)?;
        let nonce_authority = accounts.get(2)?;

        Some(AdvanceNonceAccountAccounts {
            nonce_account: *nonce_account,
            recent_blockhashes_sysvar: *recent_blockhashes_sysvar,
            nonce_authority: *nonce_authority,
        })
    }
}
