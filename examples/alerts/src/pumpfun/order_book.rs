use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;


#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Position {
    user: String,
    mint: String,
    side: Side,         // Side at which the position was opened.
    open_price: f64,    // Average price at open (in USD).
    quantity: f64,      // Quantity of tokens held.
    pub current_price: f64, // Latest market price (for PNL calculations).
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

/// OrderBook holds positions keyed by (user, mint) and a secondary mapping from mint to key.
#[derive(Debug)]
pub struct OrderBook {
    positions: HashMap<(String, String), Position>,
    keys: HashMap<String, (String, String)>, // maps mint -> (user, mint)
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
    /// If there's an existing position for (user, mint), update it.
    /// If no position exists and the trade is a Buy, open a new one.
    /// If a position is closed completely, remove it and the key mapping.
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
            // Existing position.
            if pos.side == trade_side {
                // Same side: increase the position.
                let total_cost = pos.open_price * pos.quantity;
                let trade_cost = trade_price * trade_quantity;
                pos.quantity += trade_quantity;
                pos.open_price = (total_cost + trade_cost) / pos.quantity;
                pos.current_price = trade_price;
                None
            } else {
                // Opposite side: trade reduces the position.
                if trade_quantity >= pos.quantity {
                    // Trade closes the position completely.
                    let realized_pnl = pos.pnl(trade_price);
                    self.positions.remove(&key);
                    self.keys.remove(mint);
                    Some(realized_pnl)
                } else {
                    // Partial close.
                    let closed_quantity = trade_quantity;
                    let realized_pnl = match pos.side {
                        Side::Buy => (trade_price - pos.open_price) * closed_quantity,
                        Side::Sell => (pos.open_price - trade_price) * closed_quantity,
                    };
                    pos.quantity -= closed_quantity;
                    pos.current_price = trade_price;
                    Some(realized_pnl)
                }
            }
        } else {
            if trade_side == Side::Buy {
                // Open a new position.
                let pos = Position {
                    user: user.to_string(),
                    mint: mint.to_string(),
                    side: trade_side,
                    open_price: trade_price,
                    quantity: trade_quantity,
                    current_price: trade_price,
                };
                self.keys.insert(mint.to_string(), key.clone());
                self.positions.insert(key, pos);
                None
            } else {
                // No position to reduce for a Sell trade.
                None
            }
        }
    }

    /// Retrieve a position by mint address.
    pub fn get_position(&self, mint: &str) -> Option<&Position> {
        self.keys.get(mint).and_then(|key| self.positions.get(key))
    }
}

// Create a global static order book for use in our price checker.
pub static ORDER_BOOK: Lazy<Mutex<OrderBook>> = Lazy::new(|| Mutex::new(OrderBook::new()));

