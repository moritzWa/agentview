import {SyncError} from '../sync/projection.mjs';

// Recheck after waits and inside mutations. Authentication at request entry
// alone is insufficient when consent can be revoked while a tool is waiting.
export async function requireGrant(db,principal,{audience='read',scope,lock=false}={}) {
  const row=(await db.query(`SELECT scopes,device_id,database_epoch FROM av_credentials
    WHERE token_hash=$1 AND owner_id=$2 AND audience=$3 AND revoked=false AND expires_at>$4 ${lock?'FOR SHARE':''}`,
    [principal.token_hash,principal.owner_id,audience,Date.now()])).rows[0];
  const ownerGrant=audience==='read'&&row?.device_id==='*'&&row?.database_epoch==='*';
  if(!row || (!ownerGrant && (row.device_id!==principal.device_id||row.database_epoch!==principal.database_epoch)))throw new SyncError('unauthorized',401);
  if(scope&&!row.scopes.split(' ').includes(scope))throw new SyncError(scope==='messages:send'?'write_permission_required':'forbidden',403);
}
