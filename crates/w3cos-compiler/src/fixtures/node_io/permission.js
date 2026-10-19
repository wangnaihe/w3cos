'use strict';
const cp = require('child_process');
console.log('has child', process.permission.has('child'), 'has fs.read', process.permission.has('fs.read'));
for (const call of [() => cp.spawn('echo', ['x']), () => cp.execSync('echo x'), () => cp.spawnSync('echo')]) {
  try { call(); console.log('allowed'); } catch (e) { console.log('denied', e.code, e.permission, e.message); }
}
try { cp.exec('echo x', () => {}); } catch (e) { console.log('exec denied', e.code); }
console.log('done');
