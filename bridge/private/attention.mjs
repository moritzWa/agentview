// Pure helpers that turn session rows into a short, prioritized attention
// digest. No I/O here, so the shapes are unit-testable with synthetic data.

const attentionReason={approval:'Approve a native OpenCode permission on the Mac.',question:'Answer a native OpenCode question on the Mac.',failed:'The last turn failed; decide whether to retry.',reply:'The latest assistant message is waiting for your reply or decision.'};
export {attentionReason};

// Phrases that mean "I am stuck on you" without a question mark. "Let me know if
// you want more" is deliberately absent: the desktop classifier calls it done.
const BLOCKER=/\b(blocked (on|by|until)|waiting (on|for) (you|your|a decision|human)|need(s)? (you|your) (to|input|decision|call)|your call|please (confirm|decide|choose|pick|provide|label|review|approve)|which (one|option|approach|of these))\b/i;
const MAX_TOP=3,MAX_MORE=6,DECISION_CHARS=280;
const kindRank={approval:0,question:1,failed:2,reply:3,unconfirmed_ask:4,possible_decision:5};
const urgencyOf={approval:'high',question:'high',failed:'medium',reply:'medium',unconfirmed_ask:'low',possible_decision:'low'};
const kindMeaning={approval:'needs a native permission approval',question:'has a native question open',failed:'last turn failed',reply:'last reply waits on you',unconfirmed_ask:'last reply looks like a question (not yet classified)',possible_decision:'marked done, but its last reply still looks like it asks something'};
export const SOON_MS=24*3600_000;

const clip=(text,max)=>{const chars=[...text.trim().replace(/\s+/g,' ')];return chars.length<=max?chars.join(''):'…'+chars.slice(-(max-1)).join('');};

/** The last question or blocker sentence of a message, or null. `window` limits
 * how far from the end to look, so an old question in a long reply that later
 * moved on is not reported as the open decision. */
export function finalAsk(text,{window=600}={}) {
  if(typeof text!=='string'||!text.trim())return null;
  const tail=text.slice(-window),q=tail.lastIndexOf('?');
  // Index of the first character of the sentence or line that contains position i.
  const sentenceStart=i=>Math.max(0,tail.lastIndexOf('\n',i)+1,...['. ','! ','? '].map(m=>{const j=tail.lastIndexOf(m,i-1);return j<0?0:j+2;}));
  if(q>=0){
    let start=sentenceStart(q),sentence=tail.slice(start,q+1).trim();
    if([...sentence].length<12&&start>0)sentence=tail.slice(sentenceStart(start-1),q+1).trim();
    if(sentence)return {text:clip(sentence,DECISION_CHARS),source:'question_in_last_reply'};
  }
  const match=BLOCKER.exec(tail);
  if(match){const end=tail.indexOf('\n',match.index);return {text:clip(tail.slice(sentenceStart(match.index),end<0?tail.length:end),DECISION_CHARS),source:'blocker_phrase_in_last_reply'};}
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

/** @param rows annotated inventory rows (status, reason, attention_assessed, title, project_name, updated_at, mirrored, last_user_ms)
 *  @param tails Map<session_id,{text,assistant_last}>: tail of the latest assistant message and whether it is newer than the latest user message */
export function buildDigest({rows,tails,now,counts,statusFresh,freshness,statusObservedAt}) {
  const items=[];
  for(const row of rows) {
    const tail=tails.get(row.session_id)??null;
    let kind=null,ask=null;
    if(row.status==='needs_you') {
      kind=['approval','question','failed'].includes(row.reason)?row.reason:'reply';
      if(kind==='reply'&&tail)ask=finalAsk(tail.text,{window:1500})??endOfReply(tail.text);
    } else if(row.status==='done'&&tail?.assistant_last) {
      ask=finalAsk(tail.text);
      if(ask)kind=row.attention_assessed?'possible_decision':'unconfirmed_ask';
    }
    if(!kind)continue;
    const decision=ask?.text??attentionReason[row.reason]??attentionReason.reply;
    const activity=realMs(row.updated_at);
    items.push({session_id:row.session_id,title:row.title,project:row.project_name??null,kind,urgency:urgencyOf[kind],confirmed:row.status==='needs_you',
      why:kindMeaning[kind],decision,decision_source:ask?.source??'native_prompt',last_activity_ago:activity?ageText(now-activity):null,_activity:activity??0});
  }
  items.sort((a,b)=>kindRank[a.kind]-kindRank[b.kind]||b._activity-a._activity||a.session_id.localeCompare(b.session_id));
  const strip=({_activity,...item})=>item;
  const confirmed=items.filter(i=>i.confirmed).length;
  const statusAge=statusObservedAt?now-statusObservedAt:null,historyAge=freshness?.source_checked_at?now-Number(freshness.source_checked_at):null;
  const caveats=[];
  if(!statusFresh)caveats.push(`Session status is not live${statusAge!==null?` (last observed ${ageText(statusAge)} ago)`:''}; say so before presenting it as current.`);
  if(freshness&&freshness.mac_seen_recently===false)caveats.push('The Mac has not been seen recently, so this may be out of date.');
  const total=items.length;
  const headline=total===0?'No session is waiting on you as far as the last sync shows.'
    :`${total} session${total===1?'':'s'} may need you${confirmed===total?'':`: ${confirmed} confirmed, ${total-confirmed} less certain (marked done or unclassified, but the last reply looks like it asks something)`}.`;
  return {headline,total_attention:total,confirmed,less_certain:total-confirmed,working:counts?.working??0,
    top:items.slice(0,MAX_TOP).map(strip),more_count:Math.max(0,total-MAX_TOP),more:items.slice(MAX_TOP,MAX_TOP+MAX_MORE).map(({session_id,title,kind})=>({session_id,title,kind})),
    data_age:{status:statusAge===null?null:ageText(statusAge),history_checked:historyAge===null?null:ageText(historyAge),status_fresh:Boolean(statusFresh),mac_seen_recently:freshness?.mac_seen_recently??null,caveat:caveats.join(' ')||null},
    presentation:'OVERVIEW MODE. Say the headline, then at most the top 3 items, one short sentence each: session name, the decision it needs, how urgent. Do not list working, finished or "more" sessions unless asked, and do not repeat this list in later turns. Offer to dig into one. When the owner names a session, call session_brief for it instead of answering from this digest. Mention data_age.caveat if present.'};
}

/** Normalised substring match so "diagram bench" finds "diagram-bench". */
export const normalise=s=>String(s??'').toLowerCase().replace(/[^a-z0-9]+/g,' ').trim();
export function matchesQuery(row,query) {
  const hay=normalise(`${row.title??''} ${row.project_name??''} ${row.directory??''}`),tokens=normalise(query).split(' ').filter(Boolean);
  return tokens.length>0&&tokens.every(t=>hay.includes(t));
}
