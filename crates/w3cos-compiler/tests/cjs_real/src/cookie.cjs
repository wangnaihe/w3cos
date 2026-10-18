const cookie = require("cookie");
console.log(JSON.stringify(cookie.parse("foo=bar; equation=E%3Dmc%5E2; asdf=%")));
console.log(cookie.serialize("foo", "bar baz", { httpOnly: true, maxAge: 60, path: "/", sameSite: "strict", secure: true }));
