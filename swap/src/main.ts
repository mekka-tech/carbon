import 'dotenv/config';
import * as WebSocket from 'ws';
import { OrderBook, Side, OrderStatus, Order } from './services/order.book';
import { DiscordWebhookService } from './services/discord.webhook';
import { nonBlockingWrapper } from './utils/nonBlockingWrapper';
import { pumpFunSwap } from './pump/swap';
import { LAMPORTS_PER_SOL } from '@solana/web3.js';
import { getKeyPairFromPrivateKey } from './pump/utils';
import { JitoBundleService } from './services/jito.bundle';
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

setInterval(() => {
  JitoBundleService.updateJitoFee()
}, 30_000);
JitoBundleService.updateJitoFee().catch((error: any) => {
  console.error('Failed to update Jito fee:', error);
})

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


const payer = getKeyPairFromPrivateKey(process.env.PRIVATE_KEY!)

const CREATORS = ['744ZryTiFQ1LDySKUikc93M7MT7ZdB3DnFGsrT1gYhNW']

const BUY_AMOUNT = 0.1
const SOL_PRICE = 130
const GAS_FEE = 0.005
const SLIPPAGE = 50

const EXPIRED_ORDERS: Order[] = []
setInterval(() => {
  const expiredOrders = orderBook.getExpiredOrders()
  if (expiredOrders.length > 0) {
    expiredOrders.forEach((order) => {
      if (!EXPIRED_ORDERS.includes(order))  {
        EXPIRED_ORDERS.push(order)
      }
    })
  }

  EXPIRED_ORDERS.forEach(async(order) => {
    orderBook.processTrade(
      order.mint,
      Side.SELL,
      order.price_bought,
      order.amount_bought,
      order.origin,
      'InternalInternalInternal',
      order.bonding_curve,
      order.associated_bonding_curve
    )
    await pumpFunSwap(
      payer,
      order.mint,
      order.price_bought,
      order.bonding_curve,
      order.associated_bonding_curve,
      6,
      false,
      order.amount_bought,
      GAS_FEE,
      SLIPPAGE,
      JitoBundleService.getCurrentJitoFee(),
      orderBook
    )
  })
}, 10_000);

// Handle new connections
wss.on('connection', (ws: WebSocket) => {
  console.log('Client connected');

  // Handle messages from clients
  ws.on('message', async (message: Buffer) => {
    const data = JSON.parse(message.toString('utf-8')) as SwapOrder;
    // Process the trade in the order book
    const side = data.is_buy ? Side.BUY : Side.SELL;
    const tokenPriceOnSol = parseFloat(data.sol_amount) / parseFloat(data.amount)
    const amount = parseFloat(data.amount);
    
    const previousOrderStatus = orderBook.getOrderStatus(data.mint)
    if (CREATORS.includes(data.creator)) {
      // Process my trade
      const order = orderBook.processTrade(
        data.mint,
        side,
        tokenPriceOnSol,
        amount,
        data.origin,
        data.signature,
        data.bonding_curve,
        data.associated_bonding_curve
      );
      if (order && order.status === OrderStatus.CLOSED && previousOrderStatus !== order.status) {
        await pumpFunSwap(
          payer,
          data.mint,
          tokenPriceOnSol,
          order.bonding_curve,
          order.associated_bonding_curve,
          6,
          data.is_buy,
          order.amount_bought,
          GAS_FEE,
          SLIPPAGE,
          JitoBundleService.getCurrentJitoFee(),
          orderBook
        )
      }
    } else {
      const order = orderBook.processTrade(
        data.mint,
        side,
        tokenPriceOnSol,
        amount,
        data.origin,
        data.signature,
        data.bonding_curve,
        data.associated_bonding_curve
      ); 
      if (order && order.status === OrderStatus.PENDING && previousOrderStatus !== order.status && data.is_buy === true) {
        await pumpFunSwap(
          payer,
          data.mint,
          tokenPriceOnSol,
          order.bonding_curve,
          order.associated_bonding_curve,
          6,
          data.is_buy,
          BUY_AMOUNT,
          GAS_FEE,
          SLIPPAGE,
          JitoBundleService.getCurrentJitoFee(),
          orderBook
        )
      } else if (order && order.status === OrderStatus.CLOSED && previousOrderStatus !== order.status && data.is_buy === false) {
        await pumpFunSwap(
          payer,
          data.mint,
          tokenPriceOnSol,
          order.bonding_curve,
          order.associated_bonding_curve,
          6,
          data.is_buy,
          order.amount_bought,
          GAS_FEE,
          SLIPPAGE,
          JitoBundleService.getCurrentJitoFee(),
          orderBook
        )
      }
    }
    
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
