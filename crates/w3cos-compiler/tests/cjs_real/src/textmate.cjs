const tm = require("vscode-textmate");
const grammar = tm.parseRawGrammar(JSON.stringify({ scopeName: "source.demo", patterns: [{ match: "\\bdemo\\b", name: "keyword.demo" }] }), "demo.json");
console.log(grammar.scopeName, grammar.patterns.length, typeof tm.Registry, typeof tm.INITIAL);
