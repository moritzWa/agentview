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
export const FOLLOW_MS=7*86400_000;

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

const CLOSED=/\b(nothing else( needed)?|nothing further|no further( action)?|all done|all finished|that'?s everything|nothing left|no action needed)\b/i;
const PAGE=/\b(page|section|chapter|part|vol|v)\s*$/i;
const PROGRESS=/\b(done|complete|completed|remaining|left|scored|passed|finished|benchmark|progress|so far|still)\b/i;

function sentenceAt(text,index) {
  const breaks=[text.lastIndexOf('\n',index),text.lastIndexOf('. ',index-1),text.lastIndexOf('! ',index-1),text.lastIndexOf('? ',index-1)];
  const start=Math.max(0,...breaks.map(j=>j<0?0:j+1));
  const ends=[text.indexOf('\n',index),text.indexOf('. ',index),text.indexOf('! ',index),text.indexOf('?',index)].filter(j=>j>=0);
  const end=ends.length?Math.min(...ends)+1:Math.min(text.length,index+140);
  return text.slice(start,end).replace(/^\s+/,'').trim();
}

/** A finished turn that still names unfinished work. Not a question, and not a
 * completed session. Looks only at the end of the reply, so an old plan the
 * agent later closed is not reopened. */
export function followUp(text) {
  if(typeof text!=='string'||!text.trim())return null;
  const tail=text.slice(-800),end=tail.slice(-180);
  if(CLOSED.test(end))return null;
  const ratios=[...tail.matchAll(/\b(\d{1,4})\s*(?:\/|of)\s*(\d{1,4})\b/gi)];
  for(let i=ratios.length-1;i>=0;i--) {
    const m=ratios[i],n=Number(m[1]),total=Number(m[2]);
    if(total<2||total>500||n>=total||n<1)continue;
    const before=tail.slice(Math.max(0,m.index-12),m.index);
    if(PAGE.test(before))continue;
    const around=tail.slice(Math.max(0,m.index-40),m.index+m[0].length+40);
    if(!m[0].includes('of')&&total<10&&!PROGRESS.test(around))continue;
    if(!m[0].includes('of')&&total>=10&&PAGE.test(before))continue;
    const sentence=sentenceAt(tail,m.index);
    if(OFFER.test(sentence)||CLOSED.test(sentence))continue;
    return {signal:'incomplete',reason:clip(sentence,140),action:`Decide what to do with the remaining ${total-n}.`,done:n,total};
  }
  const go=/\b(awaiting (?:your |a )?(?:go-?ahead|decision)|ready for (?:your |a )?go-?ahead|proposed (?:an |the )?(?:implementation|plan|approach)|needs? (?:your |a )?go-?ahead)\b/i.exec(tail);
  if(go) {
    const sentence=sentenceAt(tail,go.index);
    if(!OFFER.test(sentence)&&!CLOSED.test(sentence))return {signal:'decision',reason:clip(sentence,140),action:'Say whether to proceed.'};
  }
  const next=/(?:next(?: step)?(?::| is)\s+\S|\b(?:still (?:need to|to do|left|open|unfinished|(?:has|have|needs?) [^.\n]{0,60}?\bto (?:fix|finish|run|decide|resolve|ship|do))|not yet (?:done|finished)|remaining (?:work|step|benchmark|items)|left to (?:do|run|decide))\b)/i.exec(tail);
  if(next) {
    const sentence=sentenceAt(tail,next.index);
    if(!OFFER.test(sentence)&&!CLOSED.test(sentence))return {signal:'next_step',reason:clip(sentence,140),action:clip(sentence,90)};
  }
  return null;
}

/** Chat markdown out, one line, so a spoken line never reads "**" or list dashes. */
export const plain=t=>String(t??'').replace(/```[\s\S]*?```/g,' ').replace(/\[([^\]]*)\]\([^)]*\)/g,'$1').replace(/`([^`]*)`/g,'$1').replace(/\*\*|__/g,'').replace(/^\s*(?:#+|[-*]|\d+\.)\s+/gm,'').replace(/\s+/g,' ').trim();

/** The start of a text, cut at a sentence end when one fits, else at a word. */
export function headCut(text,max) {
  const t=plain(text);
  if([...t].length<=max)return t;
  const cut=[...t].slice(0,max).join(''),end=Math.max(cut.lastIndexOf('. '),cut.lastIndexOf('? '),cut.lastIndexOf('! '));
  if(end>=max*0.4)return cut.slice(0,end+1);
  const space=cut.lastIndexOf(' ');
  return (space>0?cut.slice(0,space):cut)+'…';
}

/** Last paragraph, for a confirmed needs_you reply that has no question mark. Its start carries the point, not its tail. */
export function endOfReply(text) {
  if(typeof text!=='string')return null;
  const paragraphs=text.trim().split(/\n\s*\n/).filter(p=>p.trim());
  return paragraphs.length?{text:headCut(paragraphs.at(-1),DECISION_CHARS),source:'end_of_last_reply'}:null;
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

const followRank={incomplete:0,decision:1,next_step:2};
const shortTitle=t=>{const chars=[...String(t??'')];return chars.length<=40?chars.join(''):chars.slice(0,39).join('')+'…';};
const shortSignal=f=>f.signal==='incomplete'&&f.done!=null?`${f.done} of ${f.total}`:shorten(f.reason,42);

/** @param rows annotated inventory rows (status, reason, attention_assessed, title, project_name, updated_at, mirrored)
 *  @param followRows done sessions from the last week, including ones the question pass already saw
 *  @param tails Map<session_id,{text,assistant_last}>: tail of the latest assistant message and whether it is newer than the latest user message */
export function buildDigest({rows,followRows=[],tails,now,counts,statusFresh,freshness,statusObservedAt,timing}) {
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
      decision:ask?plain(ask.text):attentionReason[row.reason]??attentionReason.reply,_source:ask?.source,_activity:activity??0});
  }
  items.sort((a,b)=>kindRank[a.kind]-kindRank[b.kind]||b._activity-a._activity||a.session_id.localeCompare(b.session_id));
  const seen=new Set(items.map(i=>i.session_id));
  const followUps=[];
  for(const row of followRows) {
    if(row.status!=='done'||seen.has(row.session_id))continue;
    const tail=tails.get(row.session_id);
    if(!tail?.assistant_last)continue;
    const found=followUp(tail.text);
    if(!found)continue;
    seen.add(row.session_id);
    const activity=realMs(row.updated_at);
    followUps.push({session_id:row.session_id,title:row.title,kind:'follow_up',signal:found.signal,urgency:'low',confirmed:false,
      reason:found.reason,action:found.action,done:found.done,total:found.total,_activity:activity??0,_rank:followRank[found.signal]});
  }
  followUps.sort((a,b)=>a._rank-b._rank||b._activity-a._activity||a.session_id.localeCompare(b.session_id));
  const followNamed=followUps.slice(0,3).map(({_activity,_rank,...i})=>i);
  const confirmedItems=items.filter(i=>i.confirmed),uncertain=items.filter(i=>!i.confirmed);
  // Overview shows confirmed decisions first; less certain ones are named, never argued.
  const topRaw=confirmedItems.slice(0,MAX_TOP);
  const top=topRaw.map(({_activity,_source,...i})=>i);
  const maybe=uncertain.slice(0,LESS_CERTAIN_MAX).map(({session_id,title})=>({session_id,title}));
  const statusAge=statusObservedAt?now-statusObservedAt:null,historyAge=freshness?.source_checked_at?now-Number(freshness.source_checked_at):null;
  const caveats=[];
  if(!statusFresh)caveats.push(`Session status is not live${statusAge!==null?` (last observed ${ageText(statusAge)} ago)`:''}; say so before presenting it as current.`);
  if(freshness&&freshness.mac_seen_recently===false)caveats.push('The Mac has not been seen recently, so this may be out of date.');
  const caveat=caveats.join(' ')||null;
  const sentence=t=>/[.?!…]$/.test(t)?t:t+'.';
  const lines=topRaw.map(i=>sentence(`${i.title}: ${spoken[i.kind]??(i._source==='end_of_last_reply'?headCut(i.decision,90):shorten(i.decision,90))}`));
  const hidden=confirmedItems.length-top.length;
  const followBit=followNamed.length?` ${followNamed.length===1?'One done session may need a follow-up':`${followNamed.length} done sessions may need a follow-up`}: ${followNamed.map(f=>`${shortTitle(f.title)} (${shortSignal(f)})`).join(', ')}.${followUps.length>followNamed.length?` ${followUps.length-followNamed.length} more.`:''}`:'';
  let say=!items.length&&!followNamed.length?'Nothing needs you right now.'
    :`${confirmedItems.length?`${confirmedItems.length===1?'One session needs':`${confirmedItems.length} sessions need`} you. ${lines.join(' ')}`:'Nothing is confirmed waiting on you.'}${hidden>0?` ${hidden} more.`:''}${maybe.length?` ${maybe.map(m=>m.title).join(' and ')} may also be waiting.`:''}${followBit}`;
  if(caveat&&!statusFresh)say+=` My view of your sessions is ${statusAge!==null?ageText(statusAge)+' old':'out of date'}.`;
  return {say,confirmed:confirmedItems.length,top,may_also_need_you:maybe,more_confirmed:Math.max(0,hidden),
    ...(followNamed.length?{follow_ups:followNamed,more_follow_ups:Math.max(0,followUps.length-followNamed.length),follow_up_note:'A follow-up is a next step stated at the end of a finished turn. It is not a session waiting on you, and it is read from the last synced reply, not a live review of the project.'}:{}),
    working:counts?.working??0,
    data_age:{status:statusAge===null?null:ageText(statusAge),history_checked:historyAge===null?null:ageText(historyAge),status_fresh:Boolean(statusFresh),mac_seen_recently:freshness?.mac_seen_recently??null,caveat},
    ...(timing?{timing_ms:timing}:{}),
    presentation:'Answer with `say`, in your own words if you must, and nothing else: no "I\'ll check", no list, no written copy of what you said aloud. Add detail only when asked. When the owner names a session, call session_brief. If follow_ups is set, name those titles and the short reason. Do not call a follow-up blocked.'};
}

/** Normalised substring match so "diagram bench" finds "diagram-bench". */
export const normalise=s=>String(s??'').toLowerCase().replace(/[^a-z0-9]+/g,' ').trim();
export function matchesQuery(row,query) {
  const hay=normalise(`${row.title??''} ${row.project_name??''} ${row.directory??''}`),tokens=normalise(query).split(' ').filter(Boolean);
  return tokens.length>0&&tokens.every(t=>hay.includes(t));
}
