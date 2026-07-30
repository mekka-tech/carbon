//! Address Lookup Table resolution for shredstream transactions.
//!
//! Shreds carry no transaction metadata, so a shredstream `TransactionUpdate`
//! has an empty `loaded_addresses`. For a v0 transaction that uses an Address
//! Lookup Table this is not a cosmetic gap: `extract_instructions_with_metadata`
//! builds its account-key list as
//!
//! ```text
//! static_keys ++ loaded_addresses.writable ++ loaded_addresses.readonly
//! ```
//!
//! so every account index pointing into the ALT range falls off the end of the
//! list. The instruction is still extracted (program ids live in the static
//! range), but its account list is truncated, `ArrangeAccounts` cannot fill the
//! decoder's account struct, and the decoder returns `None`.
//!
//! **Nothing errors.** The pipeline reports 100% success while decoding nothing,
//! which makes this failure invisible in metrics. Since virtually all modern
//! pump.fun traffic is v0-with-ALT, that means no launches decode at all.
//!
//! This module resolves the lookups against chain state and fills in
//! `loaded_addresses` so the rest of the pipeline behaves exactly as it does on
//! a metadata-carrying feed.

use {
    scc::HashMap as ConcurrentHashMap,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_message::{v0::LoadedAddresses, VersionedMessage},
    solana_pubkey::Pubkey,
    std::sync::Arc,
};

/// Byte offset of the first address in a lookup-table account.
///
/// Layout preceding it: `u32` discriminator, `u64` deactivation slot, `u64`
/// last-extended slot, `u8` last-extended start index, `Option<Pubkey>`
/// authority, `u16` padding. Parsed by offset rather than pulling in the
/// lookup-table interface crate for one constant.
const LOOKUP_TABLE_META_SIZE: usize = 56;

/// Resolved addresses for one lookup table, cached by table address.
///
/// Lookup tables are append-only: an address at a given index never changes, so
/// a hit is valid indefinitely. A table that has been *extended* since we cached
/// it simply yields a short vector, which `resolve` detects and refetches.
#[derive(Clone)]
pub struct AltCache {
    tables: Arc<ConcurrentHashMap<Pubkey, Arc<Vec<Pubkey>>>>,
    rpc: Arc<RpcClient>,
}

impl AltCache {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            tables: Arc::new(ConcurrentHashMap::new()),
            rpc,
        }
    }

    /// Addresses for one lookup table, from cache or chain.
    ///
    /// `min_len` is the highest index the transaction referenced, plus one. A
    /// cached vector shorter than that means the table was extended after we
    /// cached it, so it is refetched once.
    async fn table(&self, address: &Pubkey, min_len: usize) -> Option<Arc<Vec<Pubkey>>> {
        if let Some(hit) = self.tables.read_async(address, |_, v| v.clone()).await {
            if hit.len() >= min_len {
                return Some(hit);
            }
        }

        let account = match self.rpc.get_account(address).await {
            Ok(account) => account,
            Err(err) => {
                log::warn!("alt: failed to fetch lookup table {address}: {err}");
                return None;
            }
        };
        let addresses = parse_lookup_table(&account.data)?;
        let addresses = Arc::new(addresses);
        let _ = self
            .tables
            .insert_async(*address, addresses.clone())
            .await
            .or_else(|(k, v)| {
                // Another task cached it first; overwrite so the longer of the
                // two wins after an extension.
                self.tables.upsert(k, v.clone());
                Ok::<(), ()>(())
            });
        Some(addresses)
    }

    /// Resolve every lookup in a v0 message into `LoadedAddresses`.
    ///
    /// Returns the default (empty) value for legacy messages and for v0
    /// messages with no lookups — in both cases the static keys are already the
    /// complete set.
    ///
    /// Ordering matches the runtime: all writable addresses across every table
    /// in table order, then all readonly across every table. Getting this wrong
    /// silently shifts every ALT-resolved account index.
    pub async fn resolve(&self, message: &VersionedMessage) -> LoadedAddresses {
        let lookups = match message {
            VersionedMessage::V0(v0) if !v0.address_table_lookups.is_empty() => {
                &v0.address_table_lookups
            }
            _ => return LoadedAddresses::default(),
        };

        let mut writable = Vec::new();
        let mut readonly = Vec::new();

        for lookup in lookups.iter() {
            let highest = lookup
                .writable_indexes
                .iter()
                .chain(lookup.readonly_indexes.iter())
                .copied()
                .max()
                .unwrap_or(0) as usize;

            let Some(table) = self.table(&lookup.account_key, highest.saturating_add(1)).await
            else {
                // Without this table the resulting key list would be silently
                // misaligned, which is worse than dropping the transaction:
                // every downstream account would be off by the missing count.
                log::debug!(
                    "alt: unresolved table {}, dropping lookups for this transaction",
                    lookup.account_key
                );
                return LoadedAddresses::default();
            };

            for index in lookup.writable_indexes.iter() {
                match table.get(*index as usize) {
                    Some(key) => writable.push(*key),
                    None => return LoadedAddresses::default(),
                }
            }
            for index in lookup.readonly_indexes.iter() {
                match table.get(*index as usize) {
                    Some(key) => readonly.push(*key),
                    None => return LoadedAddresses::default(),
                }
            }
        }

        LoadedAddresses { writable, readonly }
    }
}

/// Extract the address list from raw lookup-table account data.
///
/// Returns `None` when the account is too small or its address region is not a
/// whole number of 32-byte keys, either of which means this is not a lookup
/// table.
fn parse_lookup_table(data: &[u8]) -> Option<Vec<Pubkey>> {
    let raw = data.get(LOOKUP_TABLE_META_SIZE..)?;
    if raw.is_empty() || raw.len() % 32 != 0 {
        return None;
    }
    Some(
        raw.chunks_exact(32)
            .map(|chunk| {
                let mut key = [0u8; 32];
                key.copy_from_slice(chunk);
                Pubkey::new_from_array(key)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses_after_the_metadata_header() {
        let mut data = vec![0u8; LOOKUP_TABLE_META_SIZE];
        data.extend_from_slice(&[7u8; 32]);
        data.extend_from_slice(&[9u8; 32]);
        let parsed = parse_lookup_table(&data).expect("two whole keys should parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], Pubkey::new_from_array([7u8; 32]));
        assert_eq!(parsed[1], Pubkey::new_from_array([9u8; 32]));
    }

    #[test]
    fn rejects_a_partial_trailing_key() {
        // A ragged tail means we are not looking at a lookup table; silently
        // truncating would shift every later index.
        let mut data = vec![0u8; LOOKUP_TABLE_META_SIZE];
        data.extend_from_slice(&[1u8; 40]);
        assert!(parse_lookup_table(&data).is_none());
    }

    #[test]
    fn rejects_an_account_smaller_than_the_header() {
        assert!(parse_lookup_table(&[0u8; 10]).is_none());
    }

    #[test]
    fn rejects_a_header_with_no_addresses() {
        assert!(parse_lookup_table(&[0u8; LOOKUP_TABLE_META_SIZE]).is_none());
    }
}
