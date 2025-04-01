use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;
use crate::orders::order_position::{EnhancedPosition, PositionAction};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub user: String,
    pub mint: String,
    pub side: Side,         // Side at which the position was opened.
    pub open_price: f64,    // Average price at open (in USD).
    pub quantity: f64,      // Quantity of tokens held.
    pub current_price: f64, // Latest market price (for PNL calculations
    pub enhanced_position: EnhancedPosition,
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

/// OrderBook holds positions keyed by (user, mint) and a secondary mapping from mint to keys.
#[derive(Debug)]
pub struct OrderBook {
    positions: HashMap<(String, String), Position>,
    // Maps mint -> list of (user, mint) keys that have positions for this mint
    mint_keys: HashMap<String, Vec<(String, String)>>,
}

impl OrderBook {
    fn new() -> Self {
        OrderBook {
            positions: HashMap::new(),
            mint_keys: HashMap::new(),
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
    ) -> Option<PositionAction> {
        let key = (user.to_string(), mint.to_string());

        if let Some(pos) = self.positions.get_mut(&key) {
            let action = pos.enhanced_position.process_price_update(trade_price);
            // Existing position.
            if pos.side == trade_side {
                // Same side: increase the position.
                let total_cost = pos.open_price * pos.quantity;
                let trade_cost = trade_price * trade_quantity;
                pos.quantity += trade_quantity;
                pos.open_price = (total_cost + trade_cost) / pos.quantity;
                pos.current_price = trade_price;
            } else {
                // Opposite side: trade reduces the position.
                if trade_quantity >= pos.quantity {
                    // Trade closes the position completely.
                    let realized_pnl = pos.pnl(trade_price);
                    self.positions.remove(&key);
                    
                    // Remove key from mint_keys
                    if let Some(keys) = self.mint_keys.get_mut(mint) {
                        keys.retain(|k| k != &key);
                        if keys.is_empty() {
                            self.mint_keys.remove(mint);
                        }
                    }
                    
                } else {
                    // Partial close.
                    let closed_quantity = trade_quantity;
                    let realized_pnl = match pos.side {
                        Side::Buy => (trade_price - pos.open_price) * closed_quantity,
                        Side::Sell => (pos.open_price - trade_price) * closed_quantity,
                    };
                    pos.quantity -= closed_quantity;
                    pos.current_price = trade_price;
                }
            }
            return Some(action);
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
                    enhanced_position: EnhancedPosition::new(user.to_string(), mint.to_string(), trade_price),
                };
                // Add key to mint_keys
                self.mint_keys
                    .entry(mint.to_string())
                    .or_insert_with(Vec::new)
                    .push(key.clone());
                
                self.positions.insert(key, pos);
                return Some(PositionAction::HOLD);
            } else {
                // No position to reduce for a Sell trade.
                return None;
            }
        }
    }

    /// Retrieve a position by user and mint address.
    pub fn get_position(&self, user: &str, mint: &str) -> Option<&Position> {
        let key = (user.to_string(), mint.to_string());
        self.positions.get(&key)
    }
    
    /// Retrieve all positions for a given mint address.
    pub fn get_positions_by_mint(&self, mint: &str) -> Vec<&Position> {
        match self.mint_keys.get(mint) {
            Some(keys) => keys
                .iter()
                .filter_map(|key| self.positions.get(key))
                .collect(),
            None => Vec::new(),
        }
    }
    
    /// Retrieve the first position for a given mint (for backward compatibility).
    pub fn get_position_by_mint(&self, mint: &str) -> Option<&Position> {
        self.mint_keys.get(mint)
            .and_then(|keys| keys.first())
            .and_then(|key| self.positions.get(key))
    }
}

// Create a global static order book for use in our price checker.
pub static ORDER_BOOK: Lazy<Mutex<OrderBook>> = Lazy::new(|| Mutex::new(OrderBook::new()));

