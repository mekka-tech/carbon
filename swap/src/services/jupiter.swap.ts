import { Connection, Keypair, VersionedTransaction } from '@solana/web3.js';
import fetch from 'cross-fetch';
import bs58 from 'bs58';
import { private_connection } from '../config';
import { NATIVE_MINT } from '@solana/spl-token';


const swap = async (keypair: Keypair, inputMint: string, outputMint: string, amount: number, slippage: number) => {
  
  // Swapping SOL to USDC with input 0.1 SOL and 0.5% slippage
  const quoteResponse = await (
    await fetch(`https://quote-api.jup.ag/v6/quote?inputMint=${inputMint}\
  &outputMint=${outputMint}\
  &amount=${amount}\
  &slippageBps=${slippage}`
    )
  ).json();
  // console.log({ quoteResponse })

  // get serialized transactions for the swap
  const { swapTransaction } = await (
    await fetch('https://quote-api.jup.ag/v6/swap', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        // quoteResponse from /quote api
        quoteResponse,
        // user public key to be used for the swap
        userPublicKey: keypair.publicKey.toString(),
        // auto wrap and unwrap SOL. default is true
        wrapAndUnwrapSol: true,
        // Optional, use if you want to charge a fee.  feeBps must have been passed in /quote API.
        // feeAccount: "fee_account_public_key"
      })
    })
  ).json();

  // deserialize the transaction
  const swapTransactionBuf = Buffer.from(swapTransaction, 'base64');
  var transaction = VersionedTransaction.deserialize(swapTransactionBuf);
  console.log(transaction);

  // sign the transaction
  transaction.sign([keypair]);

  // Execute the transaction
  const rawTransaction = transaction.serialize()
  const txid = await private_connection.sendRawTransaction(rawTransaction, {
    skipPreflight: true,
    maxRetries: 2
  });
  
  console.log(`https://solscan.io/tx/${txid}`);

  return txid;
}

export const buyWithJupiter = async (keypair: Keypair, mint: string, amount: number, slippage: number) => {
  const txid = await swap(keypair, NATIVE_MINT.toString(), mint, amount * 1e9, slippage);
  return txid;
}

export const sellWithJupiter = async (keypair: Keypair, mint: string, amount: number, decimal: number, slippage: number) => {
  const txid = await swap(keypair, mint, NATIVE_MINT.toString(), amount * 10 ** decimal, slippage);
  return txid;
}