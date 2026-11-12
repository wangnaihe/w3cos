'use strict';
const http = require('http');
const net = require('net');

const server = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks).toString();
    console.log('req', req.method, req.url, req.httpVersion, req.headers['x-a'], JSON.stringify(req.headers['set-cookie']), JSON.stringify(body));
    if (req.url === '/chunked') {
      res.setHeader('X-Kind', 'chunked');
      res.write('hello ');
      res.end('world');
    } else {
      res.writeHead(201, { 'Content-Type': 'text/plain' });
      res.end('ok:' + body.length);
    }
  });
});
server.on('upgrade', (req, socket, head) => {
  console.log('upgrade', req.url, req.headers.upgrade, JSON.stringify(head.toString()));
  socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: test\r\nConnection: Upgrade\r\n\r\n');
  socket.on('data', (d) => { socket.end('echo:' + d); });
  if (head.length) socket.emit('data', head);
});

function exchange(port, payload) {
  return new Promise((resolve) => {
    const socket = net.connect(port, '127.0.0.1');
    let data = '';
    socket.on('data', (d) => { data += d; });
    socket.on('end', () => resolve(data));
    socket.on('connect', () => socket.write(payload));
  });
}

function show(label, text) {
  const lines = text.split('\r\n').filter((l) => !/^Date: /.test(l));
  console.log(label, JSON.stringify(lines.join('|')));
}

server.listen(0, '127.0.0.1', async () => {
  const port = server.address().port;
  console.log('listening', typeof port);
  show('post', await exchange(port,
    'POST /p?q=1 HTTP/1.1\r\nHost: x\r\nX-A: 1\r\nX-a: 2\r\nSet-Cookie: a\r\nSet-Cookie: b\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde'));
  show('chunked', await exchange(port,
    'POST /chunked HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n'));
  show('keepalive', await exchange(port,
    'GET /one HTTP/1.1\r\nHost: x\r\n\r\nGET /two HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n'));
  show('upgrade', await exchange(port,
    'GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: test\r\nConnection: Upgrade\r\n\r\nping'));
  server.close(() => console.log('closed'));
});
