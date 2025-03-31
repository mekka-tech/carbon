import { ComputeBudgetProgram, PublicKey, SystemProgram } from '@solana/web3.js'
import { Wallet } from '@coral-xyz/anchor'
import { tipAccounts } from '../services/jito.bundle'
import { createAssociatedTokenAccountIdempotentInstruction } from '@solana/spl-token'

export const calculateMicroLamports = (gasvalue: number, cu: number) => {
    const adjustedGas = Math.max(gasvalue - 0.000005, 0);
    const microlamports = (adjustedGas * (10 ** 15 / cu)).toFixed(0);
    const result = Math.max(Number(microlamports), 1);
    return result;
}

export async function getFeeInstruction(gasValue: number) {
    const cu = 1_000_000
    const microLamports = calculateMicroLamports(gasValue, cu)

    const feeInstruction = [
        ComputeBudgetProgram.setComputeUnitPrice({ microLamports: microLamports }),
        ComputeBudgetProgram.setComputeUnitLimit({ units: cu }),
    ]
    return feeInstruction
}

export async function getJitoFeeInstruction(wallet: Wallet, jitoFeeValueWei: number) {
    return jitoFeeValueWei > 0
        ? [
              SystemProgram.transfer({
                  fromPubkey: wallet.publicKey,
                  toPubkey: new PublicKey(tipAccounts[0]),
                  lamports: jitoFeeValueWei,
              }),
          ]
        : []
}


export async function getCreateAccountInstruction(wallet: Wallet, tokenAccountOut: PublicKey, mint: string) {
    return createAssociatedTokenAccountIdempotentInstruction(wallet.publicKey, tokenAccountOut, wallet.publicKey, new PublicKey(mint))
}
