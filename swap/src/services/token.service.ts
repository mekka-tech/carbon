import { PublicKey } from "@solana/web3.js";

import { getAccount } from "@solana/spl-token";

import { getAssociatedTokenAddress } from "@solana/spl-token";
import { private_connection } from "../config";

export class TokenService {
  static async getTokenBalance(walletAddress: PublicKey, mintAddress: PublicKey) {
      try {
            // Derive the associated token account address
      const tokenAccountAddress = await getAssociatedTokenAddress(
        mintAddress,     // Token Mint Address
        walletAddress    // Wallet Public Key
      );
  
      // Fetch token account information
      const tokenAccount = await getAccount(private_connection, tokenAccountAddress);
  
      // Token balance in the smallest unit (lamports)
      const lamports = tokenAccount.amount;
  
      // Convert lamports to token units
      return Number(lamports);
    } catch (error) {
      // console.error("Error fetching SPL token balance:", error);
      return 0; // Return 0 if an error occurs
    }
  }
}
