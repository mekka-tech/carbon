import 'dotenv/config';
import * as WebSocket from 'ws';
import { OrderBook, Side, OrderStatus, Order } from './services/order.book';
import { DiscordWebhookService } from './services/discord.webhook';
import { nonBlockingWrapper } from './utils/nonBlockingWrapper';
import { pumpFunSwap } from './pump/swap';
import { LAMPORTS_PER_SOL } from '@solana/web3.js';
import { getKeyPairFromPrivateKey } from './pump/utils';
import { JitoBundleService } from './services/jito.bundle';
import { OWNER_ADDRESS, private_connection } from './config';
// Create a WebSocket server that listens on port 3012
const wss = new WebSocket.Server({ port: 3012 });

// Initialize the order book
const orderBook = new OrderBook();
const discordWebhook = new DiscordWebhookService();


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

const CREATORS = [OWNER_ADDRESS]
let CURRENT_BALANCE = 0
let INITIAL_BALANCE = 0
const updateBalances = async () => {
  const balance = await private_connection.getBalance(payer.publicKey)
  let balanceInSol = balance / LAMPORTS_PER_SOL
  if (INITIAL_BALANCE === 0) {
    INITIAL_BALANCE = balanceInSol
  }
  console.log('===============================================')
  if (balanceInSol !== CURRENT_BALANCE) {  
    const diff = balanceInSol - CURRENT_BALANCE
    console.log(`${diff > 0 ? '+' : ''}${diff} SOL`)
    CURRENT_BALANCE = balanceInSol
  }
  console.log('BALANCE:', CURRENT_BALANCE)
  console.log('===============================================')
    
}
setInterval(() => {
  updateBalances().catch((error: any) => {
    console.error('Failed to update balances:', error);
  })
}, 10_000)
updateBalances().then(() => {
  discordWebhook.sendPnlSummary(INITIAL_BALANCE, CURRENT_BALANCE, orderBook.getClosedOrders().length);
}).catch((error: any) => {
  console.error('Failed to update balances:', error);
})



setInterval(() => {
  discordWebhook.sendPnlSummary(INITIAL_BALANCE, CURRENT_BALANCE, orderBook.getClosedOrders().length);
}, 60_000);

const BUY_AMOUNT = 0.1
const SOL_PRICE = 130
const GAS_FEE = 0.001
const SLIPPAGE = 30
const MIN_BALANCE = 0.1
const MAX_JITO_FEE = 0.001

// const EXPIRED_ORDERS: Order[] = []
// setInterval(() => {
//   const notExpiredOrders: Order[] = [] 
//   EXPIRED_ORDERS.forEach((order) => {
//     if (order.status === OrderStatus.CLOSED ) {
//       notExpiredOrders.push(order)
//     }
//   });
//   notExpiredOrders.forEach((order) => {
//     EXPIRED_ORDERS.splice(EXPIRED_ORDERS.indexOf(order), 1)
//   })

//   const expiredOrders = orderBook.getExpiredOrders()
//   if (expiredOrders.length > 0) {
//     expiredOrders.forEach((order) => {
//       if (!EXPIRED_ORDERS.includes(order))  {
//         EXPIRED_ORDERS.push(order)
//       }
//     })
//   }

//   EXPIRED_ORDERS.forEach(async(order) => {
//     console.log('============================= EXPIRED ORDER ====================================')
//     console.log(`${order.mint.substring(0, 4)}-${order.mint.substring(order.mint.length - 6)}`)
//     console.log(`---------------------------------------------`)
//     orderBook.processTrade(
//       order.mint,
//       Side.SELL,
//       order.price_bought,
//       order.amount_bought,
//       "stop_loss",
//       'InternalInternalInternal',
//       order.bonding_curve,
//       order.associated_bonding_curve
//     )
//     await pumpFunSwap(
//       payer,
//       order.mint,
//       order.price_bought,
//       order.bonding_curve,
//       order.associated_bonding_curve,
//       6,
//       false,
//       order.amount_bought,
//       GAS_FEE,
//       SLIPPAGE,
//       MAX_JITO_FEE,
//       orderBook
//     )
//   })
// }, 10_000);

// Handle new connections
wss.on('connection', (ws: WebSocket) => {
  console.log('Client connected');

  // Handle messages from clients
  ws.on('message', async (message: Buffer) => {
    const data = JSON.parse(message.toString('utf-8')) as SwapOrder;

    const timeDiff = Date.now() - data.timestamp
    console.log(`[${data.mint}] TIME_DIFF ${timeDiff}`)

    // Process the trade in the order book
    const side = data.is_buy ? Side.BUY : Side.SELL;
    if (side === Side.BUY && CURRENT_BALANCE < MIN_BALANCE) {
      return
    }
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
          MAX_JITO_FEE,
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
          MAX_JITO_FEE,
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
          MAX_JITO_FEE,
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
