export {};
const TOKEN="PR55_REVIEW_TOKEN";
function row(name,run){const selected=window.W3COS_REVIEW_CASE;if(selected&&selected!==name)return;let actual;try{actual={value:run()};}catch(e){actual={exception:e.name||typeof e,token:e===TOKEN};}console.log("W3COS_PR55_REVIEW "+JSON.stringify({name:name,actual:actual}));}
function fresh(){return document.createElement("div");}
function inheritedUndefined(){const e=fresh();let calls=0;Object.defineProperty(Element.prototype,"nodeType",{configurable:true,get:function(){calls++;return undefined;}});return[typeof e.nodeType,calls];}
function inheritedReceiverMutation(){const e=fresh();let calls=0;Object.defineProperty(Element.prototype,"probe",{configurable:true,get:function(){calls++;this.side=7;return this===e;}});return[e.probe,e.side,calls];}
function inheritedReentrantPrototype(){const e=fresh();let calls=0;Object.defineProperty(Element.prototype,"probe",{configurable:true,get:function(){calls++;Object.defineProperty(Element.prototype,"side",{value:9,configurable:true});return this===e;}});return[e.probe,e.side,calls];}
function inheritedSetterReceiver(){const e=fresh();let calls=0;Object.defineProperty(Element.prototype,"probe",{configurable:true,set:function(v){calls++;this.side=v;}});e.probe=8;return[e.side,calls];}
function authoredProxyUndefined(){const e=fresh();let calls=0;const proto=new Proxy({}, {get:function(t,k,r){calls++;if(k==="nodeType")return undefined;return t[k];}});Object.setPrototypeOf(e,proto);return[typeof e.nodeType,calls];}
function expandoShadowsPrototype(){const e=fresh();e.probe=7;Element.prototype.probe=9;return e.probe;}
function descriptorShadowsCachedStyle(){const e=fresh();const old=e.style;let calls=0;const custom={width:"custom"};Object.defineProperty(e,"style",{configurable:true,get:function(){calls++;return custom;}});return[e.style===custom,calls];}
function latePrototypeMethod(){const e=fresh(),child=fresh();e.appendChild(child);let calls=0;Node.prototype.appendChild=function(v){calls++;return v;};const child2=fresh();const result=e.appendChild(child2);return[calls,e.childNodes.length,result===child2];}
function freshClosureException(){const readers=[];let caught=false;try{for(let i=0;i<3;i++){readers.push(function(){return i;});if(i===1)throw TOKEN;}}catch(e){caught=e===TOKEN;}return[caught,readers[0](),readers[1]()];}
function sharedCaptureMutation(){let value=1;function read(){return value;}const out=[];for(var i=0;i<3;i++){value+=1;out.push(read());}return[out.join(","),read()];}
row("symmetric-inherited-undefined",inheritedUndefined);
row("symmetric-inherited-receiver-mutation",inheritedReceiverMutation);
row("symmetric-inherited-reentrant-prototype",inheritedReentrantPrototype);
row("symmetric-inherited-setter-receiver",inheritedSetterReceiver);
row("symmetric-authored-proxy-undefined",authoredProxyUndefined);
row("symmetric-expando-shadows-prototype",expandoShadowsPrototype);
row("symmetric-descriptor-shadows-style-cache",descriptorShadowsCachedStyle);
row("symmetric-late-prototype-method",latePrototypeMethod);
row("symmetric-fresh-capture-after-throw",freshClosureException);
row("symmetric-shared-capture-live-mutation",sharedCaptureMutation);
console.log("W3COS_PR55_REVIEW_DONE");
