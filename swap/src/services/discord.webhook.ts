import axios from 'axios';
import { Order } from './order.book';

interface EmbedField {
  name: string;
  value: string;
  inline?: boolean;
}

interface DiscordEmbed {
  title?: string;
  description?: string;
  color?: number;
  fields?: EmbedField[];
  footer?: {
    text: string;
    icon_url?: string;
  };
  timestamp?: string;
}

interface WebhookPayload {
  username?: string;
  avatar_url?: string;
  content?: string;
  embeds?: DiscordEmbed[];
}

export class DiscordWebhookService {
  private webhookUrl: string = 'https://discord.com/api/webhooks/1356801829815713792/rZluu8ouZaEBfoy9cbk4-SpOho58Y2pP3sF9nJmDXvIopny1jAps1AiRfF9D6sdYwBpt';

  /**
   * Send a PNL summary of closed orders to Discord
   * @param orders Array of closed orders to summarize
   */
  async sendPnlSummary(initialBalance: number, currentBalance: number, executedOrders: number): Promise<void> {
    if (!this.webhookUrl) {
      return;
    }
    try {
      // Calculate total PNL
      const totalPnl = currentBalance - initialBalance;
      const totalPnlFormatted = totalPnl.toFixed(4);
      
      // Determine color based on PNL (green for positive, red for negative)
      const color = totalPnl >= 0 ? 0x00FF00 : 0xFF0000;
      
      // Create the embed
      const embed: DiscordEmbed = {
        title: '🔔 PNL Summary',
        description: `**Total PNL: ${totalPnl >= 0 ? '+' : ''}${totalPnlFormatted}**`,
        color: color,
        fields: [
          {
            name: 'Current Balance',
            value: `${currentBalance.toFixed(4)} SOL`,
            inline: false
          },
          {
            name: 'Initial Balance',
            value: `${initialBalance.toFixed(4)} SOL`,
            inline: false
          },
          {
            name: 'Executed Orders',
            value: `${executedOrders}`,
            inline: false
          }
        ],
        footer: {
          text: 'SuperSwap Bot'
        },
        timestamp: new Date().toISOString()
      };
      
      // Create the webhook payload
      const payload: WebhookPayload = {
        username: 'Kortopi',
        embeds: [embed]
      };
      
      // Send the webhook
      await axios.post(this.webhookUrl, payload);
      console.log('Discord PNL summary sent successfully');
    } catch (error) {
      console.error('Failed to send Discord PNL summary:', error);
    }
  }
} 