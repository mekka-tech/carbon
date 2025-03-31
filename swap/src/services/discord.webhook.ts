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
  private webhookUrl: string = 'https://discord.com/api/webhooks/1356321233699995678/nkUw_Q-N_l2TAXp8c-NYh0o3eLzbxManNDFZT5QziMfrTr02Udjkb453ufjO5ziVG3eU';

  /**
   * Send a PNL summary of closed orders to Discord
   * @param orders Array of closed orders to summarize
   */
  async sendPnlSummary(orders: Order[]): Promise<void> {
    if (!this.webhookUrl || orders.length === 0) {
      return;
    }

    try {
      // Calculate total PNL
      const totalPnl = orders.reduce((sum, order) => sum + (order.pnl || 0), 0);
      const totalPnlFormatted = totalPnl.toFixed(4);
      
      // Determine color based on PNL (green for positive, red for negative)
      const color = totalPnl >= 0 ? 0x00FF00 : 0xFF0000;
      
      // Create fields for each order
      const fields: EmbedField[] = orders.map(order => {
        const pnlFormatted = (order.pnl || 0).toFixed(4);
        const pnlSign = order.pnl >= 0 ? '+' : '';
        
        return {
          name: `Order #${order.mint.substring(0, 4)}-${order.mint.substring(order.mint.length - 6)}`,
          value: `**Entry:** ${order.price_bought}\n**Exit:** ${order.price_sold}\n**PNL:** ${pnlSign}${pnlFormatted}`,
          inline: true
        };
      });
      
      // Create the embed
      const embed: DiscordEmbed = {
        title: '🔔 PNL Summary - Closed Orders',
        description: `**Total PNL: ${totalPnl >= 0 ? '+' : ''}${totalPnlFormatted}**`,
        color: color,
        fields: fields,
        footer: {
          text: 'Pump.fun Swap Bot'
        },
        timestamp: new Date().toISOString()
      };
      
      // Create the webhook payload
      const payload: WebhookPayload = {
        username: 'Swap PNL Tracker',
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