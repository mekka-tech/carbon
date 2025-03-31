use once_cell::sync::OnceCell;
use tungstenite::{connect, WebSocket, stream::MaybeTlsStream, Message};
use std::{
  fmt,
  net::TcpStream
};
use serde::{Serialize, Deserialize};
use carbon_core::error::{Error, CarbonResult};

// Global OnceCell to hold the initialized publisher, wrapped in a Box.
static GLOBAL_SWAP_PUBLISHER: OnceCell<Box<&mut SwapPublisher>> = OnceCell::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapOrder {
  pub mint: String,
  pub amount: String,
  pub price: String,
  pub bonding_curve: String,
  pub associated_bonding_curve: String,
  pub decimal: u8,
  pub is_buy: bool,
}

pub struct SwapPublisher {
  socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl SwapPublisher {
  /// Asynchronously creates a new publisher instance and stores it globally.
  pub async fn init() -> CarbonResult<()> {
    let (mut socket, response) = connect("ws://localhost:3012").expect("Can't connect");
    socket.send(Message::Text("Copy Bot Started".into())).unwrap();
    let mut publisher = SwapPublisher { socket };
    GLOBAL_SWAP_PUBLISHER.set(Box::new(&mut publisher));
    Ok(())
  }

  async fn _publish_swap_order(
    &mut self,
    swap_order: &SwapOrder,
  ) -> CarbonResult<()> {
    let message = serde_json::to_string(&swap_order).unwrap_or("{}".to_string());
    self.socket.send(Message::Text(message.into()));
    Ok(())
  }

  /// Static async method to publish a SwapResult via the global instance.
  /// This allows you to call:
  pub async fn publish_swap_order(
    swap_order: &SwapOrder,
  ) -> CarbonResult<()> {
    if let Some(publisher) = GLOBAL_SWAP_PUBLISHER.get() {
        publisher._publish_swap_order(swap_order).await
    } else {
        Err(Error::Custom("Global publisher not initialized".to_string()))
    }
  }
}
