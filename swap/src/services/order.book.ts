export enum OrderStatus {
    PENDING = 'pending',
    OPEN = 'open',
    CLOSED = 'closed'
}

export enum Side {
    BUY = 'buy',
    SELL = 'sell'
}

export interface Order {
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
    bonding_curve: string;
    associated_bonding_curve: string;
}

export class OrderBook {
    private orders: Map<string, Order> = new Map();
    
    constructor() {
        console.log('Order book initialized');
    }
    
    // Get an order by creator and mint
    _getOrder(mint: string): Order | undefined {
        return this.orders.get(mint);
    }
    
    // Add or update an order
    _addOrder(order: Order): Order {
        if (!this.orders.has(order.mint)) {
            this.orders.set(order.mint, order);
        }
        return order;
    }
    _updateOrder(order: Order): Order {
        this.orders.set(order.mint, order);
        return order;
    }
    getOrderStatus(mint: string): OrderStatus | undefined {
        return this._getOrder(mint)?.status;
    }
    
    // Process a trade
    processTrade(mint: string, side: Side, price: number, amount: number, origin: string, signature: string, bondingCurve: string, associatedBondingCurve: string): Order | undefined {
        const order = this._getOrder(mint);
        if (!order && side === Side.BUY) {
            if (!bondingCurve || !associatedBondingCurve) { return undefined }
            console.log('============================= NEW PENDING ORDER ====================================')
            console.log(`https://solscan.io/tx/${signature}`)
            console.log(`---------------------------------------------`)
            return this._addOrder({
                mint,
                amount_bought: amount,
                amount_sold: 0,
                price_bought: price,
                price_sold: 0,
                timestamp_bought: Date.now(),
                timestamp_sold: 0,
                pnl: 0,
                status: OrderStatus.PENDING,
                origin: origin,
                bonding_curve: bondingCurve,
                associated_bonding_curve: associatedBondingCurve
            });
        } else if (order && order.status === OrderStatus.PENDING && side === Side.BUY) {
            console.log('============================= OPEN ORDER ====================================')
            let text = 'Order =>'
            if (side === Side.BUY) {
              text = `[${mint}] BUY => ${amount} => $${price} USD => ${price * amount} TOTAL`
            } else {
              text = `[${mint}] SELL => ${amount} => $${price} USD => ${price * amount} TOTAL`;
            }
            console.log(text);
            console.log(`https://solscan.io/tx/${signature}`)
            console.log(`---------------------------------------------`)
            return this._updateOrder({
                mint,
                amount_bought: amount,
                amount_sold: 0,
                price_bought: price,
                price_sold: 0,
                timestamp_bought: Date.now(),
                timestamp_sold: 0,
                pnl: 0,
                status: OrderStatus.OPEN,
                origin: origin,
                bonding_curve: bondingCurve,
                associated_bonding_curve: associatedBondingCurve
            });
        } else if (order && order.status === OrderStatus.OPEN && side === Side.SELL) {
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
                console.log(`==================================\nPOSITION CLOSED => ${origin.toUpperCase()}\n[${order.mint.substring(0, 4)}-${order.mint.substring(order.mint.length - 6)}] PNL: ${pnl.toFixed(2)} (${pnl_percentage.toFixed(4)}%)\n==================================`)
                console.log(`https://solscan.io/tx/${signature}`)
                console.log(`---------------------------------------------`)
            } else if (origin === 'stop_loss' || origin === 'take_profit') {
                order.amount_sold += order.amount_bought;
                order.price_sold = price;
                order.timestamp_sold = Date.now();
                order.pnl = pnl;
                order.status = OrderStatus.CLOSED;
                order.origin = origin;
                console.log(`==================================\nPOSITION CLOSED => ${origin.toUpperCase()}\n[${order.mint.substring(0, 4)}-${order.mint.substring(order.mint.length - 6)}] PNL: ${pnl.toFixed(2)} (${pnl_percentage.toFixed(4)}%)\n==================================`)
                console.log(`https://solscan.io/tx/${signature}`)
                console.log(`---------------------------------------------`)
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
    

    getClosedOrders(): Order[] {
        return Array.from(this.orders.values()).filter(order => order.status === OrderStatus.CLOSED);
    }
    
} 
