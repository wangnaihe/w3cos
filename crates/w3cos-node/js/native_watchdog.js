'use strict';
// `@vscode/native-watchdog` (M3 item 20). The native addon watches the
// parent pid from a separate thread and exits the extension host when the
// parent is gone, even while the event loop is busy. w3cos keeps the
// package's argument checks and `exit`; the parent-liveness check itself is
// left to the timer Code-OSS's extension host already runs next to it
// (extensionHostProcess.ts), so `start` does not spawn a monitor thread.

let hasStarted = false;

function start(pid) {
  if (typeof pid !== 'number' || Math.round(pid) !== pid) {
    throw new Error('Expected integer pid!');
  }
  if (hasStarted) {
    throw new Error('Can only monitor a single process!');
  }
  hasStarted = true;
}

function exit(code) {
  process.exit(code || 0);
}

module.exports = { start, exit };
