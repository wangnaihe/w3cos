// node-pty: the program runs on a pseudo-terminal of the requested size,
// output and echoed keystrokes arrive through onData, onExit reports the code.
const pty = require('node-pty');
console.log('api', typeof pty.spawn);
const term = pty.spawn('/bin/sh', ['-c', 'stty size; echo ready; read line; echo got:$line; exit 3'], {
  name: 'xterm-256color', cols: 100, rows: 30, cwd: '/', env: { PATH: process.env.PATH, FOO: 'bar' },
});
console.log('pid', typeof term.pid, term.pid > 0, term.process, term.cols, term.rows);
let out = '';
let sent = false;
term.onData((data) => {
  out += data;
  if (!sent && out.includes('ready')) {
    sent = true;
    term.write('abc\n');
  }
});
term.onExit((event) => {
  const text = out.replace(/\r/g, '');
  console.log('size', /(^|\n)30 100\n/.test(text));
  console.log('echo', text.includes('abc\n'), 'got', text.includes('got:abc\n'));
  console.log('exit', event.exitCode);
  const second = pty.spawn('/bin/sh', ['-c', 'sleep 0.3; stty size'], { cols: 80, rows: 24 });
  let two = '';
  second.onData((data) => { two += data; });
  second.resize(120, 40);
  console.log('resized', second.cols, second.rows);
  second.onExit((e) => {
    console.log('second', two.trim(), e.exitCode);
    const third = pty.spawn('/bin/sh', ['-c', 'sleep 30'], {});
    third.onExit((e3) => {
      console.log('killed', e3.exitCode, e3.signal);
      try { pty.spawn('/definitely/missing', [], {}); } catch (error) { console.log('missing', error.code); }
      console.log('done');
    });
    third.kill('SIGTERM');
  });
});
