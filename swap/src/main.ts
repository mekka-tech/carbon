import * as WebSocket from 'ws';
import { OrderBook, OrderStatus, Side } from './services/order.book';
import axios from 'axios';
import { DiscordWebhookService } from './services/discord.webhook';

// Create a WebSocket server that listens on port 3012
const wss = new WebSocket.Server({ port: 3012 });

// Initialize the order book
const orderBook = new OrderBook();
const discordWebhook = new DiscordWebhookService();


setInterval(() => {
  const closedOrders = orderBook.getClosedOrders();
  if (closedOrders.length > 0) {
    discordWebhook.sendPnlSummary(closedOrders);
  }
}, 60_000);


console.log('WebSocket server started on port 3012');


interface SwapOrder {
  creator: string;
  mint: string;
  amount: string;
  sol_amount: string;
  bonding_curve: string;
  associated_bonding_curve: string;
  decimal: number;
  is_buy: boolean;
  origin: string;
  timestamp: number;
  signature: string;
}
enum Origin {
  STOP_LOSS = 'stop_loss',
  TAKE_PROFIT = 'take_profit',
  NORMAL = 'normal',
  UPDATE = 'update',
}


const CREATORS = ['CkMbUezSZm6eteRBg5vJLDmxXL4YcSPT6zJtrBwjDWU4']

const SOL_PRICE = 130

// Handle new connections
wss.on('connection', (ws: WebSocket) => {
  console.log('Client connected');

  // Handle messages from clients
  ws.on('message', (message: Buffer) => {
    const data = JSON.parse(message.toString('utf-8')) as SwapOrder;
    // Process the trade in the order book
    const side = data.is_buy ? Side.BUY : Side.SELL;
    const price = parseFloat(data.sol_amount) / parseFloat(data.amount) * SOL_PRICE;
    const amount = parseFloat(data.amount);
    
    // Process the trade
    orderBook.processTrade(
      data.mint,
      side,
      price,
      amount,
      data.origin,
      data.signature
    );
    
  });

  // Handle client disconnection
  ws.on('close', () => {
    console.log('Client disconnected');
  });

  // Handle connection errors
  ws.on('error', (error: Error) => {
    console.error('WebSocket error:', error);
  });
  
});


// Handle server errors
wss.on('error', (error: Error) => {
  console.error('WebSocket server error:', error);
});

// You'll need to keep the process running
// If this is part of a larger application, you might not need this
process.on('SIGINT', () => {
  console.log('Shutting down WebSocket server');
  wss.close();
  process.exit(0);
});
