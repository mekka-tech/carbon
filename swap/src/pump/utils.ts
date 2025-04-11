import { ComputeBudgetProgram, Keypair } from '@solana/web3.js'
import { Connection, PublicKey, Transaction, TransactionInstruction, sendAndConfirmTransaction } from '@solana/web3.js'
import bs58 from 'bs58'

export function getKeyPairFromPrivateKey(key: string) {
    return Keypair.fromSecretKey(new Uint8Array(bs58.decode(key)))
}

export async function createTransaction(
    connection: Connection,
    instructions: TransactionInstruction[],
    payer: PublicKey,
    priorityFeeInSol: number = 0,
): Promise<Transaction> {
    const modifyComputeUnits = ComputeBudgetProgram.setComputeUnitLimit({
        units: 1400000,
    })

    const transaction = new Transaction().add(modifyComputeUnits)

    if (priorityFeeInSol > 0) {
        const microLamports = priorityFeeInSol * 1_000_000_000 // convert SOL to microLamports
        const addPriorityFee = ComputeBudgetProgram.setComputeUnitPrice({
            microLamports,
        })
        transaction.add(addPriorityFee)
    }

    transaction.add(...instructions)

    transaction.feePayer = payer
    transaction.recentBlockhash = (await connection.getRecentBlockhash()).blockhash
    return transaction
}

export async function sendAndConfirmTransactionWrapper(connection: Connection, transaction: Transaction, signers: any[]) {
    try {
        const signature = await sendAndConfirmTransaction(connection, transaction, signers, {
            skipPreflight: true,
            preflightCommitment: 'confirmed',
        })
        console.log('Transaction confirmed with signature:', signature)
        return signature
    } catch (error) {
        console.error('Error sending transaction:', error)
        return null
    }
}

export function bufferFromUInt64(value: number | string) {
    let buffer = Buffer.alloc(8)
    buffer.writeBigUInt64LE(BigInt(value))
    return Uint8Array.from(buffer)
}

export const blockchainErrorMap: Record<string, string> = {
  "0xbc4": "Network occupied, try again later.", // The program expected this account to be already initialized.
  "3012": "Network occupied, try again later.",
  "6005": "Token migrated to Raydium.",
  "0x1765": "Token migrated to Raydium.",
  "0x1": "Another error occurred",
  "6": "IncorrectProgramId",
};

export function genericBlockchainError(code: string): string {
  const normalizedCode = code.toLowerCase();

  // Primero busca tal cual está
  if (blockchainErrorMap[normalizedCode]) {
    return blockchainErrorMap[normalizedCode];
  }

  // Intenta parsear como número
  let parsedHex: string | null = null;
  try {
    if (normalizedCode.startsWith("0x")) {
      parsedHex = parseInt(normalizedCode, 16).toString(); // de hex a decimal string
    } else {
      parsedHex = "0x" + parseInt(normalizedCode).toString(16); // de decimal a hex string
    }
  } catch (err) {
    return `Unknown error code: ${code}`;
  }

  // Intenta encontrar el error con la versión parseada
  if (blockchainErrorMap[parsedHex]) {
    return blockchainErrorMap[parsedHex];
  }
  if (blockchainErrorMap[parsedHex.toString()]) {
    return blockchainErrorMap[parsedHex.toString()];
  }

  return `Unknown error code: ${code}`;
}

