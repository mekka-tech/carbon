use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Buy,
    Sell,
}

/// A Position represents a net position for a given user and token.
/// For simplicity, we assume that a positive quantity means a long position,
/// and that a trade in the opposite side reduces the position.
#[derive(Debug, Clone)]
struct Position {
    user: String,
    mint: String,
    side: Side,     // Side at which the position was opened.
    open_price: f64,   // Average price at open (in USD).
    quantity: f64,     // Quantity of tokens held.
    current_price: f64 // Latest market price (for PNL calculations).
}

impl Position {
    /// Calculate the current profit and loss.
    fn pnl(&self, closing_price: f64) -> f64 {
        match self.side {
            Side::Buy => (closing_price - self.open_price) * self.quantity,
            Side::Sell => (self.open_price - closing_price) * self.quantity,
        }
    }
}

/// OrderBook holds positions keyed by (user, mint).
#[derive(Debug)]
struct OrderBook {
    positions: HashMap<(String, String), Position>,
}

impl OrderBook {
    fn new() -> Self {
        OrderBook {
            positions: HashMap::new(),
        }
    }

    /// Process a trade.
    ///
    /// * `user` is the user identifier.
    /// * `mint` is the token mint address.
    /// * `trade_side` is the side of the trade. (For our example, we assume:
    ///    - a buy trade indicates the user is buying tokens (thus going long),
    ///    - a sell trade indicates the user is selling tokens (thus closing a long position).)
    /// * `trade_price` is the price (in USD) of this trade.
    /// * `trade_quantity` is the quantity of tokens in the trade.
    ///
    /// The function returns an Option<f64> with the realized PNL if a position is closed (fully or partially).
    fn process_trade(
        &mut self,
        user: &str,
        mint: &str,
        trade_side: Side,
        trade_price: f64,
        trade_quantity: f64,
    ) -> Option<f64> {
        let key = (user.to_string(), mint.to_string());

        if let Some(pos) = self.positions.get_mut(&key) {
            // There is an existing position.
            if pos.side == trade_side {
                // Same side: increase the position.
                // Update the weighted average open price.
                let total_cost = pos.open_price * pos.quantity;
                let trade_cost = trade_price * trade_quantity;
                pos.quantity += trade_quantity;
                pos.open_price = (total_cost + trade_cost) / pos.quantity;
                pos.current_price = trade_price;
                None
            } else {
                // Opposite side: this trade is closing or reducing the position.
                if trade_quantity >= pos.quantity {
                    // The trade quantity is enough to close the position completely.
                    let realized_pnl = pos.pnl(trade_price);
                    self.positions.remove(&key);
                    Some(realized_pnl)
                } else {
                    // Partial close: reduce the position quantity and calculate PNL for the closed portion.
                    let closed_quantity = trade_quantity;
                    let realized_pnl = match pos.side {
                        Side::Buy => (trade_price - pos.open_price) * closed_quantity,
                        Side::Sell => (pos.open_price - trade_price) * closed_quantity,
                    };
                    pos.quantity -= closed_quantity;
                    // Optionally update pos.current_price if needed.
                    pos.current_price = trade_price;
                    Some(realized_pnl)
                }
            }
        } else {
            // No existing position: open a new one with the trade's side.
            let pos = Position {
                user: user.to_string(),
                mint: mint.to_string(),
                side: trade_side,
                open_price: trade_price,
                quantity: trade_quantity,
                current_price: trade_price,
            };
            self.positions.insert(key, pos);
            None
        }
    }
}

// Create a global static order book for use in our price checker.
static ORDER_BOOK: Lazy<Mutex<OrderBook>> = Lazy::new(|| Mutex::new(OrderBook::new()));

fn main() {
    // Example usage:
    // Let's assume "Alice" buys 100 tokens at $10 each (a long position).
    {
        let mut ob = ORDER_BOOK.lock().unwrap();
        // A buy trade means the user is going long.
        ob.process_trade("Alice", "TOKEN", Side::Buy, 10.0, 100.0);
    }
    // Now, later, "Alice" sells 100 tokens at $12 each, which should close her position.
    {
        let mut ob = ORDER_BOOK.lock().unwrap();
        if let Some(pnl) = ob.process_trade("Alice", "TOKEN", Side::Sell, 12.0, 100.0) {
            println!("Alice's PNL from closing position: ${}", pnl);
        }
    }
    
    // In your price checker, you can now access ORDER_BOOK to check open positions,
    // update positions with new trade prices, and compute unrealized PNL.
}
