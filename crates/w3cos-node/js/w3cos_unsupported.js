'use strict';
// Shared error for builtin APIs a W3C OS AOT app does not provide yet
// (M3 item 23). Modules load and expose Node's shapes; calling the missing
// operation throws this coded error instead of failing at link time.

function unsupported(api) {
  const error = new Error(api + ' is not supported by w3cos-node yet');
  error.code = 'ERR_W3COS_UNSUPPORTED';
  return error;
}

module.exports = { unsupported };
