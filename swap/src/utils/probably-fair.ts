import { createHash } from 'crypto'

export class ProbablyFair {
    private serverSeed: string
    private clientSeed: string
    private nonce: number

    constructor(userId: string) {
        this.serverSeed = this.generateServerSeed(userId)
        this.clientSeed = this.generateClientSeed(userId)
        this.nonce = 0
    }

    private generateRandomHex(length: number): string {
        const chars = '0123456789ABCDEF'
        let result = ''
        for (let i = 0; i < length; i++) {
            result += chars[Math.floor(Math.random() * chars.length)]
        }
        return result
    }

    private generateServerSeed(userId: string): string {
        const timestamp = Date.now()
        const randomHex = this.generateRandomHex(6)
        const seedBase = `${userId}-${timestamp}-${randomHex}`
        return createHash('sha256').update(seedBase).digest('hex')
    }

    private generateClientSeed(userId: string): string {
        const timestamp = Date.now()
        return `${userId}-${timestamp}`
    }

    public roll(min: number, max: number): number {
        const hash = createHash('sha256').update(`${this.serverSeed}:${this.clientSeed}:${this.nonce++}`).digest('hex')

        // Use first 5 characters of hash to generate number
        const decimal = parseInt(hash.substring(0, 5), 16)

        // Scale the number to our desired range
        return min + (decimal % (max - min + 1))
    }

    public getServerSeed(): string {
        return this.serverSeed
    }

    public getClientSeed(): string {
        return this.clientSeed
    }
}
