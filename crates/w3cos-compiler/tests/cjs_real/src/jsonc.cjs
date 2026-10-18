const jsonc = require("jsonc-parser");
const errors = [];
const value = jsonc.parse('{ // comment\n "a": [1, 2, /* x */ 3,], "b": { "c": true }, }', errors, { allowTrailingComma: true });
console.log(JSON.stringify(value), errors.length);
const edits = jsonc.format('{"a":1,"b":[1,2]}', undefined, { tabSize: 2, insertSpaces: true });
console.log(jsonc.applyEdits('{"a":1,"b":[1,2]}', edits).replace(/\n/g, "\\n"));
const tree = jsonc.parseTree('{"x": {"y": [10, 20]}}');
console.log(jsonc.getNodeValue(jsonc.findNodeAtLocation(tree, ["x", "y", 1])));
console.log(JSON.stringify(jsonc.modify('{"a": 1}', ["b"], 2, { formattingOptions: { insertSpaces: true, tabSize: 2 } })));
