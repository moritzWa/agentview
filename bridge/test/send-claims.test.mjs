import {test} from 'node:test';
import assert from 'node:assert/strict';
import {grade,claimKind,events} from '../eval/send-claims.mjs';

const T0=1_795_000_000;
function conversation(steps) {
  const mapping={root:{id:'root',parent:null,message:null}};let parent='root',n=0;
  for(const [sec,kind,text,extra] of steps) {
    const id=`n${n++}`,create_time=T0+sec;let message;
    if(kind==='user')message={author:{role:'user'},create_time,recipient:'all',content:{content_type:'multimodal_text',parts:[text]},metadata:{}};
    else if(kind==='say')message={author:{role:'assistant'},create_time,recipient:'all',content:{content_type:'multimodal_text',parts:[text]},metadata:{bidi_voice_mode_message:true,...extra}};
    else if(kind==='call')message={author:{role:'assistant'},create_time,recipient:'api_tool.call_tool',content:{content_type:'code',text:JSON.stringify({path:`/${extra?.app??'AgentVoice'}/link_abc/${text}`,args:{}})},metadata:{}};
    mapping[id]={id,parent,message};parent=id;
  }
  return {conversation_id:'c1',title:'t',mapping,current_node:parent};
}

test('claims are short first-person statements; negations, drafts and long summaries are not claims',()=>{
  assert.equal(claimKind('Okay, sending that.'),'present');
  assert.equal(claimKind("Good. I'll apply that."),'present');
  assert.equal(claimKind('Sending it with your original wording.'),'present');
  assert.equal(claimKind("I've sent it to the session."),'past');
  assert.equal(claimKind('It has not been sent yet.'),null);
  assert.equal(claimKind('Here is the draft I would send.'),null);
  assert.equal(claimKind('Want me to send it?'),null);
  assert.equal(claimKind('x'.repeat(200)+' sending it now.'),null);
});

test('a claim followed by the call within 45 seconds is matched and its lead time is recorded',()=>{
  const g=grade(conversation([[0,'user','Sure, send it'],[1,'say','Okay, sending that.'],[13,'call','send_session_message']]));
  assert.equal(g.present_claims,1);assert.equal(g.unmatched.length,0);assert.deepEqual(g.claim_lead_seconds,[12]);
});

test('a restatement shortly after a call is not a missing call',()=>{
  const g=grade(conversation([[0,'user','Send it'],[2,'call','send_session_message'],[20,'say','Sending that now.']]));
  assert.equal(g.unmatched.length,0);assert.deepEqual(g.claim_lead_seconds,[]);
});

test('the incident shape is flagged: claim in the approval second, no call, owner moves on',()=>{
  const g=grade(conversation([
    [0,'user','Sure. Yeah, sure.'],[0.3,'say','Okay, sending that.',{is_thinking_preamble_message:true}],
    [6,'user','Next one.'],[7,'say',"I'll grab it."],[600,'call','send_session_message']]));
  assert.equal(g.unmatched.length,1);
  const u=g.unmatched[0];
  assert.equal(u.said,'Okay, sending that.');assert.equal(u.preamble,true);assert.equal(u.user_before,'Sure. Yeah, sure.');
  assert.equal(u.next_send_in_s,600);assert.equal(u.next_session_send_in_s,600);
});

test('any send_* tool counts as a send, including email',()=>{
  const g=grade(conversation([[0,'say','Sending now.'],[5,'call','send_email',{app:'Gmail'}]]));
  assert.equal(g.send_calls,1);assert.equal(g.unmatched.length,0);
});

test('read tools never count as sends',()=>{
  const g=grade(conversation([[0,'say','Sending that now.'],[3,'call','read_session']]));
  assert.equal(g.send_calls,0);assert.equal(g.unmatched.length,1);
});

test('only the current branch of the conversation is graded',()=>{
  const c=conversation([[0,'say','Okay, sending that.'],[3,'call','send_session_message']]);
  c.mapping.stale={id:'stale',parent:'root',message:{author:{role:'assistant'},create_time:T0+1,recipient:'all',content:{content_type:'text',parts:['Sending it now.']},metadata:{}}};
  assert.equal(events(c).filter(e=>e.kind==='say').length,1);
});
