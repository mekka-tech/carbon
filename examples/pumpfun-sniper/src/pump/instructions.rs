use {
    super::pdas,
    borsh::BorshSerialize,
    carbon_pumpfun_decoder::{
        accounts::global::Global, instructions::buy_exact_sol_in::BuyExactSolIn, types::OptionBool,
    },
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

/// Anchor discriminator for `buy_exact_sol_in` (see the decoder's `decode`).
const BUY_EXACT_SOL_IN_DISCRIMINATOR: [u8; 8] = [56, 252, 116, 8, 158, 223, 205, 95];

/// `CreateIdempotent` on the associated-token program. `0` is `Create`, which
/// fails with `IllegalOwner` when the ATA already exists.
const ATA_CREATE_IDEMPOTENT: u8 = 1;

/// Number of accounts `buy_exact_sol_in` takes: the 16 in the IDL plus the two
/// the program pulls out of `remaining_accounts` (`bonding_curve_v2`, then the
/// buyback fee recipient).
pub const BUY_EXACT_SOL_IN_ACCOUNT_COUNT: usize = 18;

/// Accounts resolved once per snipe signal (identical for every buyer wallet).
pub struct CoinAccounts {
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub creator_vault: Pubkey,
    /// Trailing `remaining_accounts[0]` of every buy. Not in the IDL.
    pub bonding_curve_v2: Pubkey,
    /// The token program that owns `mint`. `create` coins (the only ones this
    /// sniper snipes) are always classic SPL Token; `create_v2` coins are
    /// Token-2022. Passing the wrong one fails with
    /// `ConstraintAssociatedTokenTokenProgram` (2023).
    pub token_program: Pubkey,
}

impl CoinAccounts {
    /// For coins launched via `create` (classic SPL Token).
    pub fn new(
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        creator: &Pubkey,
    ) -> Self {
        Self::new_with_token_program(
            mint,
            bonding_curve,
            associated_bonding_curve,
            creator,
            pdas::TOKEN_PROGRAM_ID,
        )
    }

    pub fn new_with_token_program(
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        creator: &Pubkey,
        token_program: Pubkey,
    ) -> Self {
        Self {
            mint,
            bonding_curve,
            associated_bonding_curve,
            creator_vault: pdas::creator_vault(creator),
            bonding_curve_v2: pdas::bonding_curve_v2(&mint),
            token_program,
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
    /// `Global.buyback_fee_recipients`. One of these must be passed writable as
    /// the last account of every buy.
    pub buyback_fee_recipients: [Pubkey; 8],
}

impl StaticAccounts {
    /// Falls back to the buyback recipients snapshotted in
    /// [`pdas::BUYBACK_FEE_RECIPIENTS`]. Prefer [`Self::from_global`], which
    /// takes them from the live `Global` account.
    pub fn new(fee_recipient: Pubkey) -> Self {
        Self {
            global: pdas::global(),
            fee_recipient,
            event_authority: pdas::event_authority(),
            global_volume_accumulator: pdas::global_volume_accumulator(),
            fee_config: pdas::fee_config(),
            buyback_fee_recipients: pdas::BUYBACK_FEE_RECIPIENTS,
        }
    }

    /// Build from the decoded on-chain `Global` account so a rotation of the
    /// fee or buyback recipients is picked up at startup instead of failing
    /// every buy with `BuybackFeeRecipientNotAuthorized`.
    #[allow(dead_code)] // wire main.rs to this to track live buyback recipients
    pub fn from_global(global: &Global) -> Self {
        Self {
            buyback_fee_recipients: global.buyback_fee_recipients,
            ..Self::new(global.fee_recipient)
        }
    }

    /// Spread the write lock on the buyback recipient across wallets: all eight
    /// are accepted by the program, and picking per-buyer keeps 30 concurrent
    /// buys from serialising on a single account.
    fn buyback_fee_recipient(&self, buyer: &Pubkey) -> Pubkey {
        let idx = usize::from(buyer.as_ref()[0]) % self.buyback_fee_recipients.len();
        self.buyback_fee_recipients[idx]
    }
}

/// Build the `buy_exact_sol_in` instruction.
///
/// Account order follows `BuyExactSolInInstructionAccounts` in the pumpfun
/// decoder (and the on-chain IDL) for the first 16 entries, then the two
/// trailing `remaining_accounts` the deployed program requires. Signer/writable
/// flags follow the IDL and were confirmed by simulating this exact layout
/// against mainnet — see VERIFICATION.md.
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
        // 0 global — config only, never written by buy.
        AccountMeta::new_readonly(statics.global, false),
        // 1 fee_recipient — receives the protocol fee.
        AccountMeta::new(statics.fee_recipient, false),
        // 2 mint — read for decimals / token-program constraint.
        AccountMeta::new_readonly(coin.mint, false),
        // 3 bonding_curve — reserves are updated.
        AccountMeta::new(coin.bonding_curve, false),
        // 4 associated_bonding_curve — tokens leave this vault.
        AccountMeta::new(coin.associated_bonding_curve, false),
        // 5 associated_user — tokens land here.
        AccountMeta::new(
            pdas::associated_token_address_with_program(buyer, &coin.mint, &coin.token_program),
            false,
        ),
        // 6 user — pays lamports, must sign.
        AccountMeta::new(*buyer, true),
        AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        // 8 token_program — must own `mint`.
        AccountMeta::new_readonly(coin.token_program, false),
        // 9 creator_vault — receives the creator fee.
        AccountMeta::new(coin.creator_vault, false),
        AccountMeta::new_readonly(statics.event_authority, false),
        AccountMeta::new_readonly(pdas::PUMPFUN_PROGRAM_ID, false),
        // 12 global_volume_accumulator — read-only in the IDL; buy never writes
        // it. Keeping it read-only also avoids taking a write lock on an account
        // shared by every pump buyer in the slot.
        AccountMeta::new_readonly(statics.global_volume_accumulator, false),
        // 13 user_volume_accumulator — created/updated for this buyer.
        AccountMeta::new(pdas::user_volume_accumulator(buyer), false),
        AccountMeta::new_readonly(statics.fee_config, false),
        AccountMeta::new_readonly(pdas::FEE_PROGRAM_ID, false),
        // 16 remaining[0] bonding_curve_v2 — read-only; wrong address =>
        // InvalidBondingCurveV2 (6074).
        AccountMeta::new_readonly(coin.bonding_curve_v2, false),
        // 17 remaining[1] buyback fee recipient — must be writable (read-only
        // => PrivilegeEscalation) and must be one of
        // Global.buyback_fee_recipients (=> BuybackFeeRecipientNotAuthorized).
        AccountMeta::new(statics.buyback_fee_recipient(buyer), false),
    ];
    debug_assert_eq!(accounts.len(), BUY_EXACT_SOL_IN_ACCOUNT_COUNT);

    Instruction {
        program_id: pdas::PUMPFUN_PROGRAM_ID,
        accounts,
        data,
    }
}

/// `CreateIdempotent` on the associated-token program, hand-rolled to avoid
/// pulling the full spl-associated-token-account crate into the workspace.
pub fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    create_ata_idempotent_with_program(payer, owner, mint, &pdas::TOKEN_PROGRAM_ID)
}

pub fn create_ata_idempotent_with_program(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: pdas::ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(
                pdas::associated_token_address_with_program(owner, mint, token_program),
                false,
            ),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![ATA_CREATE_IDEMPOTENT],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth: the `buy_exact_sol_in` instruction of mainnet transaction
    /// 4iTDfoKjZF22dF9VdbskGY8irmZwNGj2ECzHSKjeaXPHqsGGabiYMRwLP1WpUwxf6oCCCAK6C9jTnPnX5nPHenuZ
    /// (slot 435898365, succeeded on-chain). See VERIFICATION.md for how it was
    /// captured and re-simulated.
    const FIXTURE_ACCOUNTS: [&str; BUY_EXACT_SOL_IN_ACCOUNT_COUNT] = [
        "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf", // global
        "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", // fee_recipient
        "HerAuoQGDYKDHhDddQSbbiZ2EztTvogFwUgSRRAWZtHC", // mint
        "4vscQFvtKuQ4gVSyMFQgR8NP1erfUxTgwtfRtbTW5cfr", // bonding_curve
        "Amj4ZUv1zwxMnVBSe5wrGC4zwzneNexKS77F7KC64JQB", // associated_bonding_curve
        "DP8pRnKUz6vgMFMAofm5tFPNuhVoQNraLBqeKu6cniuF", // associated_user
        "JDfuh8jY3LbP1j22vJGBfwtEtyWwf9Sy7AVXSPK29gfn", // user
        "11111111111111111111111111111111",             // system_program
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",  // token_program
        "7gNNmYipDqGPE5uTmjdy8i6ommvVirvGnrhCsKK4fHdN", // creator_vault
        "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1", // event_authority
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",  // program
        "Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y", // global_volume_accumulator
        "574dmPVqZGSsXrebbxRvRxqN3HM2oBk5CAgWdb5Lxixg", // user_volume_accumulator
        "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt", // fee_config
        "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ",  // fee_program
        "HnWSmuLBVrahUf5rhV26ra7ki6iTZxVhJNSP5cQJ2q5G", // remaining[0] bonding_curve_v2
        "A7hAgCzFw14fejgCp387JUJRMNyz4j89JKnhtKU8piqW", // remaining[1] buyback_fee_recipient
    ];

    /// `(is_signer, is_writable)` per account, from the on-chain IDL. Index 12
    /// (`global_volume_accumulator`) is read-only here: the IDL declares it
    /// without `mut`, and simulating this layout on mainnet succeeds. The
    /// captured transaction happens to carry it as writable at the *message*
    /// level, which is a union over every instruction in that transaction and
    /// not a requirement of the buy itself.
    const FIXTURE_FLAGS: [(bool, bool); BUY_EXACT_SOL_IN_ACCOUNT_COUNT] = [
        (false, false), // global
        (false, true),  // fee_recipient
        (false, false), // mint
        (false, true),  // bonding_curve
        (false, true),  // associated_bonding_curve
        (false, true),  // associated_user
        (true, true),   // user
        (false, false), // system_program
        (false, false), // token_program
        (false, true),  // creator_vault
        (false, false), // event_authority
        (false, false), // program
        (false, false), // global_volume_accumulator
        (false, true),  // user_volume_accumulator
        (false, false), // fee_config
        (false, false), // fee_program
        (false, false), // bonding_curve_v2
        (false, true),  // buyback_fee_recipient
    ];

    const FIXTURE_CREATOR: Pubkey =
        Pubkey::from_str_const("9acz9ookuvGHEPCCwZHSXkPMW2HYDbitFFw2euKHwX4G");
    const FIXTURE_SPENDABLE_SOL_IN: u64 = 400_000_000;
    const FIXTURE_MIN_TOKENS_OUT: u64 = 365_480_484_310;

    fn fixture(idx: usize) -> Pubkey {
        Pubkey::from_str_const(FIXTURE_ACCOUNTS[idx])
    }

    fn fixture_statics() -> StaticAccounts {
        StaticAccounts {
            // Pin every slot to the recipient the captured transaction used so
            // the per-buyer spreading picks it deterministically.
            buyback_fee_recipients: [fixture(17); 8],
            ..StaticAccounts::new(fixture(1))
        }
    }

    fn fixture_coin() -> CoinAccounts {
        CoinAccounts::new(fixture(2), fixture(3), fixture(4), &FIXTURE_CREATOR)
    }

    fn fixture_instruction() -> Instruction {
        buy_exact_sol_in(
            &fixture_statics(),
            &fixture_coin(),
            &fixture(6),
            FIXTURE_SPENDABLE_SOL_IN,
            FIXTURE_MIN_TOKENS_OUT,
            false,
        )
    }

    #[test]
    fn buy_reproduces_a_real_mainnet_instruction() {
        let ix = fixture_instruction();
        assert_eq!(ix.program_id, pdas::PUMPFUN_PROGRAM_ID);
        assert_eq!(ix.accounts.len(), BUY_EXACT_SOL_IN_ACCOUNT_COUNT);
        for (i, expected) in FIXTURE_ACCOUNTS.iter().enumerate() {
            assert_eq!(
                ix.accounts[i].pubkey,
                Pubkey::from_str_const(expected),
                "account {i} mismatch"
            );
        }
    }

    #[test]
    fn buy_account_flags_match_the_idl() {
        let ix = fixture_instruction();
        for (i, (signer, writable)) in FIXTURE_FLAGS.iter().enumerate() {
            assert_eq!(ix.accounts[i].is_signer, *signer, "signer flag {i}");
            assert_eq!(ix.accounts[i].is_writable, *writable, "writable flag {i}");
        }
        // Exactly one signer, and it is the buyer.
        let signers: Vec<_> = ix.accounts.iter().filter(|a| a.is_signer).collect();
        assert_eq!(signers.len(), 1);
        assert_eq!(signers[0].pubkey, fixture(6));
    }

    #[test]
    fn buy_data_layout_matches_the_decoder() {
        let ix = fixture_instruction();
        // discriminator + u64 + u64 + 1-byte OptionBool
        assert_eq!(ix.data.len(), 8 + 8 + 8 + 1);
        assert_eq!(&ix.data[0..8], &BUY_EXACT_SOL_IN_DISCRIMINATOR);
        assert_eq!(
            u64::from_le_bytes(ix.data[8..16].try_into().unwrap()),
            FIXTURE_SPENDABLE_SOL_IN
        );
        assert_eq!(
            u64::from_le_bytes(ix.data[16..24].try_into().unwrap()),
            FIXTURE_MIN_TOKENS_OUT
        );
        assert_eq!(ix.data[24], 0);

        // The bytes after the discriminator must round-trip through the
        // decoder's own deserializer.
        let decoded = BuyExactSolIn::decode(&ix.data).expect("decoder accepts our data");
        assert_eq!(decoded.spendable_sol_in, FIXTURE_SPENDABLE_SOL_IN);
        assert_eq!(decoded.min_tokens_out, FIXTURE_MIN_TOKENS_OUT);
        assert_eq!(decoded.track_volume, OptionBool(false));
    }

    #[test]
    fn track_volume_serialises_as_a_single_byte() {
        let ix = buy_exact_sol_in(&fixture_statics(), &fixture_coin(), &fixture(6), 1, 1, true);
        assert_eq!(ix.data.len(), 25);
        assert_eq!(ix.data[24], 1);
        assert_eq!(
            BuyExactSolIn::decode(&ix.data).unwrap().track_volume,
            OptionBool(true)
        );
    }

    #[test]
    fn buy_account_order_matches_the_decoders_arrange_accounts() {
        use carbon_core::deserialize::ArrangeAccounts;
        let ix = fixture_instruction();
        let arranged = BuyExactSolIn::arrange_accounts(&ix.accounts).expect("arrange");
        assert_eq!(arranged.global, fixture(0));
        assert_eq!(arranged.fee_recipient, fixture(1));
        assert_eq!(arranged.mint, fixture(2));
        assert_eq!(arranged.bonding_curve, fixture(3));
        assert_eq!(arranged.associated_bonding_curve, fixture(4));
        assert_eq!(arranged.associated_user, fixture(5));
        assert_eq!(arranged.user, fixture(6));
        assert_eq!(arranged.system_program, fixture(7));
        assert_eq!(arranged.token_program, fixture(8));
        assert_eq!(arranged.creator_vault, fixture(9));
        assert_eq!(arranged.event_authority, fixture(10));
        assert_eq!(arranged.program, fixture(11));
        assert_eq!(arranged.global_volume_accumulator, fixture(12));
        assert_eq!(arranged.user_volume_accumulator, fixture(13));
        assert_eq!(arranged.fee_config, fixture(14));
        assert_eq!(arranged.fee_program, fixture(15));
        // The two accounts the IDL does not name, in order.
        assert_eq!(arranged.remaining.len(), 2);
        assert_eq!(arranged.remaining[0].pubkey, fixture(16));
        assert_eq!(arranged.remaining[1].pubkey, fixture(17));
    }

    #[test]
    fn buyback_recipient_is_always_from_the_authorised_set() {
        let statics = StaticAccounts::new(fixture(1));
        let coin = fixture_coin();
        // Different buyers must all land on an authorised recipient, and the
        // set must actually get spread across.
        let mut seen = std::collections::HashSet::new();
        for i in 0u8..64 {
            let buyer = Pubkey::new_from_array([i; 32]);
            let ix = buy_exact_sol_in(&statics, &coin, &buyer, 1, 1, false);
            let last = &ix.accounts[17];
            assert!(
                pdas::BUYBACK_FEE_RECIPIENTS.contains(&last.pubkey),
                "buyback recipient {} not authorised",
                last.pubkey
            );
            assert!(last.is_writable && !last.is_signer);
            seen.insert(last.pubkey);
        }
        assert_eq!(seen.len(), 8, "buyback recipient should spread over all 8");
    }

    #[test]
    fn token_program_drives_the_buyer_ata() {
        let buyer = fixture(6);
        let mint = fixture(2);
        let t22 = CoinAccounts::new_with_token_program(
            mint,
            fixture(3),
            fixture(4),
            &FIXTURE_CREATOR,
            pdas::TOKEN_2022_PROGRAM_ID,
        );
        let ix = buy_exact_sol_in(&fixture_statics(), &t22, &buyer, 1, 1, false);
        assert_eq!(ix.accounts[8].pubkey, pdas::TOKEN_2022_PROGRAM_ID);
        assert_eq!(
            ix.accounts[5].pubkey,
            pdas::associated_token_address_with_program(
                &buyer,
                &mint,
                &pdas::TOKEN_2022_PROGRAM_ID
            )
        );
        assert_ne!(ix.accounts[5].pubkey, fixture(5));
    }

    #[test]
    fn create_ata_idempotent_layout() {
        let payer = fixture(6);
        let mint = fixture(2);
        let ix = create_ata_idempotent(&payer, &payer, &mint);
        assert_eq!(ix.program_id, pdas::ASSOCIATED_TOKEN_PROGRAM_ID);
        // 1 == CreateIdempotent; 0 (Create) fails on an existing ATA.
        assert_eq!(ix.data, vec![1u8]);
        assert_eq!(ix.accounts.len(), 6);

        let expect = [
            (payer, true, true),
            (fixture(5), false, true), // the buyer's ATA for this mint
            (payer, false, false),     // owner
            (mint, false, false),
            (solana_system_interface::program::ID, false, false),
            (pdas::TOKEN_PROGRAM_ID, false, false),
        ];
        for (i, (pk, signer, writable)) in expect.iter().enumerate() {
            assert_eq!(ix.accounts[i].pubkey, *pk, "ata account {i}");
            assert_eq!(ix.accounts[i].is_signer, *signer, "ata signer {i}");
            assert_eq!(ix.accounts[i].is_writable, *writable, "ata writable {i}");
        }
    }

    #[test]
    fn create_ata_idempotent_honours_the_token_program() {
        let payer = fixture(6);
        let mint = fixture(2);
        let ix =
            create_ata_idempotent_with_program(&payer, &payer, &mint, &pdas::TOKEN_2022_PROGRAM_ID);
        assert_eq!(ix.accounts[5].pubkey, pdas::TOKEN_2022_PROGRAM_ID);
        assert_eq!(
            ix.accounts[1].pubkey,
            pdas::associated_token_address_with_program(
                &payer,
                &mint,
                &pdas::TOKEN_2022_PROGRAM_ID
            )
        );
    }

    #[test]
    fn ata_created_matches_the_ata_the_buy_uses() {
        let coin = fixture_coin();
        let buyer = fixture(6);
        let buy = buy_exact_sol_in(&fixture_statics(), &coin, &buyer, 1, 1, false);
        let ata =
            create_ata_idempotent_with_program(&buyer, &buyer, &coin.mint, &coin.token_program);
        assert_eq!(buy.accounts[5].pubkey, ata.accounts[1].pubkey);
    }
}
