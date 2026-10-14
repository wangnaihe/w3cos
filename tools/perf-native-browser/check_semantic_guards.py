#!/usr/bin/env python3
"""Isolated semantic qualification only; no performance timer or acceptance runner."""
from datetime import datetime,timezone
from pathlib import Path
import argparse,hashlib,json,os,re,subprocess,time
from playwright.sync_api import sync_playwright
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
utc=lambda:datetime.now(timezone.utc).isoformat()
p=argparse.ArgumentParser(description=__doc__)
for key in ['source','baseline','candidate','chrome','output']:p.add_argument('--'+key,type=Path,required=True)
p.add_argument('--expected-cases',type=int,required=True);a=p.parse_args();out=a.output.resolve();out.mkdir(mode=0o700,parents=False,exist_ok=False)
source=a.source.read_text();names=re.findall(r'row\("([^"\n]+)"\s*,',source)
assert len(names)==a.expected_cases and len(set(names))==len(names)
identities={k:sha(getattr(a,k))for k in ['source','baseline','candidate','chrome']}
def write_new(path,value):
 with path.open('x')as f:json.dump(value,f,indent=2);f.write('\n')
write_new(out/'consumed.json',{'utc':utc(),'identities':identities,'cases':names,'retries':0,'scope':'semantics only; not speed measurement'})
marker='W3COS_PR55_REVIEW '
def parse(text,name):
 rows=[]
 for line in text.splitlines():
  if line.startswith('[js.console.log] '):line=line[17:]
  if line.startswith(marker):rows.append(json.loads(line[len(marker):]))
 assert len(rows)<=1 and all(x['name']==name for x in rows)
 return rows[0]['actual']if rows else None
records={k:{}for k in ['chrome','baseline','candidate']};details={k:[]for k in records}
try:
 with sync_playwright()as pw:
  browser=pw.chromium.launch(executable_path=str(a.chrome),headless=True,chromium_sandbox=True,ignore_default_args=['--unsafely-disable-devtools-self-xss-warnings','--enable-unsafe-swiftshader'])
  try:
   version=browser.version
   for name in names:
    context=browser.new_context(offline=True,service_workers='block',viewport={'width':1024,'height':768},device_scale_factor=1);logs=[];errors=[]
    try:
     page=context.new_page();page.on('console',lambda m:logs.append(m.text));page.on('pageerror',lambda e:errors.append(str(e)))
     page.set_content('<!doctype html><html><body></body></html>');page.evaluate('(name)=>window.W3COS_REVIEW_CASE=name',name)
     with page.expect_console_message(lambda m:m.text=='W3COS_PR55_REVIEW_DONE',timeout=30000):page.add_script_tag(type='module',content=source)
     actual=parse('\n'.join(logs),name);assert not errors and actual is not None
     records['chrome'][name]=actual;details['chrome'].append({'name':name,'complete':True,'fresh_context':True})
    except Exception as e:
     records['chrome'][name]={'driver_failure':str(e)};details['chrome'].append({'name':name,'complete':False,'error':str(e)})
    finally:
     context.close();(out/(name+'.chrome.log')).write_text('\n'.join(logs)+'\n');write_new(out/(name+'.chrome.json'),{'actual':records['chrome'][name],'errors':errors})
  finally:browser.close()
 env=dict(os.environ,W3COS_JS_CONSOLE='1')
 for key in ['W3COS_AOT_WINDOW','W3COS_PERF_WORKLOAD','W3COS_DOM_DUMP','W3COS_COLLECTOR_START_GATE']:env.pop(key,None)
 for label,binary in [('baseline',a.baseline),('candidate',a.candidate)]:
  folder=out/label;folder.mkdir(mode=0o700)
  for name in names:
   start=utc();process=subprocess.Popen([str(binary)],env=dict(env,W3COS_REVIEW_CASE=name),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
   write_new(folder/(name+'.started.json'),{'pid':process.pid,'utc':start,'binary_sha256':identities[label]})
   timed_out=False
   try:stdout,stderr=process.communicate(timeout=30)
   except subprocess.TimeoutExpired:
    timed_out=True;process.kill();stdout,stderr=process.communicate()
   (folder/(name+'.stdout.log')).write_text(stdout);(folder/(name+'.stderr.log')).write_text(stderr)
   actual=parse(stdout+'\n'+stderr,name)
   if actual is None:actual={'process_failure':'timeout'if timed_out else'stack_overflow'if'stack overflow'in stderr else'unobserved','exit':process.returncode}
   complete='W3COS_PR55_REVIEW_DONE'in stdout+stderr
   row={'name':name,'pid':process.pid,'exit':process.returncode,'complete':complete,'timed_out':timed_out,'start_utc':start,'end_utc':utc(),'actual':actual}
   write_new(folder/(name+'.json'),row);records[label][name]=actual;details[label].append(row)
  assert sha(binary)==identities[label],'Binary changed during qualification'
finally:
 rows=[]
 for name in names:
  row={'name':name,**{k:v.get(name,{'not_completed':True})for k,v in records.items()}}
  c,b,h=row['chrome'],row['baseline'],row['candidate']
  row['classification']='PASS'if h==c and b==c else'IMPROVED_BASELINE'if h==c else'OLD_BASELINE_DIFFERENCE'if b==h and b!=c else'NEW_REGRESSION'if b==c else'CHANGED_BASELINE_OR_UNRESOLVED'
  rows.append(row)
 result={'scope':'semantic qualification only','source_sha256':identities['source'],'identities':identities,'chrome_version':locals().get('version'),'sandbox':True,'fresh_Chrome_context_percase':True,'rows':rows,'processes':details,'new_regressions':[r['name']for r in rows if r['classification']=='NEW_REGRESSION'],'old_baseline_differences':[r['name']for r in rows if r['classification']=='OLD_BASELINE_DIFFERENCE'],'changed_or_unresolved':[r['name']for r in rows if r['classification']=='CHANGED_BASELINE_OR_UNRESOLVED'],'performance_samples':0}
 result['all_processes_complete']=all(x.get('complete')and x.get('exit',0)==0 and not x.get('timed_out',False)for values in details.values()for x in values)and all(len(values)==len(names)for values in details.values())
 result['correctness_gate']=not result['new_regressions']and not result['changed_or_unresolved']and result['all_processes_complete']
 write_new(out/'comparison.json',result)
print(json.dumps({k:result[k]for k in ['correctness_gate','new_regressions','old_baseline_differences','changed_or_unresolved','all_processes_complete','performance_samples']}))
raise SystemExit(not result['correctness_gate'])
