export enum OrderStatus {
    OPEN = 'open',
    CLOSED = 'closed'
}

export enum Side {
    BUY = 'buy',
    SELL = 'sell'
}

export interface Order {
    creator: string;
    mint: string;
    amount_bought: number;
    amount_sold: number;
    price_bought: number;
    price_sold: number;
    timestamp_bought: number;
    timestamp_sold: number;
    pnl: number;
    status: OrderStatus;
    origin: string;
}

export class OrderBook {
    private orders: Map<string, Map<string, Order>> = new Map();
    
    constructor() {
        console.log('Order book initialized');
    }
    
    // Get an order by creator and mint
    _getOrder(mint: string, creator: string): Order | undefined {
        const creatorOrders = this.orders.get(mint);
        if (!creatorOrders) return undefined;
        return creatorOrders.get(creator);
    }
    
    // Add or update an order
    _addOrder(order: Order): Order {
        if (!this.orders.has(order.mint)) {
            this.orders.set(order.mint, new Map());
        }
        this.orders.get(order.mint)!.set(order.creator, order);
        return order;
    }
    
    // Process a trade
    processTrade(creator: string, mint: string, side: Side, price: number, amount: number, origin: string, signature: string): Order | undefined {
        const order = this._getOrder(mint, creator);
        if (!order && side === Side.BUY) {
            let text = 'Order =>'
            if (side === Side.BUY) {
              text = `[${creator}] [${mint}] BUY => ${amount} => $${price} USD => ${price * amount} TOTAL`
            } else {
              text = `[${creator}] [${mint}] SELL => ${amount} => $${price} USD => ${price * amount} TOTAL`;
            }
            console.log(text);
            console.log(`https://solscan.io/tx/${signature}`)
            return this._addOrder({
                creator,
                mint,
                amount_bought: amount,
                amount_sold: 0,
                price_bought: price,
                price_sold: 0,
                timestamp_bought: Date.now(),
                timestamp_sold: 0,
                pnl: 0,
                status: OrderStatus.OPEN,
                origin: origin
            });
        } else if (order !== undefined && order.status === OrderStatus.OPEN && side === Side.SELL) {
            const priceDiff = price - order.price_bought;
            const pnl_percentage = (priceDiff / order.price_bought) * 100;
            const pnl = priceDiff * order.amount_bought
            if (pnl_percentage >= 20 && origin === 'normal') {
                order.amount_sold += order.amount_bought;
                order.price_sold = price;
                order.timestamp_sold = Date.now();
                order.pnl = pnl;
                order.status = OrderStatus.CLOSED;
                order.origin = origin;
                console.log(`[${order.creator}] [${order.mint}] PNL: ${pnl.toFixed(2)} (${pnl_percentage.toFixed(4)}%) POSITION CLOSED`)
                console.log(`https://solscan.io/tx/${signature}`)
            } else if (origin === 'stop_loss' || origin === 'take_profit') {
                order.amount_sold += order.amount_bought;
                order.price_sold = price;
                order.timestamp_sold = Date.now();
                order.pnl = pnl;
                order.status = OrderStatus.CLOSED;
                order.origin = origin;
                console.log(`[${order.creator}] [${order.mint}] PNL: ${pnl.toFixed(2)} (${pnl_percentage.toFixed(4)}%) POSITION CLOSED`)
                console.log(`https://solscan.io/tx/${signature}`)
            } else {
                return undefined;
            }
            return order;
        }


        // if (side === Side.BUY) {
        //     order.amount_bought += amount;
        //     order.price_bought = price;
        //     order.timestamp_bought = Date.now();
        // } else {
        //     order.amount_sold += amount;  
        //     order.price_sold = price;
        //     order.timestamp_sold = Date.now();
        // }

        // order.pnl = order.amount_bought * order.price_bought - order.amount_sold * order.price_sold;
        
        // // Update order status if needed
        // if (order.amount_bought > 0 && order.amount_sold > 0) {
        //     if (order.amount_bought === order.amount_sold) {
        //         order.status = OrderStatus.FILLED;
        //     }
        // }

        // this._addOrder(creator, order);
        
        return order;
    }
    
    // Get all orders for a specific creator
    getOrdersByCreator(creator: string): Order[] {
        const creatorOrders = this.orders.get(creator);
        if (!creatorOrders) return [];
        return Array.from(creatorOrders.values());
    }
    
    // Get all orders for a specific mint
    getOrdersByMint(mint: string): Order[] {
        const result: Order[] = [];
        this.orders.forEach(creatorOrders => {
            const order = creatorOrders.get(mint);
            if (order) result.push(order);
        });
        return result;
    }
    
    // Get a snapshot of the entire order book
    getOrderBookSnapshot(): { [creator: string]: Order[] } {
        const snapshot: { [creator: string]: Order[] } = {};
        this.orders.forEach((creatorOrders, creator) => {
            snapshot[creator] = Array.from(creatorOrders.values());
        });
        return snapshot;
    }
    
    // Calculate total volume for a specific mint
    calculateMintVolume(mint: string): { buyVolume: number, sellVolume: number } {
        let buyVolume = 0;
        let sellVolume = 0;
        
        this.orders.forEach(creatorOrders => {
            const order = creatorOrders.get(mint);
            if (order) {
                buyVolume += order.amount_bought * order.price_bought;
                sellVolume += order.amount_sold * order.price_sold;
            }
        });
        
        return { buyVolume, sellVolume };
    }
    
    // Get market summary for a specific mint
    getMarketSummary(mint: string) {
        const orders = this.getOrdersByMint(mint);
        const volume = this.calculateMintVolume(mint);
        
        let highestBid = 0;
        let lowestAsk = Number.MAX_VALUE;
        
        orders.forEach(order => {
            if (order.price_bought > highestBid) {
                highestBid = order.price_bought;
            }
            if (order.price_sold < lowestAsk && order.price_sold > 0) {
                lowestAsk = order.price_sold;
            }
        });
        
        return {
            mint,
            highestBid,
            lowestAsk: lowestAsk === Number.MAX_VALUE ? 0 : lowestAsk,
            spread: lowestAsk === Number.MAX_VALUE ? 0 : lowestAsk - highestBid,
            buyVolume: volume.buyVolume,
            sellVolume: volume.sellVolume
        };
    }
} 
