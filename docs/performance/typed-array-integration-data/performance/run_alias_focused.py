import csv,subprocess,statistics,json,random,pathlib,argparse
parser=argparse.ArgumentParser(description="Compare identically built alias-sync-baseline and alias-sync-candidate binaries")
parser.add_argument("evidence", type=pathlib.Path, help="Directory containing both binaries; raw CSV and JSON are written here")
out=parser.parse_args().evidence
rows=[]
cases=[(mode,n,aliases) for mode in ("indexed","fill") for n in (2048,) for aliases in (1,3)]
cases += [(mode,8192,1) for mode in ("construct","array_push")]
random.Random(20261023).shuffle(cases)
with (out/"alias-sync-raw.csv").open("w") as f:
 w=csv.writer(f);w.writerow(["pair","variant","mode","n","aliases","round","ns","checksum"])
 for mode,n,aliases in cases:
  for pair in range(8):
   for variant in (["baseline","candidate"] if pair%2==0 else ["candidate","baseline"]):
    p=subprocess.run([str(out/("alias-sync-"+variant)),mode,str(n),str(aliases)],check=True,capture_output=True,text=True)
    (out/("metadata-"+variant+".txt")).write_text(p.stderr)
    for line in p.stdout.splitlines():
     row=[pair,variant,*line.split(",")];w.writerow(row);rows.append(row)
    f.flush()
summary=[]
for mode,n,aliases in sorted(cases):
 paired=[]
 for pair in range(8):
  med={}
  for variant in ("baseline","candidate"):
   data=[int(r[6]) for r in rows if r[0]==pair and r[1]==variant and r[2]==mode and int(r[3])==n and int(r[4])==aliases and int(r[5])>=3]
   med[variant]=statistics.median(data)
  paired.append(med)
 base=statistics.median(p["baseline"] for p in paired);candidate=statistics.median(p["candidate"] for p in paired)
 ratios=[p["baseline"]/p["candidate"] for p in paired]
 item=dict(mode=mode,n=n,aliases=aliases,baseline_ns=base,candidate_ns=candidate,median_paired_speedup=statistics.median(ratios),ratio_range=[min(ratios),max(ratios)])
 summary.append(item);print(json.dumps(item),flush=True)
(out/"alias-sync-summary.json").write_text(json.dumps(summary,indent=2)+"\n")
