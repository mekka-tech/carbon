use chrono::Utc;
use std::fmt::{Formatter, Display, Result};  
#[derive(Debug, PartialEq, Clone)]
pub enum Status {
  OPEN,
  CLOSED,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Zone {
  TOP,
  BOTTOM,
  MEDIAN,
}

#[derive(Debug, PartialEq, Clone)]
pub enum PositionAction {
  HOLD,
  EXIT,
}


impl Display for PositionAction {
  fn fmt(&self, f: &mut Formatter<'_>) -> Result {
      match self {
          PositionAction::HOLD => write!(f, "HOLD"),
          PositionAction::EXIT => write!(f, "EXIT"),
      }
  }
}

#[derive(Debug, PartialEq, Clone)]
pub enum PriceRelation {
  INSIDE,
  ABOVE,
  BELOW,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Range {
  pub zone: Zone,
  pub top: f64,
  pub bottom: f64,
  pub top_price: f64,
  pub bottom_price: f64,
  pub duration: i64,
}

impl Range {
  pub fn new(zone: Zone, price: f64, top: f64, bottom: f64, duration: i64) -> Self {
    Self { 
      zone, 
      top_price: price * (1.0 + (top / 100.0)),
      bottom_price: price * (1.0 + (bottom / 100.0)),
      top,
      bottom,
      duration
    }
  }
  pub fn get_price_relation(&self, price: f64) -> PriceRelation {
    if (price >= self.bottom_price && price <= self.top_price) {
      PriceRelation::INSIDE
    } else if (price > self.top_price) {
      PriceRelation::ABOVE
    } else {
      PriceRelation::BELOW
    }
  }
}

#[derive(Debug, PartialEq, Clone)]
pub struct EnhancedPosition {
  // Basic data, user and token mint.
  pub user: String,
  pub mint: String,

  // Status of the position.
  pub status: Status,

  // Entry price of the position.
  pub entry_price: f64,

  // Actual price of the token.
  pub actual_price: f64,

  // Zone of the position.
  pub zone: Zone,

  // Entry timestamp of the position.
  pub entry_timestamp: i64,

  pub range_change_timestamp: i64,

  pub top_range: Range, // TOP RANGE
  pub median_range: Range, // MEDIAN RANGE, After first range change, mediam will be discarded.
  pub bottom_range: Range, // BOTTOM RANGE

  pub range_change_counter: i8, // Counter for range change.
}

impl EnhancedPosition {
  pub fn new(user: String, mint: String, entry_price: f64) -> Self {
    let now = Utc::now().timestamp_millis();
    Self { 
      user,
      mint,
      status: Status::OPEN,
      entry_price,
      actual_price: entry_price,
      zone: Zone::TOP,
      entry_timestamp: now,
      range_change_timestamp: now,
      top_range: Range::new(Zone::TOP, entry_price, 40.0, 20.0, 5000),
      median_range: Range::new(Zone::MEDIAN, entry_price, 20.0, -20.0, 10000),
      bottom_range: Range::new(Zone::BOTTOM, entry_price, -20.0, -40.0, 3000),
      range_change_counter: 0
    }
  }

  pub fn process_price_update(&mut self, price: f64) -> PositionAction {
    let now = Utc::now().timestamp_millis();
    let zone_changed_time = now - self.range_change_timestamp;

    if (self.bottom_range.get_price_relation(price) == PriceRelation::INSIDE && zone_changed_time > self.bottom_range.duration) {
      // EXIT POSITION
      return PositionAction::EXIT;
    } else if (self.bottom_range.get_price_relation(price) == PriceRelation::BELOW) {
      // EXIT POSITION
      return PositionAction::EXIT;
    } else if (self.top_range.get_price_relation(price) == PriceRelation::INSIDE && zone_changed_time > self.top_range.duration) {
      // EXIT POSITION
      return PositionAction::EXIT;
    } else if (self.top_range.get_price_relation(price) == PriceRelation::ABOVE) {
      // Create new ranges for TOP and BOTTOM
      self.bottom_range = Range::new(Zone::BOTTOM, price, self.top_range.top + 20.0, self.top_range.bottom, (self.bottom_range.duration as f64 * 0.8) as i64);
      self.top_range = Range::new(Zone::TOP, price, self.top_range.top + 40.0, self.top_range.top + 20.0, (self.top_range.duration as f64 * 0.8) as i64);
      self.range_change_timestamp = now;
      self.range_change_counter += 1;
      self.actual_price = price;
      return PositionAction::HOLD;
    } else if (self.range_change_counter == 0 && self.median_range.get_price_relation(price) == PriceRelation::INSIDE && zone_changed_time > self.median_range.duration) {
      // EXIT POSITION
      return PositionAction::EXIT;
    }

    return PositionAction::HOLD;
  }
}



#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_new_position() {
        let position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 1.0);
        assert_eq!(position.status, Status::OPEN);
    }

    #[test]
    fn test_process_price_update() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 1.0);
        assert_eq!(position.process_price_update(1.1), PositionAction::HOLD);
    }

    #[test]
    fn test_exit_on_bottom_range_inside() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Set a shorter duration for testing
        position.bottom_range.duration = 50; // 50 milliseconds
        
        // Update price to be inside bottom range
        let price = position.bottom_range.bottom_price + 1.0;
        // First update should return HOLD as we haven't exceeded duration
        assert_eq!(position.process_price_update(price), PositionAction::HOLD);
        
        // Sleep to exceed the duration
        std::thread::sleep(std::time::Duration::from_millis(60));
        
        // Now it should return EXIT
        assert_eq!(position.process_price_update(price), PositionAction::EXIT);
    }
    
    #[test]
    fn test_exit_on_bottom_range_below() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Price below bottom range should immediately exit
        let price = position.bottom_range.bottom_price - 1.0;
        assert_eq!(position.process_price_update(price), PositionAction::EXIT);
    }
    
    #[test]
    fn test_exit_on_top_range_inside() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Set a shorter duration for testing
        position.top_range.duration = 50; // 50 milliseconds
        
        // Update price to be inside top range
        let price = position.top_range.top_price - 1.0;

        // First update should return HOLD as we haven't exceeded duration
        assert_eq!(position.process_price_update(price), PositionAction::HOLD);
        
        // Sleep to exceed the duration
        std::thread::sleep(std::time::Duration::from_millis(60));
        
        // Now it should return EXIT
        assert_eq!(position.process_price_update(price), PositionAction::EXIT);
    }
    
    #[test]
    fn test_range_adjustment_on_price_above_top() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Save original ranges
        let original_bottom_range = position.bottom_range.clone();
        let original_top_range = position.top_range.clone();
        let original_counter = position.range_change_counter;
        
        // Price above top range should adjust ranges
        let price = position.top_range.top_price + 1.0;
        assert_eq!(position.process_price_update(price), PositionAction::HOLD);
        // Verify ranges were adjusted
        assert_ne!(position.bottom_range.top, original_bottom_range.top);
        assert_ne!(position.top_range.top, original_top_range.top);
        assert_eq!(position.range_change_counter, original_counter + 1);
    }
    
    #[test]
    fn test_exit_on_median_range_inside() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Set a shorter duration for testing
        position.median_range.duration = 50; // 50 milliseconds
        
        // Ensure range_change_counter is 0
        position.range_change_counter = 0;
        
        // Update price to be inside median range
        let price = position.median_range.bottom_price + 1.0;
        
        // First update should return HOLD as we haven't exceeded duration
        assert_eq!(position.process_price_update(price), PositionAction::HOLD);
        
        // Sleep to exceed the duration
        std::thread::sleep(std::time::Duration::from_millis(60));
        
        // Now it should return EXIT
        assert_eq!(position.process_price_update(price), PositionAction::EXIT);
    }
    
    #[test]
    fn test_range_adjustment_on_price_above_top_exit_bottom() {
        let mut position = EnhancedPosition::new("user".to_string(), "mint".to_string(), 100.0);
        
        // Save original ranges
        let original_bottom_range = position.bottom_range.clone();
        let original_top_range = position.top_range.clone();
        let original_counter = position.range_change_counter;
        
        // Price above top range should adjust ranges
        let price = position.top_range.top_price + 1.0;
        assert_eq!(position.process_price_update(price), PositionAction::HOLD);
        // Verify ranges were adjusted
        assert_ne!(position.bottom_range.top, original_bottom_range.top);
        assert_ne!(position.top_range.top, original_top_range.top);
        assert_eq!(position.range_change_counter, original_counter + 1);

        let price = position.bottom_range.bottom_price - 1.0;
        assert_eq!(position.process_price_update(price), PositionAction::EXIT);
    }
}