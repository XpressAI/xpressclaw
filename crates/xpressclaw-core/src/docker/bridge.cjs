// TCP over an owned Docker exec stream. Neither side needs a routable container IP.
const net = require('node:net');
const readline = require('node:readline');
const [direction, rawPort] = process.argv.slice(1);
const port = Number(rawPort);
const sockets = new Map();
let nextId = 0;
let server;
function send(message) {
  if (!process.stdout.write(JSON.stringify(message) + '\n')) {
    for (const socket of sockets.values()) socket.pause();
  }
}
process.stdout.on('drain', () => { for (const socket of sockets.values()) socket.resume(); });
function track(id, socket) {
  sockets.set(id, socket);
  socket.on('data', data => send({ type: 'data', id, data: data.toString('base64') }));
  socket.on('end', () => send({ type: 'end', id }));
  socket.on('error', () => socket.destroy());
  socket.on('close', () => { sockets.delete(id); send({ type: 'close', id }); });
}
if (direction === 'host_to_container') {
  server = net.createServer({ allowHalfOpen: true }, socket => {
    if (sockets.size >= 64) { socket.destroy(); return; }
    const id = ++nextId;
    send({ type: 'open', id });
    track(id, socket);
  });
  server.on('error', error => { send({ type: 'error', message: error.message }); process.exitCode = 1; process.stdin.destroy(); });
  server.listen(port, '127.0.0.1', () => send({ type: 'ready' }));
} else {
  send({ type: 'ready' });
}
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on('line', line => {
  try {
    if (line.length > 100000) throw new Error('Oversized bridge frame');
    const message = JSON.parse(line);
    if (message.type === 'open' && direction === 'container_to_host') {
      if (sockets.size >= 64 || sockets.has(message.id)) return;
      track(message.id, net.createConnection({ host: '127.0.0.1', port, allowHalfOpen: true }));
    } else {
      const socket = sockets.get(message.id);
      if (message.type === 'data' && socket) {
        if (!socket.write(Buffer.from(message.data, 'base64')) && !socket.waitingDrain) {
          socket.waitingDrain = true;
          lines.pause();
          const resume = () => {
            socket.waitingDrain = false;
            socket.removeListener('drain', resume);
            socket.removeListener('close', resume);
            lines.resume();
          };
          socket.once('drain', resume);
          socket.once('close', resume);
        }
      } else if (message.type === 'end') socket?.end();
      else if (message.type === 'close') socket?.destroy();
    }
  } catch { process.exitCode = 1; lines.close(); }
});
lines.on('close', () => {
  server?.close();
  for (const socket of sockets.values()) socket.destroy();
  process.exit();
});
