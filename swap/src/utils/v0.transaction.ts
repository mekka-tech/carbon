import {
    AddressLookupTableProgram,
    Connection,
    Keypair,
    PublicKey,
    Signer,
    TransactionInstruction,
    TransactionMessage,
    VersionedTransaction,
} from '@solana/web3.js'
import { getSignature } from './get.signature'
import { wait } from './wait'
import { private_connection } from '../config'
import { Wallet } from '@coral-xyz/anchor'
import { JitoBundleService } from '../services/jito.bundle'
import { genericBlockchainError } from '../pump/utils'

const COMMITMENT_LEVEL = 'confirmed'

export async function signV0Transaction(instructions: TransactionInstruction[], wallet: Wallet, signers: Signer[]): Promise<VersionedTransaction> {
    const { blockhash, lastValidBlockHeight } = await private_connection.getLatestBlockhash()

    const messageV0 = new TransactionMessage({
        payerKey: wallet.publicKey,
        recentBlockhash: blockhash,
        instructions,
    }).compileToV0Message()

    const transaction = new VersionedTransaction(messageV0)
    transaction.sign([wallet as unknown as Signer, ...signers])
    return transaction
}

export async function sendRawTransactionOrBundle(transaction: VersionedTransaction, jitoFeeValueWei: number) {
    const rawTransaction = transaction.serialize()
    const signature = getSignature(transaction)

    if (jitoFeeValueWei == 0) {
        await private_connection.sendRawTransaction(rawTransaction)
        console.log(`https://solscan.io/tx/${signature}`)
        return { bundleId: '', signature }
    }

    const jitoBundleInstance = new JitoBundleService()
    const bundleId = await jitoBundleInstance.sendTransaction(rawTransaction)
    if (!bundleId) throw new Error('JITO_BUNDLE_ERROR')
    console.log(`https://solscan.io/tx/${signature}`)
    return { bundleId, signature }
}

export async function trySimulateTransaction(transaction: VersionedTransaction, is_buy: boolean, mint: string) {
    const { value: simulatedTransactionResponse } = await private_connection.simulateTransaction(transaction, {
        replaceRecentBlockhash: true,
        commitment: 'processed',
    })

    const { err, logs } = simulatedTransactionResponse

    if (err) {
        console.error('SIMULATION_TRANSACTION_ERROR', {
            is_buy,
            mint,
        }, err, logs)
        
        let humanMessage = 'SWAP_FAILED'
        if (logs?.length) {
            const match = logs.find((line) =>
                line.includes('custom program error'),
            )

            if (match) {
                const code = match.split('custom program error: ')[1]?.trim()
                humanMessage = genericBlockchainError(code)
                console.log('⛔ Error en la blockchain:', humanMessage)
            } else {
                console.log('Error sin código personalizado:', err, logs)
            }
        }
        throw new Error(humanMessage)
    }
}

export const getSignatureStatus = async (signature: string) => {
    try {
        const maxRetry = 30
        let retries = 0
        while (retries < maxRetry) {
            await wait(1_000)
            retries++

            const tx = await private_connection.getSignatureStatus(signature, {
                searchTransactionHistory: false,
            })
            if (tx?.value?.err) {
                console.log('JitoTransaction Failed')
                break
            }
            if (tx?.value?.confirmationStatus === 'confirmed' || tx?.value?.confirmationStatus === 'finalized') {
                retries = 0
                console.log('JitoTransaction confirmed!!!')
                break
            }
        }

        if (retries > 0) return false
        return true
    } catch (e) {
        return false
    }
}
