#!/usr/bin/env python3
"""Read-only export of ChatGPT conversations that used the AgentVoice tools, over CDP.
Uses the page's own signed-in session (GET requests only). Writes one JSON file per conversation.
  CDP_PORT=19222 export-chatgpt.py --days 6 --out DIR
"""
import argparse, json, os, sys, time, urllib.request, websocket

TOOLS = ['send_session_message','list_sessions','session_brief','message_status','mac_presence','read_session','search_history']

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=int(os.environ.get('CDP_PORT', '19222')))
    ap.add_argument('--days', type=float, default=6)
    ap.add_argument('--out', required=True)
    a = ap.parse_args()
    tabs = [t for t in json.load(urllib.request.urlopen(f'http://127.0.0.1:{a.port}/json', timeout=10)) if t['type'] == 'page' and 'chatgpt.com' in t['url']]
    if not tabs: sys.exit('no chatgpt.com tab')
    ws = websocket.create_connection(tabs[0]['webSocketDebuggerUrl'], timeout=120, suppress_origin=True)
    n = [0]
    def ev(expr):
        n[0] += 1
        ws.send(json.dumps({'id': n[0], 'method': 'Runtime.evaluate', 'params': {'expression': expr, 'awaitPromise': True, 'returnByValue': True}}))
        while True:
            m = json.loads(ws.recv())
            if m.get('id') == n[0]:
                r = m['result']
                if 'exceptionDetails' in r: raise RuntimeError(json.dumps(r['exceptionDetails'])[:300])
                return r['result'].get('value')
    ev("(async()=>{window.__tok=(await (await fetch('/api/auth/session')).json()).accessToken;return !!window.__tok})()")
    listing = json.loads(ev("(async()=>{const out=[];for(let o=0;o<400;o+=50){const r=await (await fetch('/backend-api/conversations?offset='+o+'&limit=50&order=updated',{headers:{Authorization:'Bearer '+window.__tok}})).json();out.push(...(r.items||[]));if(!r.items||r.items.length<50)break;}return JSON.stringify(out.map(c=>({id:c.id,title:c.title,update_time:c.update_time,create_time:c.create_time})))})()"))
    recent = [c for c in listing if c['update_time'] and (time.time() - (c['update_time'] if isinstance(c['update_time'], (int, float)) else time.mktime(time.strptime(c['update_time'][:19], '%Y-%m-%dT%H:%M:%S')))) < a.days * 86400]
    print(f'{len(listing)} conversations listed, {len(recent)} in the last {a.days} days', file=sys.stderr)
    os.makedirs(a.out, exist_ok=True)
    kept = 0
    for c in recent:
        raw = ev("(async()=>{const r=await fetch('/backend-api/conversation/%s',{headers:{Authorization:'Bearer '+window.__tok}});return r.ok?await r.text():''})()" % c['id'])
        if not raw: continue
        if not any(t in raw for t in TOOLS): continue
        open(os.path.join(a.out, c['id'] + '.json'), 'w').write(raw)
        kept += 1
        print(f"kept {c['id'][:8]} {len(raw)//1024} KB {str(c['title'])[:40]!r}", file=sys.stderr)
    print(f'{kept} conversations used AgentVoice tools', file=sys.stderr)

main()
