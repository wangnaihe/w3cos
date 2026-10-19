// The @vscode/spdlog API VS Code's file logger uses
// (src/vs/platform/log/node/spdlogLog.ts).
const spdlog = require('@vscode/spdlog');
const fs = require('fs');
const os = require('os');
const path = require('path');

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'w3cos-spdlog-'));
const stamp = /\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}/;
const show = (file) => fs.readFileSync(file, 'utf8').split('\n').filter(Boolean)
  .map((line) => line.replace(stamp, '<time>'));
const files = () => fs.readdirSync(path.join(dir, 'logs')).sort().join(',');

console.log('api', typeof spdlog.version, typeof spdlog.setLevel, typeof spdlog.setFlushOn, typeof spdlog.createAsyncRotatingLogger);
spdlog.createRotatingLogger('main', path.join(dir, 'logs', 'main.log'), 200, 2).then((logger) => {
  console.log('level', logger.getLevel());
  logger.trace('hidden trace');
  logger.info('first info');
  logger.warn('a warning');
  logger.setLevel(0);
  logger.trace('now traced');
  logger.debug('debug line');
  logger.error('an error');
  logger.critical('critical!');
  logger.flush();
  console.log('default pattern');
  for (const line of show(path.join(dir, 'logs', 'main.log'))) console.log(' ', line);
  logger.setPattern('%Y-%m-%d %H:%M:%S.%e [%l] %v');
  logger.info('vscode pattern');
  logger.setPattern('[%n] %L | %v %%');
  logger.warn('short level');
  logger.flush();
  console.log('patterns');
  for (const line of show(path.join(dir, 'logs', 'main.log')).slice(-2)) console.log(' ', line);
  console.log('rotate before', files(), show(path.join(dir, 'logs', 'main.1.log')).join(' / '));
  for (let i = 0; i < 12; i++) logger.info('rotation filler line ' + i);
  logger.flush();
  console.log('rotate after', files());
  console.log('newest', show(path.join(dir, 'logs', 'main.log')).join(' / '));
  console.log('older', show(path.join(dir, 'logs', 'main.1.log')).join(' / '));
  console.log('oldest', show(path.join(dir, 'logs', 'main.2.log')).join(' / '));
  logger.setLevel(6);
  logger.critical('off');
  logger.flush();
  console.log('off', show(path.join(dir, 'logs', 'main.log')).some((l) => l.includes('off')));
  logger.drop();
  return spdlog.createAsyncRotatingLogger('async', path.join(dir, 'logs', 'async.log'), 1024 * 1024, 1);
}).then((logger) => {
  logger.setPattern('%l %v');
  logger.info('async info');
  logger.flush();
  console.log('async', show(path.join(dir, 'logs', 'async.log')).join(' / '));
  logger.drop();
  fs.rmSync(dir, { recursive: true, force: true });
  console.log('done');
});
