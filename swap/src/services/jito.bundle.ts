import bs58 from 'bs58'
import axios from 'axios'
import { wait } from '../utils/wait'

const MAX_CHECK_JITO = 20

type Region = 'ams' | 'ger' | 'ny' | 'tokyo' | 'default'

// Region => Endpoint
export const endpoints = {
    default: 'https://mainnet.block-engine.jito.wtf',
    ams: 'https://amsterdam.mainnet.block-engine.jito.wtf',
    ger: 'https://frankfurt.mainnet.block-engine.jito.wtf',
    ny: 'https://ny.mainnet.block-engine.jito.wtf',
    tokyo: 'https://tokyo.mainnet.block-engine.jito.wtf',
}

const regions = ['ams', 'ger', 'ny', 'tokyo', 'default'] as Region[] // "default",
let idx = 0

export const JitoTipAmount = 0.001

export const tipAccounts = [
    '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5',
    'HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe',
    'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY',
    'ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49',
    'DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh',
    'ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt',
    'DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL',
    '3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT',
]

interface JitoFeeResponse {
    time: string;
    landed_tips_25th_percentile: number;
    landed_tips_50th_percentile: number;
    landed_tips_75th_percentile: number;
    landed_tips_95th_percentile: number;
    landed_tips_99th_percentile: number;
    ema_landed_tips_50th_percentile: number;
}

export class JitoBundleService {
    endpoint: string
    static currentJitoFee: number = JitoTipAmount;

    // constructor(_region: Region) {
    constructor() {
        this.endpoint = endpoints.default
    }

    updateRegion() {
        this.endpoint = regions[idx] || endpoints.default
        idx = (idx + 1) % regions.length
    }
    async sendBundle(serializedTransaction: Uint8Array) {
        const encodedTx = bs58.encode(serializedTransaction)
        // const jitoURL = `${this.endpoint}/api/v1/bundles?uuid=${JITO_UUID}`; // ?uuid=${JITO_UUID}
        const jitoURL = `${this.endpoint}/api/v1/bundles` // ?uuid=${JITO_UUID}
        const payload = {
            jsonrpc: '2.0',
            id: 1,
            method: 'sendBundle',
            params: [[encodedTx]],
        }

        try {
            const response = await axios.post(jitoURL, payload, {
                headers: { 'Content-Type': 'application/json' },
            })
            return response.data.result
        } catch (error) {
            console.error('cannot send!:', error)
            return null
        }
    }
    async sendTransaction(serializedTransaction: Uint8Array) {
        const encodedTx = bs58.encode(serializedTransaction)
        const jitoURL = `${this.endpoint}/api/v1/transactions?uuid=${process.env.JITO_AUTH_TOKEN}`
        // const jitoURL = `${this.endpoint}/api/v1/bundles?uuid=${JITO_UUID}`
        const payload = {
            jsonrpc: '2.0',
            id: 1,
            method: 'sendTransaction',
            params: [encodedTx],
        }

        try {
            const response = await axios.post(jitoURL, payload, {
                headers: { 'Content-Type': 'application/json' },
            })
            return response.data.result
        } catch (error) {
            // console.error("Error:", error);
            throw new Error('cannot send!')
        }
    }

    async getBundleStatus(bundleId: string) {
        const payload = {
            jsonrpc: '2.0',
            id: 1,
            method: 'getBundleStatuses',
            params: [[bundleId]],
        }

        let retries = 0
        while (retries < MAX_CHECK_JITO) {
            retries++
            try {
                this.updateRegion()
                // const jitoURL = `${this.endpoint}/api/v1/bundles?uuid=${JITO_UUID}`; // ?uuid=${JITO_UUID}
                const jitoURL = `${this.endpoint}/api/v1/bundles` // ?uuid=${JITO_UUID}
                // console.log("retries", jitoURL);

                const response = await axios.post(jitoURL, payload, {
                    headers: { 'Content-Type': 'application/json' },
                })
                // console.log("🚀 ~ getBundleStatus ~ response:", response)

                if (!response || response.data.result.value.length <= 0) {
                    await wait(1000)
                    continue
                }

                const bundleResult = response.data.result.value[0]
                if (bundleResult.confirmation_status === 'confirmed' || bundleResult.confirmation_status === 'finalized') {
                    retries = 0
                    console.log('JitoTransaction confirmed!!!')
                    break
                }
            } catch (error) {
                console.error('GetBundleStatus Failed')
            }
        }
        if (retries === 0) return true
        return false
    }

    static async updateJitoFee() {
        try {
            const response = await axios.get('https://bundles.jito.wtf/api/v1/bundles/tip_floor');
            const feeData: JitoFeeResponse[] = response.data;
            
            if (feeData && feeData.length > 0) {
                // Using the EMA of 50th percentile as a reasonable fee
                const emaFee = feeData[0].landed_tips_95th_percentile;
                
                // Convert from SOL to lamports (1 SOL = 10^9 lamports)
                this.currentJitoFee = +emaFee * 1.2
                
                console.log(`Updated Jito fee: ${this.currentJitoFee} lamports (${emaFee} SOL)`);
                return this.currentJitoFee;
            }
            
            return this.currentJitoFee;
        } catch (error) {
            console.error('Failed to update Jito fee:', error);
            return this.currentJitoFee;
        }
    }
    
    static getCurrentJitoFee(): number {
        return this.currentJitoFee;
    }
}
