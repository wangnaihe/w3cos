export {};
let result="no-error";
try { const x=1; x=2; } catch(e) { result=e.name; }
console.log("W3COS_PR55_CONST "+JSON.stringify(result));
