const minimist = require("minimist");
console.log(JSON.stringify(minimist(["-a", "1", "--b=x", "--no-c", "pos", "-xyz", "--", "rest"])));
console.log(JSON.stringify(minimist(["--n", "5", "--s", "007"], { string: ["s"], boolean: ["t"], alias: { n: "num" }, default: { t: true } })));
