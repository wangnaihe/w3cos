export {};
const TOKEN="PR55_REVIEW_TOKEN";
function row(name,run){const selected=window.W3COS_REVIEW_CASE;if(selected&&selected!==name)return;let actual;try{actual={value:run()};}catch(e){actual={exception:e.name||typeof e,token:e===TOKEN};}console.log("W3COS_PR55_REVIEW "+JSON.stringify({name:name,actual:actual}));}
function fusionFill(doc,parent,nodes,n){for(var i=0;i<n;i++){var el=doc.createElement("div");el.id="risk-"+i;el.className="row";el.textContent="item "+i;parent.appendChild(el);nodes.push(el);}return nodes.length;}
function pristine(){const root=document.createElement("div"),nodes=[];const count=fusionFill(document,root,nodes,2);return[count,root.childNodes.length,nodes[0].id,nodes[1].textContent];}
function createOwn(){const root=document.createElement("div"),nodes=[],original=document.createElement;let calls=0;document.createElement=function(tag){calls++;return original.call(this,tag);};fusionFill(document,root,nodes,2);return[calls,nodes.length,root.childNodes.length];}
function createGetterThrow(){const root=document.createElement("div"),nodes=[];Object.defineProperty(document,"createElement",{configurable:true,get:function(){throw TOKEN;}});fusionFill(document,root,nodes,2);return"getter-was-skipped";}
function appendOwn(){const root=document.createElement("div"),nodes=[];let calls=0;root.appendChild=function(child){calls++;return child;};fusionFill(document,root,nodes,2);return[calls,nodes.length,root.childNodes.length];}
function appendGetterThrow(){const root=document.createElement("div"),nodes=[];Object.defineProperty(root,"appendChild",{configurable:true,get:function(){throw TOKEN;}});fusionFill(document,root,nodes,2);return"getter-was-skipped";}
function prototypeIdSetter(){const root=document.createElement("div"),nodes=[];let calls=0;Object.defineProperty(Element.prototype,"id",{configurable:true,set:function(value){calls++;}});fusionFill(document,root,nodes,2);return[calls,nodes.length];}
function prototypeIdSetterThrow(){const root=document.createElement("div"),nodes=[];Object.defineProperty(Element.prototype,"id",{configurable:true,set:function(value){throw TOKEN;}});fusionFill(document,root,nodes,2);return"setter-was-skipped";}
function ownArrayPush(){const root=document.createElement("div"),nodes=[];let calls=0;nodes.push=function(value){calls++;return this.length;};fusionFill(document,root,nodes,2);return[calls,nodes.length,root.childNodes.length];}
function prototypeArrayPush(){const root=document.createElement("div"),nodes=[];let calls=0;Array.prototype.push=function(value){calls++;return this.length;};fusionFill(document,root,nodes,2);return[calls,nodes.length,root.childNodes.length];}
function zeroTripTDZ(){function fillZero(){for(var i=0;i<0;i++){var el=doc.createElement("div");el.id="risk-"+i;el.className="row";el.textContent="item "+i;parent.appendChild(el);nodes.push(el);}return"ok";}const result=fillZero();let doc=document;let parent=document.createElement("div");let nodes=[];return result;}
function zeroTripGetter(){const root=document.createElement("div"),nodes=[];let reads=0;Object.defineProperty(document,"createElement",{configurable:true,get:function(){reads++;throw TOKEN;}});const n=fusionFill(document,root,nodes,0);return[n,reads,root.childNodes.length];}
function freshCounterCaptures(){const readers=[];for(let i=0;i<3;i++){readers.push(function(){return i;});}return[readers[0](),readers[1](),readers[2]()];}
function freshBodyCaptures(){const readers=[];for(var i=0;i<3;i++){let value=i;readers.push(function(){return value;});}return[readers[0](),readers[1](),readers[2]()];}
function freshCaptureWrites(){const readers=[],writers=[];for(let i=0;i<3;i++){readers.push(function(){return i;});writers.push(function(value){i=value;});}writers[0](9);return[readers[0](),readers[1](),readers[2]()];}
function sharedCaptureWrites(){let value=0;const read=function(){return value;},out=[];for(var i=0;i<3;i++){value=i+1;out.push(read());}return[out.join(","),read()];}
row("risk-pristine-fusion",pristine);
row("risk-preloop-create-own",createOwn);
row("risk-preloop-create-getter-throw",createGetterThrow);
row("risk-preloop-append-own",appendOwn);
row("risk-preloop-append-getter-throw",appendGetterThrow);
row("risk-preloop-id-prototype-setter",prototypeIdSetter);
row("risk-preloop-id-prototype-setter-throw",prototypeIdSetterThrow);
row("risk-preloop-array-own-push",ownArrayPush);
row("risk-preloop-array-prototype-push",prototypeArrayPush);
row("risk-zero-trip-TDZ",zeroTripTDZ);
row("risk-zero-trip-getter",zeroTripGetter);
row("risk-refresh-counter-captures",freshCounterCaptures);
row("risk-refresh-body-captures",freshBodyCaptures);
row("risk-refresh-capture-writes",freshCaptureWrites);
row("risk-shared-capture-live-writes",sharedCaptureWrites);
console.log("W3COS_PR55_REVIEW_DONE");
