export const MIDDLE_OMITTED='\n[… middle omitted …]\n';

/** Rows contain only bounded SQL text slices. Offsets/counts use Unicode
 * characters, matching read_message, not UTF-16 units or UTF-8 bytes. */
export function contextMessage(row,budget) {
 if(!Number.isSafeInteger(budget)||budget<[...MIDDLE_OMITTED].length+2)throw new TypeError('invalid_context_budget');
 const total=Number(row.total_characters);
 const common={message_id:row.message_id,message_order:Number(row.message_order),role:row.role};
 if(total<=budget)return {...common,text:row.text_head,truncated:false,total_characters:total};
 const room=Math.max(0,budget-[...MIDDLE_OMITTED].length);
 const head=Math.min(Math.ceil(total*0.2),Math.floor(room/2));
 const tail=Math.min(Math.floor(total*0.2),room-head);
 const first=[...row.text_head].slice(0,head).join(''),last=tail?[...row.text_tail].slice(-tail).join(''):'';
 return {...common,text:first+MIDDLE_OMITTED+last,truncated:true,total_characters:total,
  head_characters:head,tail_characters:tail,omitted_characters:total-head-tail,
  truncation:'middle_omitted',excerpt_policy:'first_20_percent_and_last_20_percent_capped_by_overview_budget'};
}
