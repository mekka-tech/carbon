import * as crypto from 'crypto'

export const retrieveEnvVariable = (variableName: string) => {
    const variable = process.env[variableName] || ''
    if (!variable) {
        console.error(`${variableName} is not set`)
        process.exit(1)
    }
    return variable
}

export const sleep = async (ms: number) => {
    await new Promise((resolve) => setTimeout(resolve, ms))
}

export const isValidWalletAddress = (address: string): boolean => {
    if (!address) return false
    const pattern: RegExp = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/

    return pattern.test(address)
}

export function isValidEthereumAddress(address: string): boolean {
    if (!address) return false
    const pattern: RegExp = /^0x[a-fA-F0-9]{40}$/

    return pattern.test(address)
}

const checkAddressInArray = (tokenList: any, address: string) => {
    return tokenList.some((token: any) => token.address == address)
}

export const generateReferralCode = (length: number) => {
    let code = ''
    // Convert the Telegram username to a hexadecimal string
    const characters = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'
    for (let i = 0; i < length; i++) {
        code += characters.charAt(Math.floor(Math.random() * characters.length))
    }
    return code
}

export function formatNumber(number: bigint | string | number) {
    if (!number) return '0'
    // Convert the number to a string and add commas using regular expression
    return number.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',')
}

export function formatKMB(val: bigint | string | number) {
    if (!val) return '0'
    if (Number(val) > 1000000000) {
        return `${(Number(val) / 1000000000).toFixed(1)}B`
    }
    if (Number(val) > 1000000) {
        return `${(Number(val) / 1000000).toFixed(1)}M`
    }
    if (Number(val) > 1000) {
        return `${(Number(val) / 1000).toFixed(1)}k`
    }
    return Number(val).toFixed(3)
}


export function formatPrice(price: number) {
    if (!price) return 0
    if (price <= 0) return 0
    // If the price is less than 1, format it to 6 decimal places
    if (price < 1) {
        let decimal = 15
        while (1) {
            if (price * 10 ** decimal < 1) {
                break
            }
            decimal--
        }
        return price.toFixed(decimal + 3)
    }
    // If the price is greater than or equal to 1, format it to 3 decimal places
    return price.toFixed(2)
}

export const copytoclipboard = (text: string) => {
    return `<code class="text-entity-code clickable" role="textbox" tabindex="0" data-entity-type="MessageEntityCode">${text}</code>`
}

export const isEqual = (a: number, b: number) => {
    return Math.abs(b - a) < 0.001
}

export const fromWeiToValue = (wei: string | number, decimal: number) => {
    return Number(wei) / 10 ** decimal
}

export const generateCode = (username: string): string => {
    // Get the current date-time in milliseconds
    const timestamp = Date.now().toString()
    const randomValue = Math.floor(Math.random() * 1000000).toString(36) // Random value in base36
    const baseString = username + timestamp + randomValue

    const hash = crypto.createHash('sha256').update(baseString).digest('hex')

    return hash.slice(0, 8) // Truncate the hash to 8 characters, or adjust as needed
}

interface GetCaptionParams {
  status: string;
  suffix?: string;
  amount: number;
  solPrice: number;
  name: string;
  symbol: string;
  mint: string;
  isToken2022: boolean;
}

export const getCaption = ({
  status,
  suffix = '',
  amount,
  solPrice,
  name,
  symbol,
  mint,
  isToken2022,
}: GetCaptionParams) => {
  return (
    `▪️ Token: <b>${name ?? 'undefined'} (${symbol ?? 'undefined'})</b> ` +
    `${isToken2022 ? '<i>Token2022</i>' : ''}\n` +
    `<i>${copytoclipboard(mint)}</i>\n` +
    status +
    `💲 <b>Value: ${amount} SOL ($ ${(amount * solPrice).toFixed(3)})</b>\n` +
    suffix
  );
};
