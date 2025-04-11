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
  decimals: number;
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
let MIN_BALANCE = parseFloat(process.env.MIN_BALANCE_INITIAL || '2')
let PROFIT_LOCK_PERCENTAGE = parseFloat(process.env.PROFIT_LOCK_PERCENTAGE || '0.8') // % of profits get locked
let MIN_BALANCE_PERCENTAGE = parseFloat(process.env.MIN_BALANCE_PERCENTAGE || '80') // % of balance to keep

const updateBalances = async () => {
  const balance = await private_connection.getBalance(payer.publicKey)
  let balanceInSol = balance / LAMPORTS_PER_SOL
  if (INITIAL_BALANCE === 0) {
    INITIAL_BALANCE = balanceInSol
    MIN_BALANCE = INITIAL_BALANCE - ((100 - MIN_BALANCE_PERCENTAGE) * INITIAL_BALANCE / 100);
  }
  console.log('===============================================')
  if (balanceInSol !== CURRENT_BALANCE) {  
    const diff = balanceInSol - CURRENT_BALANCE
    console.log(`${diff > 0 ? '+' : ''}${diff} SOL`)
    CURRENT_BALANCE = balanceInSol
    
    // Update MIN_BALANCE if we have profits
    if (CURRENT_BALANCE > INITIAL_BALANCE) {
      const totalProfit = CURRENT_BALANCE - INITIAL_BALANCE
      const profitToLock = totalProfit * PROFIT_LOCK_PERCENTAGE
      const newInitialBalance = INITIAL_BALANCE + profitToLock
      MIN_BALANCE = newInitialBalance - ((100 - MIN_BALANCE_PERCENTAGE) * newInitialBalance / 100);
      console.log(`MIN_BALANCE updated to: ${MIN_BALANCE.toFixed(4)} SOL`)
    }
  }
  console.log('BALANCE:', +CURRENT_BALANCE.toFixed(4))
  console.log('MIN_BALANCE:', +MIN_BALANCE.toFixed(4))
  console.log('INITIAL_BALANCE:', +INITIAL_BALANCE.toFixed(4))
  console.log('===============================================')
}
setInterval(() => {
  updateBalances().catch((error: any) => {
    console.error('Failed to update balances:', error);
  })
}, 10_000)
updateBalances().then(() => {
  // discordWebhook.sendPnlSummary(INITIAL_BALANCE, CURRENT_BALANCE, orderBook.getClosedOrders().length);
}).catch((error: any) => {
  console.error('Failed to update balances:', error);
})



setInterval(() => {
  discordWebhook.sendPnlSummary(INITIAL_BALANCE, CURRENT_BALANCE, orderBook.getClosedOrders().length);
}, 600_000);

const SWAP_SIMULATE = process.env.SWAP_SIMULATE === 'true'
const BUY_AMOUNT = parseFloat(process.env.BUY_AMOUNT || '0.1')
const GAS_FEE = parseFloat(process.env.GAS_FEE || '0.001')
const SLIPPAGE = parseFloat(process.env.SLIPPAGE || '30')
const JITO_TIP_AMOUNT = parseFloat(process.env.JITO_TIP_AMOUNT ?? '0.0001')
const MAX_JITO_FEE = Math.min(JITO_TIP_AMOUNT, parseFloat(process.env.MAX_JITO_FEE || '0.001'))
const TIME_DIFF_PERMITTED = parseFloat(process.env.TIME_DIFF_PERMITTED || '3')

console.log({SWAP_SIMULATE, BUY_AMOUNT, GAS_FEE, SLIPPAGE, JITO_TIP_AMOUNT, MAX_JITO_FEE, TIME_DIFF_PERMITTED})

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
    //console.log(data)

    const now = new Date().getTime()
    const timeDiff = now - data.timestamp
    console.log(now, data.timestamp, timeDiff)
    console.log(`[${data.mint}] TIME_DIFF ${timeDiff}`)

    if (timeDiff > TIME_DIFF_PERMITTED) {
      console.log(`SKIPPING ORDER BECAUSE OF TIME DIFF`)
      return
    }

    // Process the trade in the order book
    const side = data.is_buy ? Side.BUY : Side.SELL;
    if (side === Side.BUY && CURRENT_BALANCE < MIN_BALANCE) {
      return
    }
    const tokenPriceOnSol = parseFloat(data.sol_amount) / parseFloat(data.amount)
    const amount = parseFloat(data.amount);
    const isMe = CREATORS.includes(data.creator)
    const closeAccount = true
    const decimals = data.decimals ?? 6
    
    const previousOrderStatus = orderBook.getOrderStatus(data.mint)

    if (SWAP_SIMULATE) {
      console.log('SIMULATE ORDER:', data)
      return
    }

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
          decimals,
          data.is_buy,
          order.amount_bought,
          GAS_FEE,
          SLIPPAGE,
          MAX_JITO_FEE,
          closeAccount,
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
          decimals,
          data.is_buy,
          BUY_AMOUNT,
          GAS_FEE,
          SLIPPAGE,
          MAX_JITO_FEE,
          closeAccount,
          orderBook
        )
      } else if (order && order.status === OrderStatus.CLOSED && previousOrderStatus !== order.status && data.is_buy === false) {
        await pumpFunSwap(
          payer,
          data.mint,
          tokenPriceOnSol,
          order.bonding_curve,
          order.associated_bonding_curve,
          decimals,
          data.is_buy,
          order.amount_bought,
          GAS_FEE,
          SLIPPAGE,
          MAX_JITO_FEE,
          closeAccount,
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
