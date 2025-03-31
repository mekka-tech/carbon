use once_cell::sync::OnceCell;
use tungstenite::{connect, WebSocket, stream::MaybeTlsStream, Message};
use std::{
  fmt,
  net::TcpStream
};
use serde::{Serialize, Deserialize};
use carbon_core::error::{Error, CarbonResult};

use std::sync::Mutex;
use once_cell::sync::Lazy;

// Global OnceCell to hold the initialized publisher, wrapped in a Box.
static GLOBAL_SWAP_PUBLISHER: OnceCell<Box<SwapPublisher>> = OnceCell::new();
pub static SOCKET: Lazy<Mutex<SwapPublisher>> = Lazy::new(|| Mutex::new(SwapPublisher::new()));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapOrder {
  pub mint: String,
  pub amount: String,
  pub sol_amount: String,
  pub bonding_curve: String,
  pub associated_bonding_curve: String,
  pub decimal: u8,
  pub is_buy: bool,
}

pub struct SwapPublisher {
  pub socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl SwapPublisher {
  fn new() -> Self {
    let (mut socket, response) = connect("ws://localhost:3012").expect("Can't connect");
    socket.send(Message::Text("Copy Bot Client Started".into())).unwrap_or(());
    SwapPublisher {
      socket,
    }
  }
}
