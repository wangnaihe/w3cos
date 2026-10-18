const { URI, Utils } = require("vscode-uri");
const uri = URI.parse("https://code.visualstudio.com/docs/extensions/overview#frag");
console.log(uri.scheme, uri.authority, uri.path, uri.fragment);
console.log(URI.file("/home/user/a b.txt").toString());
console.log(Utils.joinPath(URI.parse("file:///root/dir"), "sub", "file.ts").toString(), Utils.basename(uri), Utils.extname(URI.file("/x/y.tar.gz")));
console.log(JSON.stringify(uri.toJSON()));
