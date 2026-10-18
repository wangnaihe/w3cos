const iconv = require("@vscode/iconv-lite-umd");
console.log(iconv.encodingExists("utf8"), iconv.encodingExists("win1252"), iconv.encodingExists("nope"));
