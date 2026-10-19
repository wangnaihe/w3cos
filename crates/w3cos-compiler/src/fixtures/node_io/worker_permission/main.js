'use strict';
const path = require('path');
const { Worker } = require('worker_threads');
console.log('has worker', process.permission.has('worker'));
try {
  new Worker(path.join(__dirname, 'worker.js'));
  console.log('started');
} catch (e) {
  console.log('denied', e.code, e.permission, e.message);
}
console.log('done');
