// @vscode/native-watchdog as Code-OSS's extension host calls it
// (src/vs/workbench/api/node/extensionHostProcess.ts).
const watchdog = require('@vscode/native-watchdog');
console.log('api', typeof watchdog.start, typeof watchdog.exit);
for (const pid of ['1', 1.5, undefined]) {
  try {
    watchdog.start(pid);
    console.log('started', pid);
  } catch (err) {
    console.log('rejected', String(pid), err.message);
  }
}
watchdog.start(4242);
console.log('started parent');
try {
  watchdog.start(4242);
} catch (err) {
  console.log('again', err.message);
}
setTimeout(() => console.log('not reached'), 1000);
watchdog.exit(0);
