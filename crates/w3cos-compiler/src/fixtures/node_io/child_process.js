'use strict';
const cp = require('child_process');
const util = require('util');
const steps = [];
function log(...a) { console.log(...a); }

function t1() {
  const child = cp.spawn('sh', ['-c', 'echo out1; echo err1 >&2; exit 3']);
  log('spawn pid', typeof child.pid, child.spawnfile, JSON.stringify(child.spawnargs), child.exitCode, child.killed);
  let out = '', err = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (d) => { out += d; });
  child.stderr.on('data', (d) => { err += d.toString(); });
  const order = [];
  child.stdout.on('end', () => order.push('stdout-end'));
  child.on('exit', (code, signal) => order.push('exit:' + code + ':' + signal));
  child.on('close', (code, signal) => {
    order.push('close:' + code + ':' + signal);
    log('t1', JSON.stringify(out), JSON.stringify(err), child.exitCode, child.signalCode);
    log('t1 order', order.filter((x) => x !== 'stdout-end').join(','), order.includes('stdout-end'));
    t2();
  });
}

function t2() {
  const child = cp.spawn('cat');
  const chunks = [];
  child.stdout.on('data', (d) => chunks.push(d));
  child.on('close', (code) => {
    log('t2 cat', code, JSON.stringify(Buffer.concat(chunks).toString()));
    t3();
  });
  child.stdin.write('hello ');
  child.stdin.write(Buffer.from('world'));
  child.stdin.end('!\n');
}

function t3() {
  const child = cp.spawn('definitely-not-a-command-w3cos', ['x']);
  child.on('error', (e) => log('t3 error', e.code, e.errno, e.syscall, e.path, JSON.stringify(e.spawnargs), e.message));
  child.on('close', (code, signal) => { log('t3 close', code, signal, typeof child.pid); t4(); });
}

function t4() {
  const child = cp.spawn('sleep', ['5']);
  child.on('exit', (code, signal) => {
    log('t4 exit', code, signal, child.killed, child.signalCode);
    t5();
  });
  setTimeout(() => log('t4 kill', child.kill()), 20);
}

function t5() {
  cp.exec('echo $W3COS_T | tr a-z A-Z; pwd', { env: Object.assign({}, process.env, { W3COS_T: 'shout' }), cwd: '/tmp' }, (err, stdout, stderr) => {
    log('t5 exec', err, JSON.stringify(stdout), JSON.stringify(stderr));
    cp.exec('echo bad >&2; exit 7', (err2, so, se) => {
      log('t5 fail', err2.code, err2.killed, err2.signal, JSON.stringify(err2.cmd), JSON.stringify(err2.message), JSON.stringify(se));
      t6();
    });
  });
}

function t6() {
  cp.execFile('printf', ['%s-%s', 'a', 'b'], (err, stdout) => {
    log('t6 execFile', err, stdout);
    util.promisify(cp.exec)('echo promised').then((r) => {
      log('t6 promisify', JSON.stringify(r.stdout), JSON.stringify(r.stderr));
      t7();
    });
  });
}

function t7() {
  const r = cp.spawnSync('sh', ['-c', 'cat; echo e >&2; exit 2'], { input: 'in-data' });
  log('t7 spawnSync', r.status, r.signal, r.stdout.toString(), JSON.stringify(r.stderr.toString()), r.output.length, r.output[0], typeof r.pid);
  const r2 = cp.spawnSync('echo', ['enc'], { encoding: 'utf8' });
  log('t7 enc', JSON.stringify(r2.stdout), typeof r2.stdout);
  const r3 = cp.spawnSync('nope-w3cos-cmd');
  log('t7 enoent', r3.error.code, r3.status, r3.pid, r3.output, r3.stdout, r3.error.message);
  log('t7 execSync', JSON.stringify(cp.execSync('echo sync').toString()), cp.execSync('echo s2', { encoding: 'utf8' }).trim());
  try { cp.execSync('exit 4', { stdio: 'pipe' }); } catch (e) { log('t7 execSync fail', e.status, e.signal, JSON.stringify(e.message), e.stdout.length); }
  log('t7 execFileSync', cp.execFileSync('printf', ['x%sy', 1], { encoding: 'utf8' }));
  const r4 = cp.spawnSync('sleep', ['5'], { timeout: 50 });
  log('t7 timeout', r4.error && r4.error.code, r4.signal, r4.status);
  log('done');
}

t1();
