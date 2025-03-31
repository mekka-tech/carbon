import 'dotenv/config'

import { Commitment } from "@solana/web3.js"
import { Connection } from "@solana/web3.js"

export const MAINNET_RPC = process.env.MAINNET_RPC || 'https://api.mainnet-beta.solana.com'
export const PRIVATE_RPC_ENDPOINT = process.env.PRIVATE_RPC_ENDPOINT || 'https://api.mainnet-beta.solana.com'
export const RPC_WEBSOCKET_ENDPOINT = process.env.RPC_WEBSOCKET_ENDPOINT || 'wss://api.mainnet-beta.solana.com/ws'

export const COMMITMENT_LEVEL = 'processed' as Commitment
export const connection = new Connection(MAINNET_RPC, COMMITMENT_LEVEL)
export const private_connection = new Connection(PRIVATE_RPC_ENDPOINT, {
    commitment: COMMITMENT_LEVEL,
    wsEndpoint: RPC_WEBSOCKET_ENDPOINT,
})
