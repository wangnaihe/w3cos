// The @parcel/watcher API Code-OSS's file watcher uses
// (src/vs/platform/files/node/watcher/parcel/parcelWatcher.ts).
const watcher = require('@parcel/watcher');
const fs = require('fs');
const os = require('os');
const path = require('path');

const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'w3cos-watch-')));
const rel = (p) => path.relative(root, p) || '.';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let batches = [];
const show = (label) => {
  const events = batches.flat().map((e) => e.type + ':' + rel(e.path)).sort();
  console.log(label, JSON.stringify(events));
  batches = [];
};

(async () => {
  console.log('api', typeof watcher.subscribe, typeof watcher.unsubscribe, typeof watcher.writeSnapshot, typeof watcher.getEventsSince);
  fs.mkdirSync(path.join(root, 'node_modules'));
  fs.mkdirSync(path.join(root, 'sub', 'deep'), { recursive: true });
  fs.writeFileSync(path.join(root, 'existing.txt'), 'x');
  const subscription = await watcher.subscribe(root, (error, events) => {
    if (error) console.log('error', error.message);
    batches.push(events);
  }, { ignore: [path.join(root, 'node_modules'), '**/*.log'] });
  console.log('subscribed', typeof subscription.unsubscribe);
  await sleep(300);
  fs.writeFileSync(path.join(root, 'a.txt'), 'hello');
  await sleep(600);
  show('create');
  fs.appendFileSync(path.join(root, 'a.txt'), ' world');
  await sleep(600);
  show('update');
  fs.writeFileSync(path.join(root, 'sub', 'deep', 'b.txt'), 'b');
  await sleep(600);
  show('nested');
  fs.writeFileSync(path.join(root, 'node_modules', 'ignored.js'), '');
  fs.writeFileSync(path.join(root, 'sub', 'trace.log'), '');
  await sleep(600);
  show('ignored');
  fs.renameSync(path.join(root, 'a.txt'), path.join(root, 'c.txt'));
  await sleep(600);
  show('rename');
  fs.unlinkSync(path.join(root, 'existing.txt'));
  await sleep(600);
  show('delete');
  await subscription.unsubscribe();
  fs.writeFileSync(path.join(root, 'after.txt'), '');
  await sleep(600);
  show('after unsubscribe');

  const snapshot = path.join(os.tmpdir(), 'w3cos-watch-snapshot-' + process.pid);
  const written = await watcher.writeSnapshot(root, snapshot);
  console.log('snapshot', typeof written, written === snapshot, fs.existsSync(snapshot));
  fs.writeFileSync(path.join(root, 'since.txt'), '');
  fs.unlinkSync(path.join(root, 'c.txt'));
  const since = await watcher.getEventsSince(root, snapshot);
  console.log('since', JSON.stringify(since.map((e) => e.type + ':' + rel(e.path)).sort()));
  fs.rmSync(snapshot, { force: true });
  fs.rmSync(root, { recursive: true, force: true });
  console.log('done');
})();
