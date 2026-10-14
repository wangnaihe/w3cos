export {};
const TOKEN="PR55_REVIEW_TOKEN";
function row(name,run){const selected=window.W3COS_REVIEW_CASE;if(selected&&selected!==name)return;let actual;try{actual={value:run()};}catch(e){actual={exception:e.name||typeof e,token:e===TOKEN};}console.log("W3COS_PR55_REVIEW "+JSON.stringify({name:name,actual:actual}));}
function fresh(){const e=document.createElement("div");document.body.appendChild(e);return e;}
function appendOwn(){const e=fresh(),c=document.createElement("b");let n=0;e.appendChild=function(x){n++;return x;};const r=e.appendChild(c);return [n,e.childNodes.length,r===c];}
function appendPrototype(){const e=fresh(),c=document.createElement("b"),p=Node.prototype,old=Object.getOwnPropertyDescriptor(p,"appendChild");let n=0;p.appendChild=function(x){n++;return x;};const r=e.appendChild(c);Object.defineProperty(p,"appendChild",old);return [n,e.childNodes.length,r===c];}
function appendGetter(){const e=fresh(),c=document.createElement("b");let reads=0,calls=0;Object.defineProperty(e,"appendChild",{configurable:true,get:function(){reads++;return function(x){calls++;return x;};}});const r=e.appendChild(c);return [reads,calls,e.childNodes.length,r===c];}
function appendGetterThrows(){const e=fresh(),c=document.createElement("b");Object.defineProperty(e,"appendChild",{configurable:true,get:function(){throw TOKEN;}});e.appendChild(c);return "getter-was-skipped";}
function appendMethodThrows(){const e=fresh(),c=document.createElement("b");e.appendChild=function(){throw TOKEN;};e.appendChild(c);return "method-was-skipped";}
function createOwn(){const old=document.createElement;let n=0;document.createElement=function(){n++;return "custom";};const r=document.createElement("div");document.createElement=old;return [n,typeof r];}
function createGetter(){const old=document.createElement;let reads=0,calls=0;Object.defineProperty(document,"createElement",{configurable:true,get:function(){reads++;return function(){calls++;return "custom";};}});const r=document.createElement("div");Object.defineProperty(document,"createElement",{value:old,writable:true,configurable:true});return [reads,calls,typeof r];}
function createMethodThrows(){const old=document.createElement;document.createElement=function(){throw TOKEN;};function invoke(){return document.createElement("div");}let caught=false;try{invoke();}catch(e){caught=e===TOKEN;}document.createElement=old;return caught;}
function textOwn(){const old=document.createTextNode;let n=0;document.createTextNode=function(){n++;return "custom";};const r=document.createTextNode("text");document.createTextNode=old;return [n,typeof r];}
function styleGetter(){const e=fresh(),shadow={width:"initial"};let n=0;Object.defineProperty(e,"style",{configurable:true,get:function(){n++;return shadow;}});e.style.width="31px";return [n,shadow.width];}
function styleGetterThrows(){const e=fresh();Object.defineProperty(e,"style",{configurable:true,get:function(){throw TOKEN;}});e.style.width="31px";return "getter-was-skipped";}
function styleGetterOrder(){const e=fresh(),shadow={width:"initial"},events=[];Object.defineProperty(e,"style",{configurable:true,get:function(){events.push("getter");return shadow;}});e.style.width={toString:function(){events.push("coercion");return "31px";}};return [events.join(","),typeof shadow.width];}
function styleDeclarationSetter(){const e=fresh(),s=e.style;let n=0;Object.defineProperty(s,"width",{configurable:true,set:function(v){n++;}});s.width="37px";return n;}
function idSetter(){const e=fresh();let n=0;Object.defineProperty(e,"id",{configurable:true,set:function(v){n++;}});e.id="custom";return n;}
function nonDomStyleGetter(){let n=0;const shadow={width:"initial"},o={};Object.defineProperty(o,"style",{configurable:true,get:function(){n++;return shadow;}});o.style.width="31px";return [n,shadow.width];}
function materializeIdentity(){const e=fresh();e.id="pr55-review-id";const before=document.getElementById("pr55-review-id")===e;const p=Object.getPrototypeOf(e);return [before,p===HTMLDivElement.prototype,document.getElementById("pr55-review-id")===e];}
function defineOwnData(){const e=fresh();Object.defineProperty(e,"reviewOwn",{value:7,writable:true,configurable:true});const before=e.reviewOwn;Object.getPrototypeOf(e);return [before,e.reviewOwn];}
function defineOwnGetter(){const e=fresh();let n=0;Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){n++;return 7;}});const before=e.reviewOwn;Object.getPrototypeOf(e);return [before,e.reviewOwn,n];}
function changedPrototype(){const e=fresh();Object.setPrototypeOf(e,{reviewOwn:9});return [e.reviewOwn,Object.getPrototypeOf(e).reviewOwn];}
function prototypeMethodBeforeAfter(){const e=fresh(),p=Element.prototype,old=Object.getOwnPropertyDescriptor(p,"matches");p.matches=function(){return "override";};const before=e.matches("*");Object.getPrototypeOf(e);const after=e.matches("*");Object.defineProperty(p,"matches",old);return [before,after];}
function prototypeGetterBeforeAfter(){const e=fresh(),p=Element.prototype;let n=0;Object.defineProperty(p,"reviewRead",{configurable:true,get:function(){n++;return this===e;}});const before=e.reviewRead;Object.getPrototypeOf(e);const after=e.reviewRead;delete p.reviewRead;return [before,after,n];}
function prototypeGetterThrows(){const e=fresh(),p=Element.prototype;Object.defineProperty(p,"reviewThrow",{configurable:true,get:function(){throw TOKEN;}});function invoke(){return e.reviewThrow;}let caught=false;try{invoke();}catch(x){caught=x===TOKEN;}delete p.reviewThrow;return caught;}
function expandoAssignment(){const e=fresh();e.reviewExpando=11;const before=e.reviewExpando;Object.getPrototypeOf(e);return [before,e.reviewExpando];}
function ownDelete(){const e=fresh();e.reviewExpando=11;const ok=delete e.reviewExpando;return [ok,typeof e.reviewExpando];}
function ownGetterThis(){const e=fresh();Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){return this===e;}});return e.reviewOwn;}
function ownGetterMutation(){const e=fresh();Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){this.reviewSide=5;return this.reviewSide;}});return [e.reviewOwn,e.reviewSide];}
function matchesIdentity(){const e=fresh();return e.matches===e.matches;}
function matchesBorrowed(){const a=fresh(),b=fresh();a.className="aa";b.className="bb";const m=a.matches;return m.call(b,".bb");}
function matchesInvalid(){const e=fresh();return e.matches.call({},"*");}
function plainProxyGetter(){let n=0;const target={value:3},o=new Proxy(target,{get:function(t,k,r){n++;return t[k];}});return [o.value,n];}
function getterUndefinedOnce(){const e=fresh();let n=0;Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){n++;return undefined;}});const v=e.reviewOwn;return [typeof v,n];}
function getterRedefinesReceiver(){const e=fresh();Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){Object.defineProperty(this,"reviewSide",{value:5,configurable:true});return this===e;}});return [e.reviewOwn,e.reviewSide];}
function getterChangesPrototype(){const e=fresh();Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){Object.setPrototypeOf(this,{reviewPrototype:9});return this===e;}});const v=e.reviewOwn;return [v,e.reviewPrototype,Object.getPrototypeOf(e).reviewPrototype];}
function methodGetterReceiver(){const e=fresh(),c=document.createElement("b");let getterThis=false,methodThis=false;Object.defineProperty(e,"appendChild",{configurable:true,get:function(){getterThis=this===e;return function(x){methodThis=this===e;return x;};}});const r=e.appendChild(c);return [getterThis,methodThis,r===c,e.childNodes.length];}
function getterExceptionSideEffect(){const e=fresh();Object.defineProperty(e,"reviewOwn",{configurable:true,get:function(){Object.defineProperty(this,"reviewSide",{value:5});throw TOKEN;}});let caught=false;try{e.reviewOwn;}catch(x){caught=x===TOKEN;}return [caught,e.reviewSide];}
function proxyUndefinedGetter(){let n=0;const target={};Object.defineProperty(target,"probe",{get:function(){n++;return 9;}});const o=new Proxy(target,{get:function(){return undefined;}});const v=o.probe;return [typeof v,n];}
function proxyRedefinesReceiver(){const o=new Proxy({}, {get:function(t,k,r){if(k==="probe"){Object.defineProperty(r,"side",{value:7});return 3;}return t[k];}});return [o.probe,o.side];}
function styleRhsOrder(){const e=fresh(),shadow={width:"initial"},events=[];Object.defineProperty(e,"style",{get:function(){events.push("getter");return shadow;}});function rhs(){events.push("rhs");return {toString:function(){events.push("coercion");return "31px";}};}e.style.width=rhs();return [events.join(","),typeof shadow.width];}
function documentGetterReceiver(){let same=false;Object.defineProperty(document,"createElement",{configurable:true,get:function(){same=this===document;return function(){return this===document;};}});return [document.createElement("div"),same];}
row("append-own-override",appendOwn);
row("append-prototype-override",appendPrototype);
row("append-own-getter",appendGetter);
row("append-getter-exception",appendGetterThrows);
row("append-method-exception",appendMethodThrows);
row("createElement-own-override",createOwn);
row("createElement-own-getter",createGetter);
row("createElement-method-exception",createMethodThrows);
row("createTextNode-own-override",textOwn);
row("style-own-getter",styleGetter);
row("style-getter-exception",styleGetterThrows);
row("style-getter-coercion-order",styleGetterOrder);
row("style-declaration-user-setter",styleDeclarationSetter);
row("element-id-user-setter",idSetter);
row("non-DOM-style-getter",nonDomStyleGetter);
row("materialize-identity",materializeIdentity);
row("materialize-define-own-data",defineOwnData);
row("materialize-define-own-getter",defineOwnGetter);
row("materialize-setPrototypeOf",changedPrototype);
row("prototype-method-before-after",prototypeMethodBeforeAfter);
row("prototype-getter-before-after",prototypeGetterBeforeAfter);
row("prototype-getter-exception",prototypeGetterThrows);
row("expando-assignment-before-after",expandoAssignment);
row("expando-delete",ownDelete);
row("own-getter-actual-this",ownGetterThis);
row("own-getter-side-effect",ownGetterMutation);
row("matches-identity",matchesIdentity);
row("matches-borrowed-this",matchesBorrowed);
row("matches-illegal-receiver",matchesInvalid);
row("ordinary-JS-Proxy",plainProxyGetter);
row("boundary-getter-undefined-once",getterUndefinedOnce);
row("boundary-getter-redefines-receiver",getterRedefinesReceiver);
row("boundary-getter-changes-prototype",getterChangesPrototype);
row("boundary-method-getter-actual-this",methodGetterReceiver);
row("boundary-getter-exception-side-effect",getterExceptionSideEffect);
row("boundary-Proxy-undefined-authoritative",proxyUndefinedGetter);
row("boundary-Proxy-redefines-receiver",proxyRedefinesReceiver);
row("boundary-style-RHS-order",styleRhsOrder);
row("boundary-document-getter-this",documentGetterReceiver);
function defineRefusal(){const p=new Proxy({}, {defineProperty:function(){return false;}});Object.defineProperty(p,"x",{value:1});return "returned";}
function defineNestedRefusal(){const inner=new Proxy({}, {defineProperty:function(){return false;}}),outer=new Proxy(inner,{});Object.defineProperty(outer,"x",{value:1});return "returned";}
row("boundary-defineProperty-false",defineRefusal);
row("boundary-defineProperty-nested-false",defineNestedRefusal);
function functionPrototypeIdentity(){function F(){} function G(){} const a=F.prototype;return [typeof a,a===F.prototype,a!==G.prototype];}
function functionPrototypeMutation(){function F(){} const p=F.prototype;p.probe=9;const o=new F();return [o.probe,Object.getPrototypeOf(o)===p,o instanceof F];}
function functionPrototypeOverwrite(){function F(){} const p={probe:12};F.prototype=p;const o=new F();return [F.prototype===p,o.probe,Object.getPrototypeOf(o)===p];}
function functionPrototypeUndefined(){function F(){} F.prototype=undefined;return [typeof F.prototype,Object.prototype.hasOwnProperty.call(F,"prototype")];}
function functionPrototypePropertyOrder(){function F(){} F.extra=3;return [Object.prototype.hasOwnProperty.call(F,"prototype"),Object.getOwnPropertyNames(F).indexOf("prototype")<Object.getOwnPropertyNames(F).indexOf("extra")];}
function functionCallIdentity(){function F(){return this.probe;}const receiver={probe:7};return [F===F,F.call(receiver),typeof F.prototype];}
row("boundary-function-prototype-identity",functionPrototypeIdentity);
row("boundary-function-prototype-mutation-construction",functionPrototypeMutation);
row("boundary-function-prototype-overwrite",functionPrototypeOverwrite);
row("boundary-function-prototype-undefined",functionPrototypeUndefined);
row("boundary-function-prototype-order",functionPrototypePropertyOrder);
row("boundary-function-call-receiver",functionCallIdentity);
function cellSharedEscape(){function make(p){return {read:function(){return p;},write:function(v){p=v;}};}const o=make(2);o.write(9);return o.read();}
function cellRecursion(){function sum(n){const own=n;return n===0?own:own+sum(n-1);}return sum(4);}
function cellReentrant(){let nested;function run(p,hook){const read=function(){return p;};hook();return read();}const out=run(2,function(){nested=run(9,function(){});});return [out,nested];}
function cellThis(){const o={mark:1,run:function(p){const same=this===o;this.mark+=p;return [same,this.mark];}};return o.run(2);}
function cellLexicalThis(){const o={mark:4,run:function(p){return ()=>this.mark+p;}};const read=o.run(3);return read.call({mark:99});}
function cellTDZ(){function run(p){try{return later;}catch(e){return e.name;}let later=p;}return run(4);}
function cellConstCapture(){function run(p){const stable=p;return function(){return stable;};}return run(6)();}
function cellDefaultOrder(){const events=[];function mark(s,v){events.push(s);return v;}function run(a=mark("a",2),b=mark("b",a+1)){return [a,b];}const values=run();return [events.join(","),values];}
function cellDefaultThrow(){const events=[];function mark(s){events.push(s);if(s==="a")throw TOKEN;return 3;}function run(a=mark("a"),b=mark("b")){return b;}let caught=false;try{run();}catch(e){caught=e===TOKEN;}return [events.join(","),caught];}
function cellDefaultTDZ(){function run(a=b,b=2){return a;}return run();}
function cellSuper(){class Base{constructor(p){this.mark=p;}read(p){return this.mark+p;}}class Child extends Base{constructor(p){super(p+1);this.extra=3;}read(p){return super.read(p)+this.extra;}}const c=new Child(4);return [c.mark,c.read(2)];}
function cellArgumentOrderException(){const events=[];function mark(p){events.push(p);if(p===2)throw TOKEN;return p;}function run(a,b,c){return a+b+c;}let caught=false;try{run(mark(1),mark(2),mark(3));}catch(e){caught=e===TOKEN;}return [events.join(","),caught];}
function cellRest(){function run(a,...rest){return [a,rest.join(",")];}return run(2,4,6);}
function cellArguments(){function run(a){a=9;return arguments[0];}return run(2);}
function cellGenerator(){function* run(p){yield p;p++;return p;}const it=run(4);return [it.next().value,it.next().value];}
function cellThrowShared(){let read;function run(p){read=function(){return p;};p=9;throw TOKEN;}let caught=false;try{run(2);}catch(e){caught=e===TOKEN;}return [caught,read()];}
row("entry-cell-shared-parameter-escape",cellSharedEscape);
row("entry-cell-recursion-independent",cellRecursion);
row("entry-cell-reentrancy",cellReentrant);
row("entry-cell-this-receiver",cellThis);
row("entry-cell-lexical-this",cellLexicalThis);
row("entry-cell-lexical-TDZ",cellTDZ);
row("entry-cell-const-capture",cellConstCapture);
row("entry-cell-default-order",cellDefaultOrder);
row("entry-cell-default-exception",cellDefaultThrow);
row("entry-cell-default-later-TDZ",cellDefaultTDZ);
row("entry-cell-this-super",cellSuper);
row("entry-cell-argument-order-exception",cellArgumentOrderException);
row("entry-cell-rest-owned",cellRest);
row("entry-cell-arguments-owned",cellArguments);
row("entry-cell-generator-owned",cellGenerator);
row("entry-cell-exception-shared-cell",cellThrowShared);
const trace=[];
async function fulfilled(){trace.push("before");await Promise.resolve(1);trace.push("after");}
const done=fulfilled();trace.push("caller");
done.then(function(){row("fulfilled-await-order",function(){return trace.join(",");});console.log("W3COS_PR55_REVIEW_DONE");});
