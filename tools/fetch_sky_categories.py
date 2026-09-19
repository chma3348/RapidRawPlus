"""Second pass: pull files straight from Commons cloud/sky categories,
which is where pure-sky photographs live."""
import json, os, re, sys, time, urllib.parse, urllib.request
OUT=sys.argv[1]; RAW=os.path.join(OUT,"raw"); os.makedirs(RAW,exist_ok=True)
UA="RapidRAWPlus/1.0 (personal photo-editor sky library; local use)"
OK=re.compile(r"(public domain|cc0|cc by|cc-by)",re.I)
CATS=["Category:Cloudscapes","Category:Cirrus clouds","Category:Cumulus clouds",
      "Category:Altocumulus clouds","Category:Cirrocumulus clouds","Category:Stratocumulus clouds",
      "Category:Mammatus clouds","Category:Sunset skies","Category:Sunrise skies",
      "Category:Red skies","Category:Orange skies","Category:Overcast","Category:Blue skies",
      "Category:Thunderstorm clouds","Category:Crepuscular rays","Category:Clouds at sunset"]
def api(p):
    url="https://commons.wikimedia.org/w/api.php?"+urllib.parse.urlencode(p)
    return json.load(urllib.request.urlopen(urllib.request.Request(url,headers={"User-Agent":UA}),timeout=40))
def strip(s): return re.sub(r"<[^>]+>","",s or "").strip()
man=json.load(open(os.path.join(OUT,"manifest.json"))) if os.path.exists(os.path.join(OUT,"manifest.json")) else []
have={m["file"] for m in man}
for cat in CATS:
    try:
        d=api({"action":"query","generator":"categorymembers","gcmtitle":cat,"gcmtype":"file",
               "gcmlimit":30,"prop":"imageinfo","iiprop":"url|size|mime|extmetadata",
               "iiurlwidth":4096,"format":"json"})
    except Exception as e:
        print("cat failed",cat,e); continue
    kept=0
    for page in (d.get("query",{}).get("pages") or {}).values():
        if "imageinfo" not in page: continue
        ii=page["imageinfo"][0]; em=ii.get("extmetadata",{})
        lic=em.get("LicenseShortName",{}).get("value") or ""
        if ii.get("mime")!="image/jpeg" or ii["width"]<2600 or not OK.search(lic): continue
        name=re.sub(r"[^A-Za-z0-9]+","_",page["title"][5:])[:60]+".jpg"
        if name in have: continue
        path=os.path.join(RAW,name)
        if not os.path.exists(path):
            try:
                with urllib.request.urlopen(urllib.request.Request(ii.get("thumburl") or ii["url"],headers={"User-Agent":UA}),timeout=90) as r, open(path,"wb") as f:
                    f.write(r.read())
                time.sleep(0.3)
            except Exception as e:
                print("dl failed",name,str(e)[:60]); continue
        have.add(name); kept+=1
        man.append({"file":name,"query_category":cat.replace("Category:","").lower().replace(" ","-"),
                    "title":strip(page["title"]),"licence":lic,"author":strip(em.get("Artist",{}).get("value")),
                    "source":ii.get("descriptionurl"),"width":ii.get("thumbwidth",ii["width"]),
                    "height":ii.get("thumbheight",ii["height"])})
    print(f"{cat:<36} kept {kept}", flush=True)
json.dump(man,open(os.path.join(OUT,"manifest.json"),"w"),indent=1)
print("total candidates:",len(man))
