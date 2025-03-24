use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

/// A Position represents a net position for a given user and token.
/// For simplicity, we assume that a positive quantity means a long position,
/// and that a trade in the opposite side reduces the position.
#[derive(Debug, Clone)]
pub struct Position {
    user: String,
    mint: String,
    side: Side,     // Side at which the position was opened.
    open_price: f64,   // Average price at open (in USD).
    quantity: f64,     // Quantity of tokens held.
    pub current_price: f64 // Latest market price (for PNL calculations).
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
pub struct OrderBook {
    positions: HashMap<(String, String), Position>,
    keys: HashMap<String, String>,
}

impl OrderBook {
    fn new() -> Self {
        OrderBook {
            positions: HashMap::new(),
            keys: HashMap::new(),
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
    pub fn process_trade(
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
                    self.keys.remove(&mint.to_string());
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
            if trade_side == Side::Buy {
                // No existing position: open a new one with the trade's side.
                let pos = Position {
                    user: user.to_string(),
                mint: mint.to_string(),
                side: trade_side,
                open_price: trade_price,
                quantity: trade_quantity,
                current_price: trade_price,
                };
                self.keys.insert(mint.to_string().clone(), key.to_string().clone());
                self.positions.insert(key, pos);
                None
            } else {
                None
            }
        }
    }

    pub fn has_position(&self, mint: &str) -> Option<&Position> {
        return self.keys.get_key_value(mint).map(|(_, position)| position);
    }

}

// Create a global static order book for use in our price checker.
pub static ORDER_BOOK: Lazy<Mutex<OrderBook>> = Lazy::new(|| Mutex::new(OrderBook::new()));
// Create a global static order book for use in our price checker.
pub static ORDER_BOOK_KEYS: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));

