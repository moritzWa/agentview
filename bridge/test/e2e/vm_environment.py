"""Disposable real OpenCode service with a deterministic local model. No paid API keys.
Run ONLY in the macOS Cua VM. All processes end after 45 minutes or explicit stop.
"""
import base64,json,os,pathlib,signal,subprocess,sys,threading,time,urllib.request
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer

ROOT=pathlib.Path(os.environ.get('AGENTVOICE_FIXTURE_ROOT','/Users/lume/agentvoice-e2e-20261116'))
WORK=ROOT/'work';PORT=int(os.environ.get('AGENTVOICE_FIXTURE_PORT','31741'));MODEL_PORT=int(os.environ.get('AGENTVOICE_MODEL_PORT','31742'))
PASSWORD='disposable-agentvoice-loopback-only'

def api(path,body=None,method=None):
    headers={'Authorization':'Basic '+base64.b64encode(('opencode:'+PASSWORD).encode()).decode()}
    data=None
    if body is not None:data=json.dumps(body).encode();headers['Content-Type']='application/json'
    request=urllib.request.Request(f'http://127.0.0.1:{PORT}'+path,data=data,headers=headers,method=method)
    with urllib.request.urlopen(request,timeout=30) as response:
        raw=response.read();return json.loads(raw) if raw else {}

class Model(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))))
        tools=body.get('tools',[]);messages=body.get('messages',[])
        last_user=next((m.get('content','') for m in reversed(messages) if m.get('role')=='user'),'')
        already_read=any(m.get('role')=='tool' for m in messages)
        read=next((t['function'] for t in tools if t.get('function',{}).get('name') in ['read','read_file']),None)
        call=None
        if read and 'read canary' in str(last_user).lower() and not already_read:
            properties=read.get('parameters',{}).get('properties',{})
            key=next((k for k in properties if k.lower() in ['path','filepath','file_path']),None)
            if key:call={'id':'call_fixture_read','type':'function','function':{'name':read['name'],'arguments':json.dumps({key:str(WORK/'canary.txt')})}}
        text='BRIDGE_E2E_AGENT_REPLY: processed the synthetic instruction.'
        if not body.get('stream'):
            answer={'id':'chatcmpl-fixture','object':'chat.completion','created':int(time.time()),'model':'echo','choices':[{'index':0,'message':{'role':'assistant','content':text},'finish_reason':'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':10,'total_tokens':20}}
            encoded=json.dumps(answer).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(encoded)));self.end_headers();self.wfile.write(encoded);return
        delta={'role':'assistant','content':text} if not call else {'role':'assistant','tool_calls':[{'index':0,**call}]}
        events=[{'id':'chatcmpl-fixture','object':'chat.completion.chunk','created':int(time.time()),'model':'echo','choices':[{'index':0,'delta':delta,'finish_reason':None}]},
                {'id':'chatcmpl-fixture','object':'chat.completion.chunk','created':int(time.time()),'model':'echo','choices':[{'index':0,'delta':{},'finish_reason':'tool_calls' if call else 'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':10,'total_tokens':20}}]
        encoded=(''.join('data: '+json.dumps(event)+'\n\n' for event in events)+'data: [DONE]\n\n').encode()
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(encoded)));self.end_headers();self.wfile.write(encoded)

def serve():
    ROOT.mkdir(mode=0o700,exist_ok=True);WORK.mkdir(exist_ok=True)
    for directory in ['config','data','cache','state','supervisor']:(ROOT/directory).mkdir(mode=0o700,exist_ok=True)
    (WORK/'canary.txt').write_text('PRIVATE_TOOL_ONLY_CANARY_739104. This must never enter the cloud text projection.')
    subprocess.run(['git','init','-q',str(WORK)],check=True)
    config={'$schema':'https://opencode.ai/config.json','model':'fixture/echo','providers':{'fixture':{'name':'Disposable model fixture','env':['BRIDGE_TEST_API_KEY'],'package':'@opencode/ai/providers/openai-compatible','settings':{'baseURL':f'http://127.0.0.1:{MODEL_PORT}/v1'},'models':{'echo':{'name':'Fixture','capabilities':{'tools':True,'input':['text'],'output':['text']},'limit':{'context':64000,'output':1024}}}}},'snapshots':False,'update':'disable'}
    (ROOT/'config'/'opencode.json').write_text(json.dumps(config))
    wrapper=ROOT/'opencode'
    variables={'XDG_CONFIG_HOME':str(ROOT/'config-home'),'OPENCODE_CONFIG_DIR':str(ROOT/'config'),'XDG_DATA_HOME':str(ROOT/'data'),'XDG_CACHE_HOME':str(ROOT/'cache'),'XDG_STATE_HOME':str(ROOT/'state'),'OPENCODE_DB':str(ROOT/'data'/'opencode'/'history-v1.db'),'BRIDGE_TEST_API_KEY':'fixture-no-real-key'}
    native=os.environ.get('AGENTVOICE_TEST_NATIVE_OPENCODE','/Users/lume/agentvoice-opencode-v2-test')
    wrapper.write_text('#!/bin/sh\n'+''.join("export %s='%s'\n"%(k,v) for k,v in variables.items())+"exec '"+native.replace("'","'\\''")+"' \"$@\"\n");wrapper.chmod(0o700)
    model=ThreadingHTTPServer(('127.0.0.1',MODEL_PORT),Model)
    threading.Thread(target=model.serve_forever,daemon=True).start()
    log=open(ROOT/'opencode.log','ab')
    child=subprocess.Popen([str(wrapper),'serve','--hostname','127.0.0.1','--port',str(PORT)],cwd=WORK,env={**os.environ,**variables,'OPENCODE_SERVER_PASSWORD':PASSWORD},stdout=log,stderr=log,start_new_session=True)
    (ROOT/'service.json').write_text(json.dumps({'url':f'http://127.0.0.1:{PORT}','pid':child.pid,'password':PASSWORD}));(ROOT/'service.json').chmod(0o600)
    (ROOT/'environment.pid').write_text(str(os.getpid()))
    def stop(*_):
        try:os.killpg(child.pid,signal.SIGTERM)
        except ProcessLookupError:pass
        model.shutdown();raise SystemExit()
    signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop);signal.signal(signal.SIGHUP,stop)
    deadline=time.monotonic()+45*60
    while child.poll() is None and time.monotonic()<deadline:time.sleep(1)
    stop()

if __name__=='__main__':
    command=sys.argv[1]
    if command=='serve':serve()
    elif command=='stop':os.kill(int((ROOT/'environment.pid').read_text()),signal.SIGTERM);print('Stopping owned test services')
    elif command=='pair':
        for line in subprocess.check_output(['ps','-axo','pid=,comm=,args='],text=True).splitlines():
            fields=line.strip().split(None,2)
            if len(fields)==3 and fields[1].endswith('agentview-bridge-test-v1') and ' pair ' in fields[2]+' ':os.kill(int(fields[0]),signal.SIGTERM)
        log=open(ROOT/'pair.log','wb');os.chmod(ROOT/'pair.log',0o600)
        child=subprocess.Popen(['/Users/lume/agentview-bridge-test-v1','--opencode-bin',str(ROOT/'opencode'),'bridge','--state-dir',str(ROOT/'bridge'),'pair','--database',str(ROOT/'data/opencode/history-v1.db'),'--supervisor-state',str(ROOT/'supervisor')],stdout=log,stderr=log,start_new_session=True)
        (ROOT/'pair.pid').write_text(str(child.pid));print('Started owned pairing process',child.pid)
    elif command=='create':
        response=api('/api/session',{'title':'AgentVoice E2E Synthetic','location':{'directory':str(WORK)},'model':{'providerID':'fixture','id':'echo'}})
        session=response.get('data',response);(ROOT/'session.json').write_text(json.dumps({'id':session['id']}));print('Created disposable session',session['id'])
    elif command=='prompt':
        session=json.loads((ROOT/'session.json').read_text())['id'];response=api(f'/api/session/{session}/prompt',{'id':'msg_e2e_initial_'+session,'text':'Please read canary.txt from the workspace (read canary), then acknowledge. Do not quote its contents.','delivery':'queue','resume':True});print('Prompt admitted',bool(response.get('data')))
    elif command=='status':
        session=json.loads((ROOT/'session.json').read_text())['id'];response=api(f'/api/session/{session}/message');messages=response.get('data',response)
        print(json.dumps({'messages':len(messages) if isinstance(messages,list) else None,'types':[m.get('type') for m in messages] if isinstance(messages,list) else None}))
    elif command in ['bulk','large']:
        response=api('/api/session',{'title':'AgentVoice payload budget fixture','location':{'directory':str(WORK)},'model':{'providerID':'fixture','id':'echo'}})
        session=response.get('data',response)['id']
        count=4 if command=='bulk' else 1
        text='BULK_VISIBLE_CANARY '+('synthetic payload budget text '*(6200 if command=='bulk' else 24_700))
        for index in range(count):
            api(f'/api/session/{session}/prompt',{'id':'msg_bulk_'+session+'_'+str(index),'text':text,'delivery':'queue','resume':True})
        print(json.dumps({'id':session,'visible_user_characters':count*len(text)}))
    elif command=='api':
        path=sys.argv[2];body=json.loads(sys.argv[3]) if len(sys.argv)>3 and sys.argv[3]!='null' else None
        result=api(path,body,sys.argv[4] if len(sys.argv)>4 else None);print(json.dumps(result))
    elif command=='stop-bridge':
        config=json.loads((ROOT/'bridge/config.json').read_text());label='com.agentview.bridge.'+config['device_id']
        subprocess.run(['launchctl','bootout','gui/'+str(os.getuid())+'/'+label],capture_output=True)
        print('Stopped owned bridge login item')
