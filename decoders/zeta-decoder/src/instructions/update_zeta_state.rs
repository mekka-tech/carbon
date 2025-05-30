use super::super::types::*;

use carbon_core::{borsh, CarbonDeserialize};

#[derive(
    CarbonDeserialize, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Clone, Hash,
)]
#[carbon(discriminator = "0x68b614bb03a43c03")]
pub struct UpdateZetaState {
    pub args: UpdateStateArgs,
}

pub struct UpdateZetaStateInstructionAccounts {
    pub state: solana_sdk::pubkey::Pubkey,
    pub admin: solana_sdk::pubkey::Pubkey,
}

impl carbon_core::deserialize::ArrangeAccounts for UpdateZetaState {
    type ArrangedAccounts = UpdateZetaStateInstructionAccounts;

    fn arrange_accounts(
        accounts: &[solana_sdk::instruction::AccountMeta],
    ) -> Option<Self::ArrangedAccounts> {
        let [state, admin, _remaining @ ..] = accounts else {
            return None;
        };

        Some(UpdateZetaStateInstructionAccounts {
            state: state.pubkey,
            admin: admin.pubkey,
        })
    }
}
