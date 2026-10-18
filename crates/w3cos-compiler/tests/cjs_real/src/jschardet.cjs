const jschardet = require("jschardet");
console.log(JSON.stringify(jschardet.detect("\xc3\xa0\xc3\xad\xc3\xa0\xc3\xa7\xc3\xa3 some utf8 text \xc3\xa9")));
console.log(JSON.stringify(jschardet.detect("plain ascii text")));
