'use strict';
const net = require('net');
const fs = require('fs');
const os = require('os');
const path = require('path');

const sock = path.join(os.tmpdir(), 'w3cos-net-e2e-' + process.pid + '.sock');
try { fs.unlinkSync(sock); } catch (e) {}

function uds(next) {
  const order = [];
  const server = net.createServer((conn) => {
    order.push('connection');
    conn.setEncoding('utf8');
    let got = '';
    conn.on('data', (d) => { got += d; });
    conn.on('end', () => {
      order.push('server-end:' + got);
      conn.end('echo:' + got.toUpperCase());
    });
    conn.on('close', (hadError) => order.push('server-close:' + hadError));
  });
  server.on('listening', () => order.push('listening'));
  server.listen(sock, () => {
    console.log('uds listening', server.address() === sock, fs.existsSync(sock), server.listening);
    const client = net.connect(sock);
    console.log('uds connecting', client.connecting);
    client.on('connect', () => {
      order.push('client-connect');
      client.write('hello ');
      client.end('uds');
    });
    let reply = '';
    client.on('data', (d) => { reply += d; });
    client.on('end', () => order.push('client-end'));
    client.on('close', (hadError) => {
      order.push('client-close:' + hadError);
      console.log('uds reply', reply, client.bytesWritten, client.bytesRead);
      server.close(() => {
        console.log('uds order', order.join(','));
        console.log('uds closed', fs.existsSync(sock), server.listening);
        next();
      });
    });
  });
}

function tcp(next) {
  let closed = 0;
  const finish = () => {
    if (++closed < 2) return;
    server.getConnections((err, count) => {
      console.log('tcp connections', err, count);
      server.close();
      server.on('close', () => { console.log('tcp server closed'); next(); });
    });
  };
  const server = net.createServer({ allowHalfOpen: false }, (conn) => {
    conn.on('data', (d) => {
      conn.write(String(Number(d.toString()) * 2));
      conn.end();
    });
    conn.on('close', finish);
  });
  server.listen(0, '127.0.0.1', () => {
    const address = server.address();
    console.log('tcp address', address.address, address.family, typeof address.port, address.port > 0);
    const client = net.createConnection({ port: address.port, host: '127.0.0.1' }, () => {
      console.log('tcp connected', client.remoteAddress, client.remotePort === address.port, client.remoteFamily, typeof client.localPort);
      client.write('21');
    });
    client.setEncoding('utf8');
    client.on('data', (d) => console.log('tcp data', d));
    client.on('close', finish);
  });
}

function errors(next) {
  const missing = net.connect(path.join(os.tmpdir(), 'w3cos-net-missing.sock'));
  missing.on('error', (e) => {
    console.log('err missing', e.code, e.syscall, e.message.replace(os.tmpdir(), '<tmp>'));
    const s1 = net.createServer().listen(0, '127.0.0.1', () => {
      const port = s1.address().port;
      const s2 = net.createServer();
      s2.on('error', (e2) => {
        console.log('err inuse', e2.code, e2.syscall, e2.port === port, e2.message.replace(String(port), '<port>'));
        s1.close(() => {
          const refused = net.connect(port, '127.0.0.1');
          refused.on('error', (e3) => console.log('err refused', e3.code, e3.syscall, e3.message.replace(String(port), '<port>')));
          refused.on('connect', () => { console.log('connected to a closed server'); refused.destroy(); });
          refused.on('close', () => next());
        });
      });
      s2.listen(port, '127.0.0.1');
    });
  });
}

console.log('isIP', net.isIP('1.2.3.4'), net.isIP('::1'), net.isIP('x'), net.isIPv4('1.2.3.4'), net.isIPv6('::1'));
uds(() => tcp(() => errors(() => console.log('done'))));
