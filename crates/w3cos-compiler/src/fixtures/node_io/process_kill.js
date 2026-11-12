'use strict';
const { spawn } = require('child_process');

console.log('self', process.kill(process.pid, 0));
try {
  process.kill(2147483646, 0);
} catch (error) {
  console.log('missing', error.code, error.errno, error.syscall, error.message);
}
for (const bad of ['abc', 1.5, null]) {
  try {
    process.kill(bad, 0);
  } catch (error) {
    console.log('pid', error.code, error instanceof TypeError);
  }
}
try {
  process.kill(process.pid, 'SIGNOPE');
} catch (error) {
  console.log('signal', error.code, error instanceof TypeError);
}
const child = spawn('sleep', ['30']);
child.on('exit', (code, signal) => {
  console.log('child exit', code, signal);
  let alive = true;
  try {
    process.kill(child.pid, 0);
  } catch (error) {
    alive = error.code !== 'ESRCH';
  }
  console.log('reaped', !alive);
  console.log('done');
});
console.log('child alive', process.kill(child.pid, 0));
console.log('child term', process.kill(String(child.pid), 'SIGTERM'));
