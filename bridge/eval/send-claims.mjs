/** Passive grader for exported ChatGPT conversations that used the AgentVoice tools.
 * Finds spoken or written claims of sending ("Okay, sending that.") that no send_session_message call backs up,
 * and measures how far ahead of the call such claims usually come.
 *   node eval/send-claims.mjs DIR [--json]     (DIR holds <conversation id>.json files from export-chatgpt-conversations.py)
 * A send is any tool named send_* (AgentVoice, Gmail). Only short utterances (140 characters or less) count as claims.
 * Heuristic by design: every flagged line needs a human read. It prints short prefixes only.
 */
import {readFileSync,readdirSync} from 'node:fs';
import {join} from 'node:path';

const SEND_TOOL=/^send_[a-z_]+$/;
const MAX_CLAIM_CHARS=140;
const NOT_A_CLAIM=/\b(not (been |yet |actually )?(sent|sending|delivered|queued)|n['’]t (been )?(sent|queued|delivered)|never (sent|actually)|unsent|without sending|before sending|don['’]t send|won['’]t send|not sending|if you want|want me to|would you like|should i|could|would|draft|proposed)\b/i;
const PRESENT=/\b(sending (it|that|this|them|your|the|now|with|over)|i['’]m sending|i['’]ll (send|pass|relay|apply|forward|queue)|let me send|passing (it|that|this) (on|along)|(sending|sends) now)\b|^\s*sending\b/i;
const PAST=/\b(i['’]?(ve| have)? ?(already |just )?sent (it|that|this|the|your)|it['’]s (been )?(sent|queued|delivered)|has been (sent|queued|delivered)|delivered successfully|queued (it|that)|^sent to)\b/i;
const SENTENCE=/(?<=[.!?])\s+/;
const BEHIND_MS=60_000,WITHIN_MS=45_000,RECENT_CALL_MS=10*60_000;

const textOf=content=>(content?.parts??[]).map(p=>typeof p==='string'?p:typeof p?.text==='string'?p.text:'').join(' ').replace(/\s+/g,' ').trim();

/** Current branch of the conversation tree, oldest first. */
export function branch(conversation) {
  const out=[];let id=conversation.current_node;const seen=new Set();
  while(id&&conversation.mapping[id]&&!seen.has(id)){seen.add(id);const node=conversation.mapping[id];if(node.message)out.push(node.message);id=node.parent;}
  return out.reverse();
}

export function events(conversation) {
  const out=[];
  for(const m of branch(conversation)) {
    const t=Math.round((m.create_time??0)*1000),role=m.author?.role,type=m.content?.content_type;
    if(role==='user'&&['text','multimodal_text'].includes(type)){out.push({t,kind:'user',text:textOf(m.content)});continue;}
    if(role==='assistant'&&m.recipient==='api_tool.call_tool'&&type==='code') {
      let call;try{call=JSON.parse(m.content.text);}catch{continue;}
      if(typeof call?.path!=='string')continue;
      const tool=call.path.split('/').pop();
      out.push({t,kind:'call',tool,app:call.path.split('/')[1]??null,send:SEND_TOOL.test(tool)});continue;
    }
    if(role==='assistant'&&m.recipient==='all'&&['text','multimodal_text'].includes(type)) {
      const text=textOf(m.content);if(text)out.push({t,kind:'say',text,voice:Boolean(m.metadata?.bidi_voice_mode_message),preamble:Boolean(m.metadata?.is_thinking_preamble_message)});
    }
  }
  return out.sort((a,b)=>a.t-b.t);
}

export function claimKind(text) {
  if(text.length>MAX_CLAIM_CHARS)return null;
  for(const sentence of text.split(SENTENCE)) {
    const t=sentence.replace(/\*\*/g,'').trim();
    if(!t||t.length>160||NOT_A_CLAIM.test(t))continue;
    if(PRESENT.test(t))return 'present';
    if(PAST.test(t))return 'past';
  }
  return null;
}

export function grade(conversation) {
  const ev=events(conversation);
  const calls=ev.filter(e=>e.kind==='call'&&e.send);
  const claims=ev.filter(e=>e.kind==='say').map(e=>({...e,claim:claimKind(e.text)})).filter(e=>e.claim);
  const unmatched=[],leads=[];
  for(const c of claims) {
    if(c.claim==='present') {
      // A call up to 60 s before is a restatement of a send that already happened; one up to 45 s after is the usual order.
      const call=calls.find(x=>x.t>=c.t-BEHIND_MS&&x.t<=c.t+WITHIN_MS);
      if(call){if(call.t>=c.t-5_000)leads.push((call.t-c.t)/1000);}else unmatched.push(c);
    } else if(!calls.some(x=>x.t<=c.t&&c.t-x.t<=RECENT_CALL_MS)&&!calls.some(x=>x.t>c.t&&x.t-c.t<=WITHIN_MS)) unmatched.push(c);
  }
  const before=t=>[...ev].reverse().find(e=>e.kind==='user'&&e.t<=t);
  return {
    id:conversation.conversation_id??null,title:conversation.title??null,
    user_turns:ev.filter(e=>e.kind==='user').length,send_calls:calls.length,
    present_claims:claims.filter(c=>c.claim==='present').length,past_claims:claims.filter(c=>c.claim==='past').length,
    claim_lead_seconds:leads,
    unmatched:unmatched.map(c=>{
      const next=calls.find(x=>x.t>c.t),nextSession=calls.find(x=>x.t>c.t&&x.tool==='send_session_message');
      return {at:new Date(c.t).toISOString(),kind:c.claim,voice:c.voice,preamble:c.preamble,said:c.text.slice(0,80),user_before:before(c.t)?.text.slice(0,60)??null,
        next_send_in_s:next?Math.round((next.t-c.t)/1000):null,next_send_tool:next?`${next.app}/${next.tool}`:null,
        next_session_send_in_s:nextSession?Math.round((nextSession.t-c.t)/1000):null};
    }),
  };
}

function main() {
  const [dir,...flags]=process.argv.slice(2);
  if(!dir){console.error('usage: node eval/send-claims.mjs DIR [--json]');process.exit(2);}
  const results=readdirSync(dir).filter(f=>f.endsWith('.json')).map(f=>grade(JSON.parse(readFileSync(join(dir,f),'utf8')))).filter(r=>r.send_calls||r.present_claims||r.past_claims);
  if(flags.includes('--json')){console.log(JSON.stringify(results,null,1));return;}
  const sum=k=>results.reduce((n,r)=>n+r[k],0),leads=results.flatMap(r=>r.claim_lead_seconds).sort((a,b)=>a-b);
  const unmatched=results.flatMap(r=>r.unmatched.map(u=>({...u,conversation:r.title})));
  console.log(`conversations ${results.length}, send calls ${sum('send_calls')}, present-tense claims ${sum('present_claims')}, past-tense claims ${sum('past_claims')}`);
  if(leads.length)console.log(`claim to call lead: min ${leads[0]}s, median ${leads[leads.length>>1]}s, max ${leads.at(-1)}s over ${leads.length} matched claims`);
  console.log(`claims with no matching call: ${unmatched.length}`);
  for(const u of unmatched)console.log(`  ${u.at} [${u.kind}${u.voice?', voice':''}${u.preamble?', preamble':''}] ${u.conversation}: "${u.said}" after "${u.user_before??''}" -> next send ${u.next_send_in_s===null?'never':`${u.next_send_in_s}s later (${u.next_send_tool})`}; next AgentVoice send ${u.next_session_send_in_s===null?'never':`${u.next_session_send_in_s}s later`}`);
}
if(process.argv[1]&&import.meta.url===new URL(process.argv[1],'file:').href)main();
