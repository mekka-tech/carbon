import * as WebSocket from 'ws';

// Create a WebSocket server that listens on port 3012
const wss = new WebSocket.Server({ port: 3012 });

console.log('WebSocket server started on port 3012');

// Handle new connections
wss.on('connection', (ws: WebSocket) => {
  console.log('Client connected');

  // Handle messages from clients
  ws.on('message', (message: Buffer) => {
    const data = message.toString('utf-8');
    console.log('Received message:', data);
    
    // You can process the message here
    
    // Example: Echo the message back to the client
    ws.send(`Server received: ${data}`);
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
