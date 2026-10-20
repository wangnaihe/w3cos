'use strict';
// `node:sqlite`: not wired to the runtime's SQLite yet (M3 item 23); the
// sqlite3 package replacement (`@vscode/sqlite3`) is.

const { unsupported } = require('node:internal/w3cos_unsupported');

class DatabaseSync {
  constructor() { throw unsupported('node:sqlite DatabaseSync'); }
}
class StatementSync {
  constructor() { throw unsupported('node:sqlite StatementSync'); }
}

module.exports = { DatabaseSync, StatementSync, constants: {} };
