import {test} from 'node:test';
import assert from 'node:assert/strict';
import {contextMessage,MIDDLE_OMITTED} from '../private/excerpt.mjs';
function row(text,budget){const chars=[...text];return {message_id:'msg_fixture',message_order:4,role:'assistant',total_characters:chars.length,text_head:chars.slice(0,budget).join(''),text_tail:chars.slice(-budget).join('')};}
test('short visible messages remain complete',()=>{
 const message=contextMessage(row('Actual short reply.',600),600);
 assert.equal(message.text,'Actual short reply.');assert.equal(message.truncated,false);assert.equal(message.total_characters,19);
});
test('long messages preserve the first and last 20 percent instead of only the ending',()=>{
 const text='A'.repeat(200)+'M'.repeat(600)+'Z'.repeat(200),message=contextMessage(row(text,600),600);
 assert.equal(message.text,'A'.repeat(200)+MIDDLE_OMITTED+'Z'.repeat(200));
 assert.equal(message.head_characters,200);assert.equal(message.tail_characters,200);assert.equal(message.omitted_characters,600);assert.equal(message.truncated,true);
});
test('very long messages preserve both ends within the strict shared budget',()=>{
 const text='TLDR: '+'x'.repeat(20000)+' FINAL QUESTION?',budget=180;
 const message=contextMessage(row(text,budget),budget);
 assert(message.text.startsWith('TLDR: '));assert(message.text.endsWith(' FINAL QUESTION?'));assert.equal([...message.text].length,budget);
 assert.equal(message.head_characters+message.tail_characters+message.omitted_characters,[...text].length);
});
test('Unicode counts and truncation do not split emoji into surrogate halves',()=>{
 const text='😀'.repeat(1000),message=contextMessage(row(text,600),600);
 assert.equal(message.text,'😀'.repeat(200)+MIDDLE_OMITTED+'😀'.repeat(200));assert.equal(message.total_characters,1000);
});
