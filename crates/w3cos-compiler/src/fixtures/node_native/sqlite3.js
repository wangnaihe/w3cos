// The subset of @vscode/sqlite3 VS Code's storage layer uses
// (src/vs/base/parts/storage/node/storage.ts), plus common node-sqlite3 calls.
const sqlite3 = require('@vscode/sqlite3');
const fs = require('fs');
const os = require('os');
const path = require('path');

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'w3cos-sqlite-'));
const file = path.join(dir, 'state.vscdb');
const log = (...args) => console.log(...args);

const db = new sqlite3.Database(file, (error) => {
  log('open', error, typeof db.exec, db.open);
  db.exec('PRAGMA user_version = 1; CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)', (error) => {
    log('exec', error);
    db.serialize(() => {
      db.exec('BEGIN TRANSACTION');
      const stmt = db.prepare('INSERT INTO ItemTable VALUES (?,?),(?,?)');
      stmt.run(['a', 'one', 'b', 'two']);
      stmt.finalize((error) => log('finalize', error));
      db.run('INSERT INTO ItemTable VALUES (?, ?)', 'c', Buffer.from([1, 2, 3]), function (error) {
        log('run', error, this.lastID, this.changes);
      });
      db.run('UPDATE ItemTable SET value = $v WHERE key = $k', { $v: 'uno', $k: 'a' }, function (error) {
        log('update', error, this.changes);
      });
      db.exec('END TRANSACTION', (error) => log('commit', error));
      db.all('SELECT * FROM ItemTable ORDER BY key', (error, rows) => {
        log('all', error, JSON.stringify(rows.map((r) => [r.key, Buffer.isBuffer(r.value) ? [...r.value] : r.value])));
      });
      db.get('PRAGMA integrity_check', (error, row) => log('integrity', error, JSON.stringify(row)));
      db.get('SELECT count(*) AS n, 1.5 AS f, NULL AS z FROM ItemTable', (error, row) => log('get', error, JSON.stringify(row)));
      db.get('SELECT * FROM ItemTable WHERE key = ?', ['missing'], (error, row) => log('get missing', error, row));
      db.all('SELECT * FROM Nope', (error, rows) => log('error', error && error.message, error && error.code, error && error.errno, rows));
      db.run('INSERT INTO ItemTable VALUES (1, 2, 3)', (error) => log('bad insert', error && error.code));
      db.each('SELECT key FROM ItemTable ORDER BY key', (error, row) => log('each row', row.key), (error, n) => log('each done', error, n));
      db.close((error) => {
        log('close', error);
        const again = new sqlite3.Database(file, sqlite3.OPEN_READONLY, (error) => {
          again.all('SELECT key FROM ItemTable ORDER BY key', (error, rows) => {
            log('reopen', error, rows.map((r) => r.key).join(','));
            again.run('DELETE FROM ItemTable', (error) => {
              log('readonly', error && error.code);
              again.close(() => {
                fs.rmSync(dir, { recursive: true, force: true });
                log('done');
              });
            });
          });
        });
      });
    });
  });
});
log('constants', sqlite3.OPEN_READONLY, sqlite3.OPEN_READWRITE, sqlite3.OPEN_CREATE, typeof sqlite3.verbose().Database);
db.on('open', () => log('event open'));
