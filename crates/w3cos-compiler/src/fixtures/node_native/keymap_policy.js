// native-keymap and @vscode/policy-watcher as Code-OSS calls them on Linux
// (src/vs/platform/keyboardLayout, src/vs/platform/policy).
const keymap = require('native-keymap');
const map = keymap.getKeyMap();
const codes = Object.keys(map);
console.log('keys', codes.length, codes.slice(0, 8).join(','));
for (const code of ['KeyA', 'KeyZ', 'Digit2', 'Minus', 'BracketLeft', 'Quote', 'Backquote', 'IntlBackslash', 'Space', 'Enter', 'Numpad5', 'F12']) {
  console.log(code, JSON.stringify(map[code]));
}
let chars = '';
for (const code of codes) chars += map[code].value + map[code].withShift;
console.log('chars', chars.length, JSON.stringify(chars));
console.log('layout', JSON.stringify(keymap.getCurrentKeyboardLayout()));
console.log('iso', keymap.isISOKeyboard());
keymap.onDidChangeKeyboardLayout(() => console.log('changed'));

const policy = require('@vscode/policy-watcher');
try {
  policy.createWatcher('CodeOSS', { UpdateMode: { type: 'string' } }, (update) => console.log('update', update));
  console.log('watcher created');
} catch (error) {
  console.log('policy', error.constructor.name, error.message);
}
