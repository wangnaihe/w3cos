const { RegExpParser, visitRegExpAST } = require("vscode-regexpp");
const ast = new RegExpParser().parseLiteral("/(?<year>\\d{4})-[a-z]+|x*?/giu");
const types = [];
visitRegExpAST(ast, { onCharacterClassEnter(n) { types.push("class:" + n.raw); }, onGroupEnter(n) { types.push("group"); }, onCapturingGroupEnter(n) { types.push("cap:" + n.name); }, onQuantifierEnter(n) { types.push("q:" + n.min + "," + n.max + "," + n.greedy); } });
console.log(ast.flags.raw, ast.pattern.alternatives.length, types.join(" "));
