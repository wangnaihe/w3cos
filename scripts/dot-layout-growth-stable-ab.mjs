// Node 22+. Fresh Chrome profile and native process per position/trial.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

async function inBlankPage(font64, count, pos, checkOnly) {
  const font = new FontFace('Ahem', 'url(data:font/ttf;base64,' + font64 + ')');
  document.fonts.add(await font.load());
  await document.fonts.ready;
  if (devicePixelRatio !== 1 || !document.fonts.check('10px Ahem')) throw Error('DPR/font');
  const short = 'XXXXXXXXXX', long = 'XXXXXXXXXXX';
  const make = (tag, parent, styles) => {
    const node = document.createElement(tag);
    Object.assign(node.style, styles);
    parent.appendChild(node);
    return node;
  };
  Object.assign(document.body.style, {margin:'0px', padding:'0px'});
  function build(isLong) {
    document.body.replaceChildren();
    const scroll = make('div', document.body, {width:'160px',height:'240px',overflow:'hidden'});
    const outside = make('div', document.body, {width:'10px',height:'5px'});
    const entries = [];
    for (let i = 0; i < count; ++i) {
      const card = make('div', scroll, {width:'140px'});
      const p = make('p', card, {margin:'0px', padding:'0px',fontFamily:'Ahem',fontSize:'10px',lineHeight:'20px',whiteSpace:'normal'});
      const span = make('span', p, {});
      span.append(document.createTextNode(i === pos && isLong ? long : short));
      const spacer = make('div', card, {height:'80px'});
      entries.push({card,p,span,spacer});
    }
    return {scroll,outside,entries};
  }
  const eq = (a,b,context) => {if (JSON.stringify(a) !== JSON.stringify(b)) throw Error(context+': '+JSON.stringify(a)+' != '+JSON.stringify(b));};
  const tuple = r => [r.x,r.y,r.width,r.height];
  const rect = n => tuple(n.getBoundingClientRect());
  function snapshot(fixture, isLong) {
    eq(rect(fixture.scroll),[0,0,160,240],'scroll');
    eq(rect(fixture.outside),[0,240,10,5],'outside');
    const boxes = fixture.entries.map(({card,p,spacer},i) => {
      const y = i*100, h = 20;
      const r = [rect(card),rect(p),rect(spacer)];
      eq(r,[[0,y,140,h+80],[0,y,140,h],[0,y+h,140,80]],'card '+i);
      return r;
    });
    const lines = Array.from(fixture.entries[pos].span.getClientRects(),tuple);
    eq(lines.map(r=>[r[0],r[2]]),isLong?[[0,110]]:[[0,100]],'inline x/width');
    return {boxes,lines};
  }
  const coldLong=snapshot(build(true),true);
  const fixture=build(false), coldShort=snapshot(fixture,false);
  const {p,span}=fixture.entries[pos], text=span.firstChild;
  text.data=long;
  eq(snapshot(fixture,true),coldLong,'warm growth');
  text.data=short;
  eq(snapshot(fixture,false),coldShort,'warm shrink');
  const check={count,position:pos,cold_parity:true};
  if(checkOnly) return check;
  const samples=[];
  for(let batch=0;batch<26;batch++) {
    let correct=0,sum=0;
    const start=performance.now();
    for(let i=0;i<128;i++) {
      const grow=i%2===0;
      text.data=grow?long:short;
      const width=span.getBoundingClientRect().width;
      correct+=Number(width===(grow?110:100));
      sum+=width;
    }
    const wall_ns=Math.round((performance.now()-start)*1e6);
    if(correct!==128 || sum!==13440) throw Error('wrong timed geometry');
    if(batch>=6) samples.push({wall_ns});
  }
  eq(snapshot(fixture,false),coldShort,'end geometry');
  return {check,ops_per_batch:128,samples};
}

async function cdp(socket) {
  const ws=new WebSocket(socket);
  const pending=new Map();
  let id=0;
  ws.addEventListener('message', event=>{
    const r=JSON.parse(event.data);
    const fn=pending.get(r.id);
    if(fn) {pending.delete(r.id);fn(r);}
  });
  await new Promise((ok, bad)=>{
    const timeout=setTimeout(()=>{ws.close();bad(Error('CDP open timeout'));},5000);
    ws.addEventListener('open',()=>{clearTimeout(timeout);ok();},{once:true});
    ws.addEventListener('error',()=>{clearTimeout(timeout);bad(Error('CDP open error'));},{once:true});
  });
  return {
    requestShutdown:()=>{
      if(ws.readyState===WebSocket.OPEN) ws.send(JSON.stringify({id:++id,method:'Browser.close'}));
    },
    close:()=>ws.close(),
    call:(method,params={},sessionId)=>new Promise((ok,bad)=>{
      const req=++id;
      const t=setTimeout(()=>{pending.delete(req);bad(Error(method+' timeout'));},20000);
      pending.set(req,r=>{clearTimeout(t);if(r.error) bad(Error(method+': '+JSON.stringify(r.error)));else ok(r.result);});
      ws.send(JSON.stringify({id:req,method,params,...(sessionId ? {sessionId} : {})}));
    }),
  };
}

async function chromeRun(chrome,font64,count,pos,checkOnly) {
  const profile=await mkdtemp(join(tmpdir(),'w3cos-growth-chrome-'));
  const child=spawn(chrome,['--headless','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0',
    '--disable-background-networking','--no-first-run','--no-default-browser-check','--force-device-scale-factor=1',
    '--user-data-dir='+profile,'about:blank'],{stdio:['ignore','ignore','pipe']});
  let stderr='', spawnError;
  child.stderr.on('data',s=>{stderr=(stderr+s).slice(-4000);});
  child.on('error',err=>{spawnError=err;});
  let proto;
  try {
    let path;
    for(let attempts=0;attempts<200;attempts++) {
      if(spawnError || child.exitCode!==null || child.signalCode!==null) break;
      try {path=(await readFile(join(profile,'DevToolsActivePort'),'utf8')).trim().split('\n');} catch(e) {if(e.code!=='ENOENT') throw e;}
      if(path?.length===2) break;
      await delay(50);
    }
    assert.match(path?.[0]??'',/^\d+$/, 'Chrome loopback port');
    assert.match(path?.[1]??'',/^\/devtools\/browser\/[a-z0-9-]+$/, 'Chrome loopback path');
    proto=await cdp('ws://127.0.0.1:'+path[0]+path[1]);
    const version=await proto.call('Browser.getVersion');
    const {targetId}=await proto.call('Target.createTarget',{url:'about:blank'});
    const {sessionId}=await proto.call('Target.attachToTarget',{targetId,flatten:true});
    await proto.call('Emulation.setDeviceMetricsOverride',{width:160,height:240,deviceScaleFactor:1,mobile:false},sessionId);
    const blank=await proto.call('Runtime.evaluate',{expression:'[innerWidth,innerHeight,devicePixelRatio]',returnByValue:true},sessionId);
    assert.deepEqual(blank.result.value,[160,240,1]);
    const expression='('+inBlankPage.toString()+')('+JSON.stringify(font64)+','+count+','+pos+','+checkOnly+')';
    const evaluated=await proto.call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true},sessionId);
    if(evaluated.exceptionDetails) throw Error(JSON.stringify(evaluated.exceptionDetails));
    return {...evaluated.result.value, browser:version.product};
  } catch(e) {
    throw Error((spawnError??e).message+'\n'+stderr,{cause:e});
  } finally {
    if(child.pid && child.exitCode===null && child.signalCode===null) {
      const stopped=once(child,'exit').then(()=>true);
      const waitForExit=(ms)=>Promise.race([stopped,delay(ms,false,{ref:false})]);
      if(proto) proto.requestShutdown();
      if(!proto || !await waitForExit(2000)) {
        child.kill('SIGTERM');
        if(!await waitForExit(2000)) {child.kill('SIGKILL');await stopped;}
      }
    }
    proto?.close();
    // A Chrome helper can finish writing after its parent exits.
    await rm(profile,{recursive:true,force:true,maxRetries:12,retryDelay:150});
  }
}

const [native,fontPath,chrome='google-chrome']=process.argv.slice(2);
assert(native && fontPath,'usage: node dot-layout-growth-stable-ab.mjs NATIVE Ahem.ttf [CHROME]');
assert(!Object.hasOwn(process.env,'W3COS_SPLICE_VERIFY'),'keep splice verification outside timing');
const font=(await readFile(fontPath)).toString('base64');
const cases=[[300,0],[300,299],[1200,0],[1200,1199]];
const engines=['w3cos-default','w3cos-candidate','chrome'];
const orders=engines.map((_,i)=>engines.slice(i).concat(engines.slice(0,i)));
orders.push(...orders.map(order=>[...order].reverse()));
const median=xs=>{const s=[...xs].sort((a,b)=>a-b);return(s[(s.length-1)>>1]+s[s.length>>1])/2;};
const all=[];
const browsers=new Set();
function nativeRun(engine,count,pos) {
  const enabled=engine==='w3cos-candidate'?'1':'0';
  const env={...process.env};
  for(const flag of ['W3COS_TAIL_SHIFT_FAST','W3COS_PREORDER_HOSTS','W3COS_TAIL_SHIFT_RANGES',
    'W3COS_CSSOM_OWNER_INDEX','DOT_LAYOUT_ADAPTIVE_SHIFT','DOT_LAYOUT_LAZY_HOSTS']) env[flag]=enabled;
  return JSON.parse(execFileSync(native,[fontPath,String(count),String(pos)],{encoding:'utf8',timeout:45000,env}));
}
for(const [count,pos] of cases) {
  const check=await chromeRun(chrome,font,count,pos,true);
  assert(check.cold_parity && check.count===count && check.position===pos,JSON.stringify(check));
  console.log(JSON.stringify({phase:'chrome-geometry',...check}));
}
for(let trial=0;trial<orders.length;trial++) {
  const rotation=trial%cases.length;
  for(const [count,pos] of cases.slice(rotation).concat(cases.slice(0,rotation))) {
    for(const engine of orders[trial]) {
      const row=engine==='chrome'?await chromeRun(chrome,font,count,pos,false):nativeRun(engine,count,pos);
      assert(row.check.cold_parity && row.check.count===count && row.check.position===pos,JSON.stringify(row.check));
      if(engine==='chrome') {
        browsers.add(row.browser);
        assert.equal(browsers.size,1,'Chrome version changed between trials');
      }
      assert.equal(row.ops_per_batch,128);
      assert.equal(row.samples.length,20);
      assert(row.samples.every(s=>Number.isSafeInteger(s.wall_ns)&&s.wall_ns>0));
      const ns_per_edit=median(row.samples.map(s=>s.wall_ns))/128;
      const cpu_ns_per_edit=engine==='chrome'?null:median(row.samples.map(s=>s.thread_cpu_ns))/128;
      const summary={trial,count,pos,engine,ns_per_edit,cpu_ns_per_edit};
      all.push(summary);
      console.log(JSON.stringify({phase:'timed',...summary,...row}));
    }
  }
}
for(const [count,pos] of cases) for(const engine of engines) {
  const arm=all.filter(r=>r.count===count&&r.pos===pos&&r.engine===engine);
  assert.equal(arm.length,orders.length);
  const ctrl=other=>arm.map(r=>r.ns_per_edit-all.find(x=>x.trial===r.trial&&x.count===count&&x.pos===pos&&x.engine===other).ns_per_edit);
  console.log(JSON.stringify({phase:'summary',count,pos,engine,median_ns:median(arm.map(r=>r.ns_per_edit)),
    median_cpu_ns:engine==='chrome'?null:median(arm.map(r=>r.cpu_ns_per_edit)),
    versus_default_ns:ctrl('w3cos-default'),versus_chrome_ns:ctrl('chrome')}));
}
