// Pure helpers that turn session rows into a short, prioritized attention
// digest. No I/O here, so the shapes are unit-testable with synthetic data.

const attentionReason={approval:'Approve a native OpenCode permission on the Mac.',question:'Answer a native OpenCode question on the Mac.',failed:'The last turn failed; decide whether to retry.',reply:'The latest assistant message is waiting for your reply or decision.'};
export {attentionReason};

// Phrases that mean "I am stuck on you" without a question mark. "Let me know if
// you want more" is deliberately absent: the desktop classifier calls it done.
const BLOCKER=/\b(blocked (on|by|until)|waiting (on|for) (you|your|a decision|human)|need(s)? (you|your) (to|input|decision|call)|your call|please (confirm|decide|choose|pick|provide|label|review|approve)|which (one|option|approach|of these))\b/i;
// Optional offers ("want me to...?", "if you want...") are what the desktop classifier
// calls done, so they never count as a decision on their own.
const OFFER=/\b(want me to|would you like me to|should i also|shall i also|let me know if|happy to|i can also|if you('d| would)? (like|want)|optional(ly)?|whenever you)\b/i;
const CHOICE=/\b(or|which|should|approve|confirm|decide|choose|pick|label|ship)\b/i;
const MAX_TOP=3,MAX_MORE=6,DECISION_CHARS=280;
const kindRank={approval:0,question:1,failed:2,reply:3,unconfirmed_ask:4,possible_decision:5};
const urgencyOf={approval:'high',question:'high',failed:'medium',reply:'medium',unconfirmed_ask:'low',possible_decision:'low'};
export const SOON_MS=24*3600_000;

const clip=(text,max)=>{const chars=[...text.trim().replace(/\s+/g,' ')];return chars.length<=max?chars.join(''):'…'+chars.slice(-(max-1)).join('');};

/** The last question or blocker sentence of a message, or null. `window` limits
 * how far from the end to look, so an old question in a long reply that later
 * moved on is not reported as the open decision. */
export function finalAsk(text,{window=600,strict=false}={}) {
  if(typeof text!=='string'||!text.trim())return null;
  const tail=text.slice(-window);
  // Index of the first character of the sentence or line that contains position i.
  const sentenceStart=i=>Math.max(0,tail.lastIndexOf('\n',i)+1,...['. ','! ','? '].map(m=>{const j=tail.lastIndexOf(m,i-1);return j<0?0:j+2;}));
  const decision=(q,source)=>{const t=clip(q,DECISION_CHARS);return OFFER.test(t)||(strict&&source==='question_in_last_reply'&&!CHOICE.test(t))?null:{text:t,source};};
  for(let q=tail.lastIndexOf('?');q>=0;q=tail.lastIndexOf('?',q-1)) {
    let start=sentenceStart(q),sentence=tail.slice(start,q+1).trim();
    if([...sentence].length<12&&start>0)sentence=tail.slice(sentenceStart(start-1),q+1).trim();
    const found=sentence&&decision(sentence,'question_in_last_reply');
    if(found)return found;
    if(q===0)break;
  }
  const match=BLOCKER.exec(tail);
  if(match){const end=tail.indexOf('\n',match.index);return decision(tail.slice(sentenceStart(match.index),end<0?tail.length:end),'blocker_phrase_in_last_reply');}
  return null;
}

/** Last paragraph, for a confirmed needs_you reply that has no question mark. */
export function endOfReply(text) {
  if(typeof text!=='string')return null;
  const paragraphs=text.trim().split(/\n\s*\n/).filter(p=>p.trim());
  return paragraphs.length?{text:clip(paragraphs.at(-1),DECISION_CHARS),source:'end_of_last_reply'}:null;
}

export function ageText(ms) {
  if(!Number.isFinite(ms)||ms<0)return null;
  const s=Math.round(ms/1000);
  if(s<90)return `${s} seconds`;
  const m=Math.round(s/60);if(m<90)return `${m} minutes`;
  const h=Math.round(m/60);if(h<36)return `${h} hours`;
  return `${Math.round(h/24)} days`;
}

const realMs=n=>Number.isFinite(Number(n))&&Number(n)>1e12?Number(n):null;

const shorten=(text,max)=>{const t=text.trim().replace(/\s+/g,' ');if([...t].length<=max)return t;const cut=[...t].slice(-max).join('');return cut.slice(cut.indexOf(' ')+1);};
const spoken={approval:'approve a permission',question:'answer a question',failed:'the last turn failed'};
const LESS_CERTAIN_MAX=2;

/** @param rows annotated inventory rows (status, reason, attention_assessed, title, project_name, updated_at, mirrored)
 *  @param tails Map<session_id,{text,assistant_last}>: tail of the latest assistant message and whether it is newer than the latest user message */
export function buildDigest({rows,tails,now,counts,statusFresh,freshness,statusObservedAt,timing}) {
  const items=[];
  for(const row of rows) {
    const tail=tails.get(row.session_id)??null;
    let kind=null,ask=null;
    if(row.status==='needs_you') {
      kind=['approval','question','failed'].includes(row.reason)?row.reason:'reply';
      if(kind==='reply'&&tail)ask=finalAsk(tail.text,{window:1500})??endOfReply(tail.text);
    } else if(row.status==='done'&&tail?.assistant_last) {
      // A turn the desktop classifier already judged finished needs a direct ask to be
      // reopened; one it never saw only needs to look like a question.
      ask=finalAsk(tail.text,{strict:Boolean(row.attention_assessed)});
      if(ask)kind=row.attention_assessed?'possible_decision':'unconfirmed_ask';
    }
    if(!kind)continue;
    const activity=realMs(row.updated_at);
    items.push({session_id:row.session_id,title:row.title,kind,urgency:urgencyOf[kind],confirmed:row.status==='needs_you',
      decision:ask?.text??attentionReason[row.reason]??attentionReason.reply,_activity:activity??0});
  }
  items.sort((a,b)=>kindRank[a.kind]-kindRank[b.kind]||b._activity-a._activity||a.session_id.localeCompare(b.session_id));
  const confirmedItems=items.filter(i=>i.confirmed),uncertain=items.filter(i=>!i.confirmed);
  // Overview shows confirmed decisions first; less certain ones are named, never argued.
  const top=confirmedItems.slice(0,MAX_TOP).map(({_activity,...i})=>i);
  const maybe=uncertain.slice(0,LESS_CERTAIN_MAX).map(({session_id,title})=>({session_id,title}));
  const statusAge=statusObservedAt?now-statusObservedAt:null,historyAge=freshness?.source_checked_at?now-Number(freshness.source_checked_at):null;
  const caveats=[];
  if(!statusFresh)caveats.push(`Session status is not live${statusAge!==null?` (last observed ${ageText(statusAge)} ago)`:''}; say so before presenting it as current.`);
  if(freshness&&freshness.mac_seen_recently===false)caveats.push('The Mac has not been seen recently, so this may be out of date.');
  const caveat=caveats.join(' ')||null;
  const lines=top.map(i=>`${i.title}: ${spoken[i.kind]??shorten(i.decision,90)}`);
  const hidden=confirmedItems.length-top.length;
  let say=!items.length?'Nothing needs you right now.'
    :`${confirmedItems.length?`${confirmedItems.length===1?'One session needs':`${confirmedItems.length} sessions need`} you. ${lines.join('. ')}.`:'Nothing is confirmed waiting on you.'}${hidden>0?` ${hidden} more.`:''}${maybe.length?` ${maybe.map(m=>m.title).join(' and ')} may also be waiting.`:''}`;
  if(caveat&&!statusFresh)say+=` My view of your sessions is ${statusAge!==null?ageText(statusAge)+' old':'out of date'}.`;
  return {say,confirmed:confirmedItems.length,top,may_also_need_you:maybe,more_confirmed:Math.max(0,hidden),working:counts?.working??0,
    data_age:{status:statusAge===null?null:ageText(statusAge),history_checked:historyAge===null?null:ageText(historyAge),status_fresh:Boolean(statusFresh),mac_seen_recently:freshness?.mac_seen_recently??null,caveat},
    ...(timing?{timing_ms:timing}:{}),
    presentation:'Answer with `say`, in your own words if you must, and nothing else: no "I\'ll check", no list, no written copy of what you said aloud. Add detail only when asked. When the owner names a session, call session_brief.'};
}

/** Normalised substring match so "diagram bench" finds "diagram-bench". */
export const normalise=s=>String(s??'').toLowerCase().replace(/[^a-z0-9]+/g,' ').trim();
export function matchesQuery(row,query) {
  const hay=normalise(`${row.title??''} ${row.project_name??''} ${row.directory??''}`),tokens=normalise(query).split(' ').filter(Boolean);
  return tokens.length>0&&tokens.every(t=>hay.includes(t));
}
