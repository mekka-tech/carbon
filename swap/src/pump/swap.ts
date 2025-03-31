import {
    LAMPORTS_PER_SOL,
    PublicKey,
    SystemProgram,
    Transaction,
    TransactionInstruction,
} from '@solana/web3.js'
import {
    getAssociatedTokenAddress,
    createAssociatedTokenAccountInstruction,
    TOKEN_PROGRAM_ID,
    createAssociatedTokenAccountIdempotentInstruction,
    createSyncNativeInstruction,
    NATIVE_MINT,
    getAssociatedTokenAddressSync,
    createCloseAccountInstruction,
} from '@solana/spl-token'
import {
    getKeyPairFromPrivateKey,
    bufferFromUInt64,
} from './utils'
import {
    GLOBAL,
    FEE_RECIPIENT,
    SYSTEM_PROGRAM_ID,
    RENT,
    PUMP_FUN_ACCOUNT,
    PUMP_FUN_PROGRAM,
    ASSOC_TOKEN_ACC_PROG,
} from './constants'
import { sendRawTransactionOrBundle, signV0Transaction, trySimulateTransaction } from '../utils/v0.transaction'
import { Wallet } from '@coral-xyz/anchor'
import { getFeeInstruction, getJitoFeeInstruction } from '../utils/swap.instructions'


export async function pumpFunSwap(
    payerPrivateKey: string,
    mintStr: string,
    price: number,
    bondingCurve: string,
    associatedBondingCurve: string,
    decimal: number,
    is_buy: boolean,
    _amount: number,
    gasFee: number,
    _slippage: number,
    mevFee: number,
): Promise<any> {
    try {

        const jitoFeeValueWei = is_buy ? BigInt((mevFee * 10 ** 9).toFixed()) : BigInt(0)

        const txBuilder = new Transaction()

        const payer = await getKeyPairFromPrivateKey(payerPrivateKey)
        const owner = payer.publicKey
        const mint = new PublicKey(mintStr)
        const slippage = _slippage / 100
        let total_fee_in_sol = 0

        const inDecimal = is_buy ? 9 : decimal

        let amountWithDecimals = Math.floor(_amount * 10 ** inDecimal)

        const tokenAccountIn = getAssociatedTokenAddressSync(is_buy ? NATIVE_MINT : mint, owner, true)
        const tokenAccountOut = getAssociatedTokenAddressSync(is_buy ? mint : NATIVE_MINT, owner, true)

        const tokenAccountAddress = await getAssociatedTokenAddress(mint, owner, false)

        const keys = getSwapKeys(owner, mint, tokenAccountAddress, bondingCurve, associatedBondingCurve, is_buy)

        let data: Buffer
        let quoteAmount = 0

        if (is_buy) {

            const tokenOut = Math.floor((amountWithDecimals * price) / LAMPORTS_PER_SOL)
            const solInWithSlippage = amountWithDecimals * (1 + slippage)
            const maxSolCost = Math.floor(solInWithSlippage * LAMPORTS_PER_SOL)

            data = Buffer.concat([bufferFromUInt64('16927863322537952870'), bufferFromUInt64(tokenOut), bufferFromUInt64(maxSolCost)])

            quoteAmount = tokenOut
            amountWithDecimals = amountWithDecimals
        } else {
            const minSolOutput = Math.floor(((_amount * price) * LAMPORTS_PER_SOL) * (1 - slippage))
            data = Buffer.concat([bufferFromUInt64('12502976635542562355'), bufferFromUInt64(amountWithDecimals), bufferFromUInt64(minSolOutput)])
            quoteAmount = minSolOutput
        }

        const instruction = new TransactionInstruction({
            keys: keys,
            programId: PUMP_FUN_PROGRAM,
            data: data,
        })
        txBuilder.add(instruction)

        const swapInstructions = txBuilder.instructions

        console.log({ gasFee })

        const [feeInstruction, jitoFeeInstruction] = await Promise.all([
            getFeeInstruction(gasFee),
            getJitoFeeInstruction(payer as unknown as Wallet, Number(jitoFeeValueWei)),
        ])

        console.log('Is_BUY', is_buy)
        const instructions: TransactionInstruction[] = is_buy
            ? [
                ...feeInstruction,
                ...jitoFeeInstruction,
                createAssociatedTokenAccountIdempotentInstruction(owner, tokenAccountIn, owner, NATIVE_MINT),
                SystemProgram.transfer({
                    fromPubkey: owner,
                    toPubkey: tokenAccountIn,
                    lamports: amountWithDecimals,
                }),
                createSyncNativeInstruction(tokenAccountIn, TOKEN_PROGRAM_ID),
                createAssociatedTokenAccountIdempotentInstruction(owner, tokenAccountOut, owner, new PublicKey(mint)),
                ...swapInstructions,
                // Unwrap WSOL for SOL
                createCloseAccountInstruction(tokenAccountIn, owner, owner),            ]
            : [
                ...feeInstruction,
                createAssociatedTokenAccountIdempotentInstruction(owner, tokenAccountOut, owner, NATIVE_MINT),
                ...swapInstructions,
                // Unwrap WSOL for SOL
                createCloseAccountInstruction(tokenAccountOut, owner, owner),
            ]

        const transaction = await signV0Transaction(instructions, payer as unknown as Wallet, [])

        await trySimulateTransaction(transaction)

        const { bundleId, signature } = await sendRawTransactionOrBundle(transaction, is_buy ? mevFee : 0)

        const quote = { inAmount: amountWithDecimals, outAmount: quoteAmount }

        return {
            quote,
            total_fee_in_sol: total_fee_in_sol || 0,
            bundleId,
            success: true,
            tokenAddress: mintStr,
            txHash: signature,
        }

    } catch (error: any) {
        // await discordLogger.error('Pumpfun swap failed', {
        //     mintStr,
        //     decimal,
        //     _amount,
        //     _slippage,
        //     gasFee,
        //     isFeeBurn,
        //     username,
        //     isToken2022,
        //     error,
        // })

        console.log(' - Swap pump token is failed', error)
        return {
            success: false,
            error: (error as Error)?.message?.toString(),
            txHash: '',
            tokenAddress: mintStr,
            bundleId: '',
            quote: { inAmount: _amount, outAmount: 0 },
        }

    }
}

function getSwapKeys(
    owner: PublicKey,
    mint: PublicKey,
    tokenAccountAddress: PublicKey,
    bondingCurve: string,
    associatedBondingCurve: string,
    is_buy: boolean,
): Array<{ pubkey: PublicKey; isSigner: boolean; isWritable: boolean }> {
    return [
        { pubkey: GLOBAL, isSigner: false, isWritable: false },
        { pubkey: FEE_RECIPIENT, isSigner: false, isWritable: true },
        { pubkey: mint, isSigner: false, isWritable: false },
        {
            pubkey: new PublicKey(bondingCurve),
            isSigner: false,
            isWritable: true,
        },
        {
            pubkey: new PublicKey(associatedBondingCurve),
            isSigner: false,
            isWritable: true,
        },
        { pubkey: tokenAccountAddress, isSigner: false, isWritable: true },
        { pubkey: owner, isSigner: false, isWritable: true },
        { pubkey: SYSTEM_PROGRAM_ID, isSigner: false, isWritable: false },
        {
            pubkey: is_buy ? TOKEN_PROGRAM_ID : ASSOC_TOKEN_ACC_PROG,
            isSigner: false,
            isWritable: false,
        },
        {
            pubkey: is_buy ? RENT : TOKEN_PROGRAM_ID,
            isSigner: false,
            isWritable: false,
        },
        { pubkey: PUMP_FUN_ACCOUNT, isSigner: false, isWritable: false },
        { pubkey: PUMP_FUN_PROGRAM, isSigner: false, isWritable: false },
    ]
}
