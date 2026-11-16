import { z } from 'zod';
export const RuntimeSnapshot=z.object({available:z.boolean(),partial:z.boolean(),truncated:z.boolean().default(false),statuses:z.array(z.object({
  session_id:z.string().min(1).max(200),status:z.enum(['working','needs_you','done','scheduled','unknown']),
  reason:z.enum(['question','approval','failed','reply']).nullable(),updated_at:z.number().int().min(0).max(Number.MAX_SAFE_INTEGER),can_message:z.boolean(),last_user_message_id:z.string().max(200).nullable().default(null),
}).strict()).max(1000)}).strict();
