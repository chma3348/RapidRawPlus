"""Fetch candidate sky photographs from Wikimedia Commons.

Only freely licensed files (public domain / CC0 / CC BY / CC BY-SA) are
kept, and every file's licence, author and source page are recorded in
skies/manifest.json so attribution is never lost.
"""
import json, os, re, sys, time, urllib.parse, urllib.request

OUT = sys.argv[1]
RAW = os.path.join(OUT, "raw")
os.makedirs(RAW, exist_ok=True)
UA = "RapidRAWPlus/1.0 (personal photo-editor sky library; local use)"
OK_LICENCE = re.compile(r"(public domain|cc0|cc by|cc-by)", re.I)

QUERIES = {
    "blue-cumulus": "blue sky white cumulus clouds",
    "blue-clear": "clear blue sky gradient",
    "wispy-cirrus": "cirrus clouds blue sky",
    "mackerel": "altocumulus mackerel sky",
    "sunset-orange": "orange sunset sky clouds",
    "sunset-dramatic": "dramatic sunset sky red clouds",
    "sunrise-pink": "sunrise sky pink clouds",
    "golden-hour": "golden hour sky clouds",
    "overcast": "overcast grey sky clouds",
    "storm": "storm clouds dark sky",
    "twilight": "twilight blue hour sky",
    "backlit": "sun rays backlit clouds sky",
}

def api(params):
    url = "https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=40) as r:
        return json.load(r)

def strip_html(s):
    return re.sub(r"<[^>]+>", "", s or "").strip()

manifest = []
seen = set()
for cat, q in QUERIES.items():
    try:
        d = api({"action": "query", "generator": "search", "gsrsearch": f"filetype:bitmap {q}",
                 "gsrnamespace": 6, "gsrlimit": 14, "prop": "imageinfo",
                 "iiprop": "url|size|mime|extmetadata", "iiurlwidth": 4096, "format": "json"})
    except Exception as e:
        print("search failed", cat, e); continue
    for page in (d.get("query", {}).get("pages") or {}).values():
        ii = page["imageinfo"][0]; em = ii.get("extmetadata", {})
        lic = (em.get("LicenseShortName", {}).get("value") or "")
        if ii.get("mime") != "image/jpeg" or ii["width"] < 3000 or not OK_LICENCE.search(lic):
            continue
        title = page["title"]
        if title in seen:
            continue
        seen.add(title)
        name = re.sub(r"[^A-Za-z0-9]+", "_", title[5:])[:60] + ".jpg"
        path = os.path.join(RAW, name)
        if not os.path.exists(path):
            try:
                req = urllib.request.Request(ii.get("thumburl") or ii["url"], headers={"User-Agent": UA})
                with urllib.request.urlopen(req, timeout=90) as r, open(path, "wb") as f:
                    f.write(r.read())
                time.sleep(0.4)
            except Exception as e:
                print("download failed", name, e); continue
        manifest.append({"file": name, "query_category": cat, "title": strip_html(title),
                         "licence": lic, "author": strip_html(em.get("Artist", {}).get("value")),
                         "source": ii.get("descriptionurl"),
                         "width": ii.get("thumbwidth", ii["width"]), "height": ii.get("thumbheight", ii["height"])})
        print(f"{cat:<16} {name[:50]:<52} {lic}", flush=True)
json.dump(manifest, open(os.path.join(OUT, "manifest.json"), "w"), indent=1)
print("candidates:", len(manifest))
